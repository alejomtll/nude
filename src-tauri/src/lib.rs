mod commands;
mod db;
mod error;
mod git;
mod safety;
mod watcher;

use tauri::{Emitter, Manager};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let database = db::Database::open_in_app_dir(&data_dir)?;

            // Vigilancia de los proyectos ya registrados + checkpoints automáticos.
            let home = app.path().home_dir()?;
            let projects = database.list_projects()?;
            let handle = app.handle().clone();
            let watcher_handle = watcher::start(database.clone(), home.clone(), projects, move |checkpoint| {
                if let Err(e) = handle.emit(watcher::EVENT_CHECKPOINT_CREATED, checkpoint) {
                    eprintln!("nude: no se pudo emitir checkpoint-created: {e}");
                }
            });

            app.manage(database);
            app.manage(watcher::HomeDir(home));
            app.manage(watcher_handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::projects::add_project,
            commands::projects::list_projects,
            commands::projects::remove_project,
            commands::projects::get_project_checkpoints,
            commands::checkpoints::create_manual_checkpoint,
            commands::checkpoints::get_checkpoint_diffs,
            commands::checkpoints::rollback_checkpoint,
            commands::checkpoints::get_checkpoint_alerts,
            commands::checkpoints::preview_undo_last_agent,
            commands::checkpoints::undo_last_agent,
            commands::git::get_git_graph,
            commands::git::git_create_branch,
            commands::git::git_checkout_branch,
            commands::git::git_quick_commit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
