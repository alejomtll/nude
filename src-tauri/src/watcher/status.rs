//! Estado en vivo del watcher por proyecto, compartido entre su hilo y los
//! comandos IPC (diagnóstico del indicador "en vivo").

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::db::models::now_rfc3339;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum WatchState {
    /// Aún no se ha intentado (el hilo está arrancando).
    Starting,
    /// `notify` vigila la carpeta y la última revisión funcionó.
    Active,
    /// No se puede vigilar o la última revisión falló (`error` lo explica).
    Error,
    /// El proyecto no está bajo vigilancia (p. ej. se quitó).
    Stopped,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WatchStatus {
    pub project_id: String,
    pub state: WatchState,
    /// Mensaje de error exacto (vigilancia o última revisión).
    pub error: Option<String>,
    pub watching_since: Option<String>,
    /// Último evento del sistema de archivos dentro del proyecto.
    pub last_event_at: Option<String>,
    /// Última comparación del árbol de trabajo con el último checkpoint.
    pub last_scan_at: Option<String>,
    pub last_checkpoint_at: Option<String>,
}

impl WatchStatus {
    pub fn new(project_id: &str, state: WatchState) -> Self {
        Self {
            project_id: project_id.to_owned(),
            state,
            error: None,
            watching_since: None,
            last_event_at: None,
            last_scan_at: None,
            last_checkpoint_at: None,
        }
    }
}

#[derive(Default)]
struct Inner {
    projects: HashMap<String, WatchStatus>,
    /// Si `notify` no pudo arrancar, afecta a todos los proyectos.
    engine_error: Option<String>,
}

/// Tablero compartido. Barato de clonar.
#[derive(Clone, Default)]
pub struct StatusBoard(Arc<Mutex<Inner>>);

impl StatusBoard {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut inner)
    }

    pub fn get(&self, project_id: &str) -> WatchStatus {
        self.with(|inner| {
            inner.projects.get(project_id).cloned().unwrap_or_else(|| {
                let mut status = WatchStatus::new(project_id, WatchState::Stopped);
                if let Some(error) = &inner.engine_error {
                    status.state = WatchState::Error;
                    status.error = Some(error.clone());
                }
                status
            })
        })
    }

    pub(super) fn set_engine_error(&self, error: String) {
        self.with(|inner| inner.engine_error = Some(error));
    }

    pub(super) fn remove(&self, project_id: &str) {
        self.with(|inner| inner.projects.remove(project_id));
    }

    /// Aplica `f` al estado del proyecto y devuelve el resultado si cambió
    /// `state` o `error` (para avisar a la UI solo cuando importa).
    pub(super) fn update(&self, project_id: &str, f: impl FnOnce(&mut WatchStatus)) -> Option<WatchStatus> {
        self.with(|inner| {
            let status = inner
                .projects
                .entry(project_id.to_owned())
                .or_insert_with(|| WatchStatus::new(project_id, WatchState::Starting));
            let before = (status.state, status.error.clone());
            f(status);
            (before != (status.state, status.error.clone())).then(|| status.clone())
        })
    }

    pub(super) fn watching(&self, project_id: &str) -> Option<WatchStatus> {
        self.update(project_id, |s| {
            s.state = WatchState::Active;
            s.error = None;
            s.watching_since = Some(now_rfc3339());
        })
    }

    pub(super) fn failed(&self, project_id: &str, error: String) -> Option<WatchStatus> {
        self.update(project_id, |s| {
            s.state = WatchState::Error;
            s.error = Some(error);
            s.watching_since = None;
        })
    }

    pub(super) fn event(&self, project_id: &str) {
        self.update(project_id, |s| s.last_event_at = Some(now_rfc3339()));
    }

    /// Resultado de una revisión (automática o forzada).
    pub(super) fn scanned(&self, project_id: &str, checkpoint: bool, error: Option<String>, watching: bool) -> Option<WatchStatus> {
        self.update(project_id, |s| {
            let now = now_rfc3339();
            s.last_scan_at = Some(now.clone());
            if checkpoint {
                s.last_checkpoint_at = Some(now);
            }
            match error {
                Some(error) => {
                    s.state = WatchState::Error;
                    s.error = Some(error);
                }
                None if watching => {
                    s.state = WatchState::Active;
                    s.error = None;
                }
                None => {}
            }
        })
    }
}
