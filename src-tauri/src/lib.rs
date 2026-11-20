mod commands;
mod db;
mod error;
mod git;

use tauri::Manager;

// Comando de la plantilla; se mantiene mientras el frontend actual lo use.
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // ~/Library/Application Support/com.nude.app/nude.db
            let data_dir = app.path().app_data_dir()?;
            let database = db::Database::open(&data_dir.join(db::DB_FILE_NAME))?;
            app.manage(database);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            commands::projects::add_project,
            commands::projects::list_projects,
            commands::projects::get_project_checkpoints,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
