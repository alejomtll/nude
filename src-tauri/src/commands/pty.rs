//! Terminal embebida por carril (`src/pty`): lanzar el agente en el worktree,
//! teclear, redimensionar y cerrar.

use std::path::Path;

use tauri::State;

use super::blocking;
use super::lanes::active_lane;
use crate::db::Database;
use crate::error::NudeResult;
use crate::pty::{LaneProgram, PtyAttach, PtyManager};

/// Abre la terminal del carril en su worktree con el shell de login del
/// usuario y lanza `agent_cmd` (`claude-code`/`claude`, `codex`, `opencode` o
/// `shell`). Si ya hay una sesión viva, se reutiliza y se devuelve su historial.
#[tauri::command]
pub async fn spawn_lane_pty(
    db: State<'_, Database>,
    pty: State<'_, PtyManager>,
    lane_id: String,
    agent_cmd: LaneProgram,
    cols: u16,
    rows: u16,
) -> NudeResult<PtyAttach> {
    let (db, pty) = (db.inner().clone(), pty.inner().clone());
    blocking(move || {
        let lane = active_lane(&db, &lane_id)?;
        pty.spawn_program(&lane.id, Path::new(&lane.worktree_path), agent_cmd, cols, rows)
    })
    .await
}

/// Engancharse a la sesión existente del carril (viva o terminada) sin lanzar
/// nada. `None` si el carril no tiene terminal.
#[tauri::command]
pub async fn attach_lane_pty(pty: State<'_, PtyManager>, lane_id: String) -> NudeResult<Option<PtyAttach>> {
    Ok(pty.attach(&lane_id))
}

#[tauri::command]
pub async fn write_lane_pty(pty: State<'_, PtyManager>, lane_id: String, data: String) -> NudeResult<()> {
    let pty = pty.inner().clone();
    blocking(move || pty.write(&lane_id, &data)).await
}

#[tauri::command]
pub async fn resize_lane_pty(pty: State<'_, PtyManager>, lane_id: String, cols: u16, rows: u16) -> NudeResult<()> {
    pty.resize(&lane_id, cols, rows)
}

/// Cierra el proceso del carril (`SIGHUP` y, si no basta, `SIGKILL`).
#[tauri::command]
pub async fn kill_lane_pty(pty: State<'_, PtyManager>, lane_id: String) -> NudeResult<bool> {
    Ok(pty.kill(&lane_id))
}
