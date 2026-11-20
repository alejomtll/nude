//! Comandos IPC expuestos al frontend (`invoke("<nombre>", {...})`).
//!
//! Todos son `async` y delegan el trabajo bloqueante (SQLite, libgit2) a
//! `spawn_blocking` para no congelar el hilo principal de la UI.

// `generate_handler!` necesita la ruta real del módulo (no un re-export).
pub mod checkpoints;
pub mod projects;

use crate::error::{NudeError, NudeResult};

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
