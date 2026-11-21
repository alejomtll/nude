//! Comandos IPC expuestos al frontend (`invoke("<nombre>", {...})`).
//!
//! Todos son `async` y delegan el trabajo bloqueante (SQLite, libgit2) a
//! `spawn_blocking` para no congelar el hilo principal de la UI.

// `generate_handler!` necesita la ruta real del módulo (no un re-export).
pub mod checkpoints;
pub mod editor;
pub mod git;
pub mod guardrails;
pub mod projects;
pub mod watcher;

use crate::db::Database;
use crate::db::models::Project;
use crate::error::{NudeError, NudeResult};

fn project_or_404(db: &Database, project_id: &str) -> NudeResult<Project> {
    db.get_project(project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))
}

/// Ejecuta `f` en el pool bloqueante de Tauri y aplana el error de join.
async fn blocking<T, F>(f: F) -> NudeResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> NudeResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| NudeError::Task(e.to_string()))?
}
