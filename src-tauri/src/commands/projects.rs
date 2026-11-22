use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use super::blocking;
use crate::db::Database;
use crate::db::models::{Checkpoint, Project};
use crate::error::{NudeError, NudeResult};
use crate::git;
use crate::watcher::WatcherHandle;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddProjectResponse {
    pub project: Project,
    /// `false` si la ruta ya estaba registrada (se devuelve el existente).
    pub created: bool,
}

/// Registra un repositorio Git existente. Acepta cualquier ruta dentro del
/// repo; se guarda la raíz canónica. Idempotente por `root_path`.
#[tauri::command]
pub async fn add_project(
    db: State<'_, Database>,
    watcher: State<'_, WatcherHandle>,
    path: String,
) -> NudeResult<AddProjectResponse> {
    let db = db.inner().clone();
    let watcher = watcher.inner().clone();
    blocking(move || {
        let info = git::register_repository(&PathBuf::from(path))?;
        let (project, created) = db.upsert_project(&info.name, &info.root_path)?;
        // Idempotente: si ya se vigilaba, el watcher lo ignora.
        watcher.watch(project.clone());
        Ok(AddProjectResponse { project, created })
    })
    .await
}

#[tauri::command]
pub async fn list_projects(db: State<'_, Database>) -> NudeResult<Vec<Project>> {
    let db = db.inner().clone();
    blocking(move || db.list_projects()).await
}

#[tauri::command]
pub async fn remove_project(
    db: State<'_, Database>,
    watcher: State<'_, WatcherHandle>,
    pty: State<'_, crate::pty::PtyManager>,
    project_id: String,
) -> NudeResult<()> {
    let db = db.inner().clone();
    let watcher = watcher.inner().clone();
    let pty = pty.inner().clone();
    blocking(move || {
        super::lanes::forget_lanes_of(&db, &watcher, &pty, &project_id)?;
        watcher.remove_project(&db, project_id)
    })
    .await
}

/// Checkpoints del proyecto, del más reciente al más antiguo.
#[tauri::command]
pub async fn get_project_checkpoints(
    db: State<'_, Database>,
    project_id: String,
) -> NudeResult<Vec<Checkpoint>> {
    let db = db.inner().clone();
    blocking(move || {
        if db.get_project(&project_id)?.is_none() {
            return Err(NudeError::NotFound(format!("proyecto {project_id}")));
        }
        db.list_checkpoints(&project_id)
    })
    .await
}
