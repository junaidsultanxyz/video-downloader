mod download;
mod paths;
mod probe;
mod state;

use state::AppState;

/// Build and run the Tauri application. `main.rs` is only a launcher; keeping
/// the body here is what allows a mobile entry point to reuse it later.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            probe::probe_url,
            probe::engine_version,
            probe::update_engine,
            download::start_download,
            download::cancel_download,
            download::pause_download,
            download::resume_download,
            paths::default_output_dir,
            paths::reveal_in_folder,
            paths::pick_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sluice");
}
