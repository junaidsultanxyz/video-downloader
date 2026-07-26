use std::collections::HashMap;
use std::sync::Mutex;

use tauri_plugin_shell::process::CommandChild;

/// Shared handle onto every yt-dlp process the app has spawned.
///
/// The map is keyed by the frontend's queue item id, so a specific running
/// job can be looked up and killed on demand (see `cancel_download`). A child
/// is inserted when its job starts and removed when it terminates or is
/// cancelled.
#[derive(Default)]
pub struct AppState {
    pub children: Mutex<HashMap<String, CommandChild>>,

    /// Windows only: the Job Object each yt-dlp process is assigned to, keyed by
    /// the same queue item id. Killing the job tears down the whole process tree
    /// — including yt-dlp's PyInstaller worker child, which parent-PID based
    /// kills do not reliably reach. Stored as the raw `HANDLE` value (`isize`)
    /// because a `HANDLE` pointer is not `Send`.
    #[cfg(windows)]
    pub jobs: Mutex<HashMap<String, isize>>,
}
