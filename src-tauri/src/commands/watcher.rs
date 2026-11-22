//! Diagnóstico del watcher (indicador "en vivo") y escaneo forzado.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde::Serialize;
use tauri::State;

use super::{blocking, project_or_404};
use crate::db::Database;
use crate::db::models::Checkpoint;
use crate::error::NudeResult;
use crate::git;
use crate::watcher::{self, ACTIVE_WINDOW, AGENT_WINDOW, HomeDir, WatchStatus, WatcherHandle};

/// Hasta dónde mira el diagnóstico para enseñar "la última sesión detectada".
const DIAGNOSTIC_WINDOW: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSighting {
    pub agent_tool: String,
    pub model_name: Option<String>,
    /// `cwd` del agente según su historial (normalizado).
    pub cwd: Option<String>,
    pub last_activity: String,
    /// Dentro de la ventana de atribución de checkpoints (15 min).
    pub attributable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDiagnostic {
    pub path: String,
    pub branch: Option<String>,
    /// Proyecto de Nude que ya vigila este worktree, si lo hay.
    pub project_id: Option<String>,
    pub last_session: Option<AgentSighting>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatcherDiagnostics {
    pub status: WatchStatus,
    pub root_path: String,
    pub last_session: Option<AgentSighting>,
    /// Worktrees enlazados del repo: los agentes que trabajan ahí no cambian
    /// el árbol de este proyecto, así que sus cambios no generan checkpoints aquí.
    pub worktrees: Vec<WorktreeDiagnostic>,
    pub attribution_window_minutes: u64,
    pub firewall_window_minutes: u64,
}

fn rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub(super) fn sighting(home: &Path, root: &Path, now: SystemTime) -> Option<AgentSighting> {
    let (at, session) = watcher::latest_session(home, root, now, DIAGNOSTIC_WINDOW)?;
    Some(AgentSighting {
        agent_tool: session.tool.as_str().to_owned(),
        model_name: session.model_name,
        cwd: session.cwd,
        last_activity: rfc3339(at),
        attributable: now.duration_since(at).map_or(true, |age| age <= ACTIVE_WINDOW),
    })
}

#[tauri::command]
pub async fn get_watcher_diagnostics(
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    watcher: State<'_, WatcherHandle>,
    project_id: String,
) -> NudeResult<WatcherDiagnostics> {
    let db = db.inner().clone();
    let home = home.inner().0.clone();
    let watcher = watcher.inner().clone();
    blocking(move || {
        let project = project_or_404(&db, &project_id)?;
        let root = Path::new(&project.root_path);
        let now = SystemTime::now();
        let projects = db.list_watch_targets()?;
        let worktrees = git::linked_worktrees(root)
            .unwrap_or_default()
            .into_iter()
            .map(|wt| {
                let normalized = watcher::normalize_path(Path::new(&wt.path));
                WorktreeDiagnostic {
                    project_id: projects
                        .iter()
                        .find(|p| watcher::normalize_path(Path::new(&p.root_path)) == normalized)
                        .map(|p| p.id.clone()),
                    last_session: sighting(&home, Path::new(&wt.path), now),
                    path: wt.path,
                    branch: wt.branch,
                }
            })
            .collect();
        Ok(WatcherDiagnostics {
            status: watcher.status(&project_id),
            last_session: sighting(&home, root, now),
            root_path: project.root_path,
            worktrees,
            attribution_window_minutes: ACTIVE_WINDOW.as_secs() / 60,
            firewall_window_minutes: AGENT_WINDOW.as_secs() / 60,
        })
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub checkpoint: Option<Checkpoint>,
    pub status: WatchStatus,
}

/// "Forzar escaneo ahora": revisa el árbol sin esperar a `notify` (y reintenta
/// la vigilancia si había fallado). El checkpoint, si lo hay, también se emite
/// como `checkpoint-created`.
#[tauri::command]
pub async fn force_scan(watcher: State<'_, WatcherHandle>, project_id: String) -> NudeResult<ScanResult> {
    let watcher = watcher.inner().clone();
    blocking(move || {
        let checkpoint = watcher.force_scan(project_id.clone())?;
        Ok(ScanResult { checkpoint, status: watcher.status(&project_id) })
    })
    .await
}

#[tauri::command]
pub async fn get_watcher_status(watcher: State<'_, WatcherHandle>, project_id: String) -> NudeResult<WatchStatus> {
    Ok(watcher.status(&project_id))
}
