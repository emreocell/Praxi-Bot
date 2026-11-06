//! Desktop shell library entry point for Tauri.

/// Builds and runs the Tauri desktop application.
///
/// # Panics
///
/// Panics if the Tauri application fails to initialize or run.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
