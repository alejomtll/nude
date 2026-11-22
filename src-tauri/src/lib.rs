mod commands;
mod db;
mod error;
mod git;
mod preview;
mod safety;
mod watcher;

use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager};

const MENU_COMMAND_PALETTE: &str = "open-command-palette";
/// Evento hacia el frontend cuando se usa el acelerador nativo de ⌘K.
const EVENT_OPEN_COMMAND_PALETTE: &str = "open-command-palette";
use tauri_plugin_notification::NotificationExt;

/// Notificación nativa de macOS cuando un agente toca una ruta protegida.
fn notify_guardrail(app: &AppHandle, alert: &watcher::GuardrailAlert) {
    let agent = match alert.agent_tool.as_str() {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "opencode" => "OpenCode",
        other => other,
    };
    let reverted = alert.violations.iter().filter(|v| v.reverted).count();
    let files: Vec<&str> = alert.violations.iter().map(|v| v.file_path.as_str()).collect();
    let mut listed = files.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    if files.len() > 3 {
        listed.push_str(&format!(" y {} más", files.len() - 3));
    }
    let outcome = match (reverted, files.len()) {
        (0, _) => "Revisa el cambio.".to_owned(),
        (r, n) if r == n => "Restaurado al último snapshot.".to_owned(),
        (r, n) => format!("{r} de {n} restaurados al último snapshot."),
    };
    let result = app
        .notification()
        .builder()
        .title(format!("{agent} tocó un archivo protegido · {}", alert.project_name))
        .body(format!("{listed}. {outcome}"))
        .show();
    if let Err(e) = result {
        eprintln!("nude: no se pudo mostrar la notificación: {e}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Menú nativo = el de macOS por defecto + "Ir › Paleta de comandos ⌘K".
        // El acelerador garantiza que ⌘K llegue aunque WKWebView no entregue la
        // tecla a la página; el frontend descarta el duplicado si llegan ambos.
        .menu(|handle| {
            let menu = Menu::default(handle)?;
            let palette = MenuItem::with_id(
                handle,
                MENU_COMMAND_PALETTE,
                "Paleta de comandos…",
                true,
                Some("CmdOrCtrl+K"),
            )?;
            menu.append(&Submenu::with_items(handle, "Ir", true, &[&palette])?)?;
            Ok(menu)
        })
        .on_menu_event(|app, event| {
            if event.id() == MENU_COMMAND_PALETTE {
                if let Err(e) = app.emit(EVENT_OPEN_COMMAND_PALETTE, ()) {
                    eprintln!("nude: no se pudo emitir {EVENT_OPEN_COMMAND_PALETTE}: {e}");
                }
            }
        })
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let database = db::Database::open_in_app_dir(&data_dir)?;

            if let Err(error) = database.prune_previews() {
                eprintln!("nude: no se pudieron limpiar las capturas huérfanas: {error}");
            }
            app.manage(preview::PreviewService::new(database.clone()));

            // Vigilancia de los proyectos ya registrados + checkpoints automáticos.
            let home = app.path().home_dir()?;
            let projects = database.list_watch_targets()?;
            let handle = app.handle().clone();
            let watcher_handle = watcher::start(database.clone(), home.clone(), projects, move |event| match event {
                watcher::WatcherEvent::Checkpoint(checkpoint) => {
                    commands::checkpoints::emit_checkpoint(&handle, checkpoint);
                }
                watcher::WatcherEvent::Guardrail(alert) => {
                    if let Err(e) = handle.emit(watcher::EVENT_GUARDRAIL_TRIGGERED, alert) {
                        eprintln!("nude: no se pudo emitir guardrail-triggered: {e}");
                    }
                    notify_guardrail(&handle, alert);
                }
                watcher::WatcherEvent::Status(status) => {
                    if let Err(e) = handle.emit(watcher::EVENT_WATCHER_STATUS, status) {
                        eprintln!("nude: no se pudo emitir watcher-status: {e}");
                    }
                }
            });

            app.manage(database);
            app.manage(watcher::HomeDir(home));
            app.manage(commands::lanes::LaneRoot(data_dir.join("worktrees")));
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
            commands::guardrails::list_guardrails,
            commands::guardrails::set_guardrail,
            commands::guardrails::remove_guardrail,
            commands::editor::list_editors,
            commands::editor::open_in_editor,
            commands::watcher::get_watcher_diagnostics,
            commands::watcher::get_watcher_status,
            commands::watcher::force_scan,
            commands::preview::set_project_preview_url,
            commands::preview::capture_checkpoint_preview,
            commands::preview::get_checkpoint_preview,
            commands::lanes::create_parallel_lane,
            commands::lanes::list_parallel_lanes,
            commands::lanes::get_lane_diff,
            commands::lanes::merge_parallel_lane,
            commands::lanes::discard_parallel_lane,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
