use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};
use tauri_plugin_shell::process::CommandEvent;
use tauri_plugin_shell::ShellExt;

use crate::paths::bundled_ffmpeg_dir;
use crate::state::AppState;

/// A single download job as requested by the frontend queue.
#[derive(Deserialize)]
pub struct DownloadRequest {
    pub id: String,
    pub url: String,
    /// "vbest", "v1080" (any `v<height>`), or "audio:mp3" | "audio:m4a" | "audio:opus".
    pub quality: String,
    pub out_dir: String,
}

/// Progress tick, emitted several times a second while a job runs.
#[derive(Serialize, Clone)]
struct Progress {
    id: String,
    percent: f64,
    speed: String,
    eta: String,
    size: String,
    stage: String,
}

/// Terminal outcome for a job, emitted exactly once.
#[derive(Serialize, Clone)]
struct Done {
    id: String,
    ok: bool,
    path: Option<String>,
    error: Option<String>,
}

/// Emitted the first time a job writes to a given path. The frontend persists
/// these so an interrupted download's partial files can be cleaned up later, or
/// simply resumed in place.
#[derive(Serialize, Clone)]
struct FileEvent {
    id: String,
    path: String,
}

/// Sentinel that marks a parseable progress line. The `download:` prefix in the
/// template is consumed by yt-dlp as the event type; `@@P|` is ours.
const PROGRESS_SENTINEL: &str = "@@P|";
/// Keep only the tail of stderr — enough to explain a failure, bounded so a
/// noisy job can't grow the buffer without limit.
const STDERR_CAP: usize = 4096;

/// Start a download. Spawns the yt-dlp sidecar, stores its handle in shared
/// state (so it can be cancelled), and streams its output into `dl:progress`
/// and a final `dl:done` event. Returns once the process has terminated.
#[tauri::command]
pub async fn start_download(
    app: tauri::AppHandle,
    request: DownloadRequest,
) -> Result<(), String> {
    let args = build_args(&request);

    let (mut rx, child) = app
        .shell()
        .sidecar("yt-dlp")
        .map_err(|e| format!("Couldn't start the download engine: {e}"))?
        .args(args)
        .spawn()
        .map_err(|e| format!("The download engine didn't start: {e}"))?;

    // On Windows, capture the pid before the child moves into shared state so we
    // can put the process in a Job Object below.
    #[cfg(windows)]
    let job_pid = child.pid();

    // Register the child so `cancel_download` can find and kill it. The lock is
    // released immediately — it must never be held across an `.await`.
    {
        let state = app.state::<AppState>();
        let mut children = state.children.lock().unwrap();
        children.insert(request.id.clone(), child);
    }

    // Windows: assign the process to a Job Object right away. Any child it later
    // spawns (the PyInstaller worker, and ffmpeg during a merge) is created into
    // the same job, so cancelling can kill the entire tree at once — something
    // parent-PID based kills miss, leaving the worker downloading to the file we
    // are trying to delete. We do this immediately after spawn, before the
    // bootloader finishes unpacking and launches its worker, so nothing escapes.
    #[cfg(windows)]
    {
        if let Some(job) = create_job_for(job_pid) {
            let state = app.state::<AppState>();
            state.jobs.lock().unwrap().insert(request.id.clone(), job);
        }
    }

    let mut tracker = ProgressTracker::default();
    let mut stderr_tail = String::new();
    let mut final_path: Option<String> = None;
    // Every path yt-dlp writes to (streams, merge target, extracted audio), so a
    // cancelled job can delete the half-written files it leaves behind.
    let mut outputs: Vec<String> = Vec::new();

    while let Some(event) = rx.recv().await {
        match event {
            CommandEvent::Stdout(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                let line = line.trim_end();
                handle_stdout_line(
                    &app,
                    &request.id,
                    line,
                    &mut tracker,
                    &mut final_path,
                    &mut outputs,
                );
            }
            CommandEvent::Stderr(bytes) => {
                append_bounded(&mut stderr_tail, &String::from_utf8_lossy(&bytes));
            }
            CommandEvent::Terminated(payload) => {
                // Remove the child; if it's already gone the job was cancelled.
                let was_present = {
                    let state = app.state::<AppState>();
                    let mut children = state.children.lock().unwrap();
                    children.remove(&request.id).is_some()
                };

                // Release this job's Job Object. On a normal finish the process
                // has already exited, so this just closes the handle; on cancel
                // it was already taken and killed by `cancel_download`.
                #[cfg(windows)]
                {
                    let state = app.state::<AppState>();
                    let job = state.jobs.lock().unwrap().remove(&request.id);
                    if let Some(job) = job {
                        close_job(job);
                    }
                }

                let ok = payload.code == Some(0);
                let done = if !was_present {
                    // Cancelled jobs surface as cancelled, not as an error — and
                    // leave nothing behind: SIGKILL denies yt-dlp its own cleanup,
                    // so we remove the partial stream/fragment files ourselves.
                    cleanup_partials(&request.out_dir, &outputs);
                    Done { id: request.id.clone(), ok: false, path: None, error: None }
                } else if ok {
                    Done {
                        id: request.id.clone(),
                        ok: true,
                        path: final_path.clone(),
                        error: None,
                    }
                } else {
                    Done {
                        id: request.id.clone(),
                        ok: false,
                        path: None,
                        error: Some(friendly_error(&stderr_tail)),
                    }
                };
                let _ = app.emit("dl:done", done);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Kill a running job. Removing the child from state before killing it is what
/// makes the terminated event read as "cancelled" rather than "failed".
///
/// We kill the whole process tree, not just the process we spawned: yt-dlp is a
/// PyInstaller one-file binary whose bootloader forks a worker child, and
/// killing only the bootloader leaves that worker downloading to completion.
/// On Windows the reliable way to reach that worker is the Job Object the
/// process was assigned to at startup; the process-tree kill is a fallback.
#[tauri::command]
pub async fn cancel_download(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<(), String> {
    let child = {
        let mut children = state.children.lock().unwrap();
        children.remove(&id)
    };
    // Take this job's Job Object so we can terminate the whole tree at once.
    #[cfg(windows)]
    let job = state.jobs.lock().unwrap().remove(&id);

    if let Some(child) = child {
        let pid = child.pid();
        // The kill (and, on Windows, the wait for every process to exit so its
        // file handles are freed before we delete the partial file) must not run
        // on the main thread, so do it on a blocking worker.
        tauri::async_runtime::spawn_blocking(move || {
            // Windows: terminate the Job Object first — this is what reaches the
            // PyInstaller worker child that keeps the partial file open. The
            // tree-terminate below is a fallback for anything not in the job.
            #[cfg(windows)]
            if let Some(job) = job {
                kill_job(job);
            }
            kill_tree(pid);
            // Fallback for the bootloader itself; ignore "already dead".
            let _ = child.kill();
        })
        .await
        .map_err(|e| e.to_string())?;
    } else {
        // The child already terminated on its own, but close any lingering job.
        #[cfg(windows)]
        if let Some(job) = job {
            close_job(job);
        }
    }
    Ok(())
}

/// Pause a running job by suspending its whole process tree (SIGSTOP). The
/// process stays alive, so it holds its place and resumes exactly where it left
/// off — no bytes are re-downloaded.
#[tauri::command]
pub fn pause_download(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    let pid = state.children.lock().unwrap().get(&id).map(|c| c.pid());
    if let Some(pid) = pid {
        stop_tree(pid);
    }
    Ok(())
}

/// Resume a paused job (SIGCONT) so its download continues.
#[tauri::command]
pub fn resume_download(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    let pid = state.children.lock().unwrap().get(&id).map(|c| c.pid());
    if let Some(pid) = pid {
        cont_tree(pid);
    }
    Ok(())
}

/// Delete the partial files left by a job the user removes without finishing —
/// e.g. an interrupted download that will never be resumed. Runs off the main
/// thread because a fragmented job can leave hundreds of segment files behind.
#[tauri::command]
pub async fn discard_download(out_dir: String, outputs: Vec<String>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || cleanup_partials(&out_dir, &outputs))
        .await
        .map_err(|e| e.to_string())
}

/// Tracks progress across a job's stages so the fill never jumps backwards when
/// yt-dlp moves from the video stream to the audio stream.
#[derive(Default)]
struct ProgressTracker {
    /// How many "[download] Destination:" lines we've seen. 1 = video, 2 = audio.
    destinations: u32,
    stage: String,
    last_percent: f64,
}

impl ProgressTracker {
    /// Clamp percent to be monotonic within the current stage.
    fn monotonic(&mut self, percent: f64) -> f64 {
        if percent >= self.last_percent {
            self.last_percent = percent;
        }
        self.last_percent
    }

    /// Reset the percentage floor when a new stage begins.
    fn enter_stage(&mut self, stage: &str) {
        self.stage = stage.to_string();
        self.last_percent = 0.0;
    }
}

/// Interpret one line of yt-dlp stdout, updating stage/progress state and
/// emitting a `dl:progress` event when the line carries a progress reading.
fn handle_stdout_line(
    app: &tauri::AppHandle,
    id: &str,
    line: &str,
    tracker: &mut ProgressTracker,
    final_path: &mut Option<String>,
    outputs: &mut Vec<String>,
) {
    // Record a path as both the current final path and a cleanup candidate, and
    // tell the frontend the first time we see it so it can be resumed or cleaned.
    let mut record = |path: String| {
        if !outputs.contains(&path) {
            outputs.push(path.clone());
            let _ = app.emit(
                "dl:file",
                FileEvent { id: id.to_string(), path: path.clone() },
            );
        }
        *final_path = Some(path);
    };

    if let Some(rest) = line.strip_prefix(PROGRESS_SENTINEL) {
        // Fields: percent | speed | eta | total_bytes | total_bytes_estimate
        let fields: Vec<&str> = rest.split('|').collect();
        if fields.len() < 5 {
            return;
        }
        let percent = fields[0]
            .trim()
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or(0.0);
        let percent = tracker.monotonic(percent);
        let speed = fields[1].trim().to_string();
        let eta = fields[2].trim().to_string();
        // Prefer the exact total; fall back to the estimate when it's unknown.
        let size = if fields[3].trim() != "N/A" {
            fields[3].trim().to_string()
        } else {
            fields[4].trim().to_string()
        };

        let stage = if tracker.stage.is_empty() {
            "Downloading".to_string()
        } else {
            tracker.stage.clone()
        };

        let _ = app.emit(
            "dl:progress",
            Progress { id: id.to_string(), percent, speed, eta, size, stage },
        );
        return;
    }

    if let Some(path) = line.strip_prefix("[download] Destination: ") {
        tracker.destinations += 1;
        // First stream is video, a second within the same job is the audio track.
        if tracker.destinations >= 2 {
            tracker.enter_stage("Downloading audio");
        } else {
            tracker.enter_stage("Downloading");
        }
        record(path.trim().to_string());
        return;
    }

    if line.contains("has already been downloaded") {
        // e.g. "[download] /path/to/file has already been downloaded"
        if let Some(rest) = line.strip_prefix("[download] ") {
            if let Some(path) = rest.split(" has already been downloaded").next() {
                record(path.trim().to_string());
            }
        }
        return;
    }

    if line.starts_with("[Merger]") {
        tracker.enter_stage("Merging");
        // "[Merger] Merging formats into \"/path/to/file.mp4\""
        if let Some(path) = between_quotes(line) {
            record(path);
        }
        return;
    }

    if let Some(path) = line.strip_prefix("[ExtractAudio] Destination: ") {
        tracker.enter_stage("Converting");
        record(path.trim().to_string());
        return;
    }

    if line.starts_with("[EmbedThumbnail]") || line.starts_with("[Metadata]") {
        tracker.enter_stage("Finishing");
    }
}

/// Assemble the full yt-dlp argument list for a request.
fn build_args(request: &DownloadRequest) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--no-playlist".into(),
        "--no-colors".into(),
        "--newline".into(),
        "--no-warnings".into(),
        "--ignore-config".into(),
        // Resume a partially downloaded file instead of restarting it — this is
        // what lets an interrupted job continue where it stopped. It is yt-dlp's
        // default, but we set it explicitly so a resume is never a re-download.
        "--continue".into(),
        "-N".into(),
        "4".into(),
        "--retries".into(),
        "5".into(),
        "--fragment-retries".into(),
        "10".into(),
        "--progress-template".into(),
        "download:@@P|%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(progress._total_bytes_str)s|%(progress._total_bytes_estimate_str)s".into(),
        "-P".into(),
        request.out_dir.clone(),
        "-o".into(),
        "%(title).180B [%(id)s].%(ext)s".into(),
    ];

    // Point yt-dlp at our bundled ffmpeg when we can find it; otherwise it will
    // look on PATH. Missing ffmpeg is what silently breaks merged downloads.
    if let Some(dir) = bundled_ffmpeg_dir() {
        args.push("--ffmpeg-location".into());
        args.push(dir);
    }

    if let Some(format) = request.quality.strip_prefix("audio:") {
        args.extend([
            "-f".into(),
            "ba/b".into(),
            "-x".into(),
            "--audio-format".into(),
            format.to_string(),
            "--audio-quality".into(),
            "0".into(),
            "--embed-thumbnail".into(),
            "--embed-metadata".into(),
        ]);
    } else {
        let selector = video_selector(&request.quality);
        args.extend([
            "-f".into(),
            selector,
            "--merge-output-format".into(),
            "mp4".into(),
            "--embed-metadata".into(),
        ]);
    }

    args.push(request.url.clone());
    args
}

/// Build the format selector for a video quality token. The triple fallback is
/// deliberate: best video + best audio, else a pre-muxed stream at that height,
/// else whatever exists — so muxed-only sites still work.
fn video_selector(quality: &str) -> String {
    if quality == "vbest" {
        return "bv*+ba/b".into();
    }
    match quality.strip_prefix('v').and_then(|h| h.parse::<u32>().ok()) {
        Some(height) => {
            format!("bv*[height<={height}]+ba/b[height<={height}]/b")
        }
        None => "bv*+ba/b".into(),
    }
}

/// Extract the substring between the first pair of double quotes, if any.
fn between_quotes(line: &str) -> Option<String> {
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// Append `chunk` to `buffer`, keeping only the last `STDERR_CAP` bytes.
fn append_bounded(buffer: &mut String, chunk: &str) {
    buffer.push_str(chunk);
    if buffer.len() > STDERR_CAP {
        // Trim from the front, snapping to a char boundary so we never split a
        // multi-byte character.
        let mut cut = buffer.len() - STDERR_CAP;
        while cut < buffer.len() && !buffer.is_char_boundary(cut) {
            cut += 1;
        }
        *buffer = buffer[cut..].to_string();
    }
}

/// Delete the half-written files a cancelled job leaves behind. For each path
/// yt-dlp reported writing to, remove that file and every sibling whose name
/// begins with it — covering `.part`, `.part-FragN`, and `.ytdl` temp files —
/// while leaving other jobs' downloads in the same folder untouched.
fn cleanup_partials(out_dir: &str, outputs: &[String]) {
    use std::path::Path;
    for output in outputs {
        let path = Path::new(output);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(out_dir).join(path)
        };
        let (Some(dir), Some(name)) = (resolved.parent(), resolved.file_name()) else {
            continue;
        };
        let name = name.to_string_lossy();
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.file_name().to_string_lossy().starts_with(name.as_ref()) {
                remove_file_retrying(&entry.path());
            }
        }
    }
}

/// Delete a file, retrying briefly on Windows. A process killed a moment ago can
/// still hold its file handle for a short while after it exits, and Windows
/// refuses to delete an open file; a few short retries clear that window. On
/// other platforms the first attempt is authoritative.
fn remove_file_retrying(path: &std::path::Path) {
    for attempt in 0..10u32 {
        match std::fs::remove_file(path) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) if cfg!(windows) => {
                std::thread::sleep(std::time::Duration::from_millis(50 * u64::from(attempt + 1)));
            }
            Err(_) => return,
        }
    }
}

// ---- Process-tree signalling -----------------------------------------------
//
// yt-dlp is a PyInstaller one-file binary: the process we spawn is a bootloader
// that forks a worker child. Cancel/pause/resume must therefore reach the whole
// tree, not just the bootloader.

/// Send `signal` to `root` and every process descended from it, leaves first.
#[cfg(target_os = "linux")]
fn signal_tree(root: u32, signal: i32) {
    for pid in process_tree(root).into_iter().rev() {
        // SAFETY: kill() with a valid signal is always sound; a stale pid simply
        // returns ESRCH, which we ignore.
        unsafe { libc::kill(pid as libc::pid_t, signal) };
    }
}

/// Freeze the whole tree with SIGSTOP, re-scanning until it stops growing.
///
/// This closes a race: a running yt-dlp can fork a new worker between our /proc
/// snapshot and the signal, and if we killed a parent first the orphan's parent
/// link would change to init and we'd lose it. A *stopped* process can neither
/// fork nor exit and keeps its parent, so re-scanning converges on the full
/// tree with every process held in place.
#[cfg(target_os = "linux")]
fn freeze_tree(root: u32) {
    let mut previous = 0;
    for _ in 0..5 {
        let tree = process_tree(root);
        for &pid in &tree {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) };
        }
        if tree.len() == previous {
            break;
        }
        previous = tree.len();
    }
}

/// The pids of `root` and all its descendants, parents before children, read
/// from a single snapshot of /proc.
#[cfg(target_os = "linux")]
fn process_tree(root: u32) -> Vec<u32> {
    use std::collections::HashMap;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() {
                if let Some(ppid) = parent_pid(pid) {
                    children.entry(ppid).or_default().push(pid);
                }
            }
        }
    }
    let mut order = vec![root];
    let mut i = 0;
    while i < order.len() {
        if let Some(kids) = children.get(&order[i]) {
            order.extend(kids);
        }
        i += 1;
    }
    order
}

/// Read a process's parent pid from /proc/<pid>/stat. The `comm` field can hold
/// spaces and parentheses, so read the fields after the final ')'.
#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = stat.rsplit_once(')')?.1;
    let mut fields = tail.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(target_os = "linux")]
fn kill_tree(pid: u32) {
    // Freeze first so nothing can fork away, then kill leaves first. SIGKILL is
    // delivered to stopped processes too, so no SIGCONT is needed.
    freeze_tree(pid);
    signal_tree(pid, libc::SIGKILL);
}
#[cfg(target_os = "linux")]
fn stop_tree(pid: u32) {
    freeze_tree(pid);
}
#[cfg(target_os = "linux")]
fn cont_tree(pid: u32) {
    // Every process in the (fully present, still-linked) frozen tree gets resumed.
    signal_tree(pid, libc::SIGCONT);
}

// Windows has no signals. Cancel is driven by the Job Object each process is
// assigned to at startup (see `create_job_for`); killing the job reaches yt-dlp's
// PyInstaller worker child reliably, which parent-PID walking does not. The
// process-tree terminate below is a fallback. Pause/resume suspend and resume
// every thread of every process in the tree.

/// Create a Job Object, assign the process `pid` to it, and return the job's raw
/// `HANDLE` as an `isize`. Any process the assigned process later spawns joins
/// the same job automatically, so terminating the job kills the whole tree.
/// Returns `None` if the job could not be created or assigned (cancel then falls
/// back to the process-tree terminate).
#[cfg(windows)]
fn create_job_for(pid: u32) -> Option<isize> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        // Kill everything left in the job when its last handle closes, so a job
        // that outlives a crash or a missed cleanup can't leak live processes.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );

        let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if proc.is_null() {
            CloseHandle(job);
            return None;
        }
        let assigned = AssignProcessToJobObject(job, proc) != 0;
        CloseHandle(proc);
        if !assigned {
            CloseHandle(job);
            return None;
        }
        Some(job as isize)
    }
}

/// Terminate every process in a Job Object and close the handle.
#[cfg(windows)]
fn kill_job(job: isize) {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;
    unsafe {
        let handle = job as HANDLE;
        TerminateJobObject(handle, 1);
        CloseHandle(handle);
    }
}

/// Close a Job Object handle without an explicit terminate — used when the
/// process has already exited on its own. With kill-on-close set, this still
/// reaps anything unexpectedly left in the job.
#[cfg(windows)]
fn close_job(job: isize) {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    unsafe {
        CloseHandle(job as HANDLE);
    }
}

#[cfg(windows)]
fn kill_tree(root: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    // Leaves first, so a worker can't be orphaned and outlive its parent.
    for pid in process_tree(root).into_iter().rev() {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid);
            if handle.is_null() {
                continue;
            }
            TerminateProcess(handle, 1);
            // Wait for the process to finish exiting so Windows releases the file
            // handles it held; then the partial file can be deleted right away
            // instead of being found locked. Bounded so a stuck process can't
            // hang the cancel forever.
            WaitForSingleObject(handle, 5000);
            CloseHandle(handle);
        }
    }
}

#[cfg(windows)]
fn stop_tree(pid: u32) {
    // Two passes catch a thread or child that appears mid-suspend; resume drains
    // the suspend count, so a thread suspended twice here still wakes fully.
    for _ in 0..2 {
        for_each_thread(&process_tree(pid), |tid| adjust_thread(tid, true));
    }
}

#[cfg(windows)]
fn cont_tree(pid: u32) {
    for_each_thread(&process_tree(pid), |tid| adjust_thread(tid, false));
}

/// `root` and all its descendants, read from one ToolHelp process snapshot.
#[cfg(windows)]
fn process_tree(root: u32) -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };

    let mut links: Vec<(u32, u32)> = Vec::new(); // (pid, parent pid)
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return vec![root];
        }
        let mut entry: PROCESSENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snap, &mut entry) != 0 {
            loop {
                links.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32Next(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }

    let mut order = vec![root];
    let mut i = 0;
    while i < order.len() {
        let parent = order[i];
        for &(pid, ppid) in &links {
            if ppid == parent && pid != parent && !order.contains(&pid) {
                order.push(pid);
            }
        }
        i += 1;
    }
    order
}

/// Call `f` with the thread id of every thread owned by one of `pids`.
#[cfg(windows)]
fn for_each_thread(pids: &[u32], mut f: impl FnMut(u32)) {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return;
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        if Thread32First(snap, &mut entry) != 0 {
            loop {
                if pids.contains(&entry.th32OwnerProcessID) {
                    f(entry.th32ThreadID);
                }
                if Thread32Next(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
}

/// Suspend a thread once, or resume it fully (draining any repeated suspends).
#[cfg(windows)]
fn adjust_thread(tid: u32, suspend: bool) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
    };

    unsafe {
        let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
        if handle.is_null() {
            return;
        }
        if suspend {
            SuspendThread(handle);
        } else {
            // ResumeThread returns the previous suspend count; keep going until
            // the thread is actually running (count was 1 → now 0), or it fails.
            loop {
                let previous = ResumeThread(handle);
                if previous == u32::MAX || previous <= 1 {
                    break;
                }
            }
        }
        CloseHandle(handle);
    }
}

/// Reduce accumulated stderr to a single readable failure line, with a hint
/// toward the "Update engine" button when the cause looks like a stale extractor.
fn friendly_error(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .unwrap_or_else(|| stderr.lines().last().unwrap_or(""))
        .trim()
        .trim_start_matches("ERROR:")
        .trim()
        .to_string();

    let lowered = line.to_lowercase();
    if lowered.contains("unsupported url") || lowered.contains("unable to extract") {
        return format!(
            "{line}\n\nThe site may have changed — try \"Update engine\" and download again."
        );
    }
    if line.is_empty() {
        return "The download failed. Check the post is public, then try again.".into();
    }
    line
}
