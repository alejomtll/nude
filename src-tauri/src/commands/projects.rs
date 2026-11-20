use std::path::PathBuf;

use serde::Serialize;
use tauri::State;

use super::blocking;
use crate::db::Database;
use crate::db::models::{Checkpoint, Project};
use crate::error::{NudeError, NudeResult};
use crate::git;

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
pub async fn add_project(db: State<'_, Database>, path: String) -> NudeResult<AddProjectResponse> {
    let db = db.inner().clone();
    blocking(move || {
        let info = git::register_repository(&PathBuf::from(path))?;
        let (project, created) = db.upsert_project(&info.name, &info.root_path)?;
        Ok(AddProjectResponse { project, created })
    })
    .await
}

#[tauri::command]
pub async fn list_projects(db: State<'_, Database>) -> NudeResult<Vec<Project>> {
    let db = db.inner().clone();
    blocking(move || db.list_projects()).await
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
