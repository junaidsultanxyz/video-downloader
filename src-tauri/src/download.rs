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

    // Register the child so `cancel_download` can find and kill it. The lock is
    // released immediately — it must never be held across an `.await`.
    {
        let state = app.state::<AppState>();
        let mut children = state.children.lock().unwrap();
        children.insert(request.id.clone(), child);
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
#[tauri::command]
pub fn cancel_download(state: tauri::State<AppState>, id: String) -> Result<(), String> {
    let child = {
        let mut children = state.children.lock().unwrap();
        children.remove(&id)
    };
    if let Some(child) = child {
        kill_tree(child.pid());
        // Fallback for the bootloader itself; ignore "already dead".
        let _ = child.kill();
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
    // Record a path as both the current final path and a cleanup candidate.
    let mut record = |path: String| {
        if !outputs.contains(&path) {
            outputs.push(path.clone());
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
                let _ = std::fs::remove_file(entry.path());
            }
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

// On Windows, taskkill terminates the whole tree. Suspend/resume aren't wired up
// there yet, so they're no-ops.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .output();
}
#[cfg(windows)]
fn stop_tree(_pid: u32) {}
#[cfg(windows)]
fn cont_tree(_pid: u32) {}

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
