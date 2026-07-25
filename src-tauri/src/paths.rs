use std::path::{Path, PathBuf};

use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

/// The folder new downloads default to: the OS "Videos" directory, falling
/// back to "Downloads", then the home directory. Returned as a string because
/// it crosses the IPC boundary to the frontend.
#[tauri::command]
pub fn default_output_dir(app: tauri::AppHandle) -> Result<String, String> {
    let path = app
        .path()
        .video_dir()
        .or_else(|_| app.path().download_dir())
        .or_else(|_| app.path().home_dir())
        .map_err(|e| format!("Couldn't resolve a default download folder: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// Show a finished download in the system file manager. When the file still
/// exists we highlight it directly; otherwise we open its parent folder so the
/// action never dead-ends on a moved or cleaned-up file.
#[tauri::command]
pub fn reveal_in_folder(app: tauri::AppHandle, path: String) -> Result<(), String> {
    let target = PathBuf::from(&path);
    if target.exists() {
        app.opener()
            .reveal_item_in_dir(&target)
            .map_err(|e| e.to_string())
    } else if let Some(parent) = target.parent() {
        app.opener()
            .open_path(parent.to_string_lossy(), None::<&str>)
            .map_err(|e| e.to_string())
    } else {
        Err("That file no longer exists.".into())
    }
}

/// Open the native folder picker and return the chosen directory, or `None` if
/// the person cancelled. This is a synchronous command so it runs on its own
/// thread; `blocking_pick_folder` dispatches the dialog to the UI thread and
/// waits, which is safe as long as it isn't called from that thread itself.
#[tauri::command]
pub fn pick_folder(app: tauri::AppHandle, start_dir: Option<String>) -> Option<String> {
    let mut builder = app.dialog().file();
    if let Some(dir) = start_dir.filter(|d| !d.is_empty()) {
        builder = builder.set_directory(dir);
    }
    builder
        .blocking_pick_folder()
        .map(|path| path.to_string())
}

/// Locate the directory that holds the bundled ffmpeg sidecar so yt-dlp can be
/// pointed at it with `--ffmpeg-location`. Returns `None` when nothing is
/// found, in which case yt-dlp falls back to whatever ffmpeg is on `PATH`.
///
/// The search covers the layouts the binary can actually run from:
///   1. the directory of the running executable (installed / bundled),
///   2. a `binaries/` subfolder beneath it,
///   3. `../../binaries` — where `tauri dev` leaves them relative to
///      `target/debug`,
///   4. a plain relative `binaries/` for good measure.
pub fn bundled_ffmpeg_dir() -> Option<String> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
            candidates.push(dir.join("binaries"));
            candidates.push(dir.join("../../binaries"));
        }
    }
    candidates.push(PathBuf::from("binaries"));

    candidates
        .into_iter()
        .find(|dir| dir_has_ffmpeg(dir))
        .map(|dir| dir.to_string_lossy().into_owned())
}

/// True when `dir` contains an executable named `ffmpeg`, `ffmpeg.exe`, or any
/// `ffmpeg-*` triple-suffixed variant that Tauri produces for sidecars.
fn dir_has_ffmpeg(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stem = name.strip_suffix(".exe").unwrap_or(&name);
        stem == "ffmpeg" || stem.starts_with("ffmpeg-")
    })
}
