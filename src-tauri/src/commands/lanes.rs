//! Carriles paralelos (worktrees aislados por agente).
//!
//! La lógica vive en funciones normales (`create_lane`, `merge_lane`…) que
//! reciben la base de datos y el watcher, para poder probar el ciclo completo
//! sin Tauri; los `#[tauri::command]` solo las envuelven.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use super::checkpoints::emit_checkpoint;
use super::watcher::{AgentSighting, sighting};
use super::{blocking, project_or_404};
use crate::db::Database;
use crate::db::models::{AgentLane, ChangeType, Checkpoint, LaneStatus, Project, new_id};
use crate::error::{NudeError, NudeResult};
use crate::git::{self, LaneMerge};
use crate::watcher::{self, HomeDir, WatcherHandle};

/// Evento con el `project_id` cuando se crea, aplica o descarta un carril.
pub const EVENT_LANES_CHANGED: &str = "lanes-changed";

/// Carpeta de los worktrees de carriles:
/// `~/Library/Application Support/com.nude.app/worktrees`.
#[derive(Debug, Clone)]
pub struct LaneRoot(pub PathBuf);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneFile {
    pub file_path: String,
    pub change_type: ChangeType,
}

/// Carril activo con lo que la vista "Paralelo" enseña en su columna.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneSummary {
    #[serde(flatten)]
    pub lane: AgentLane,
    /// Proyecto interno que vigila el worktree (checkpoints del carril).
    pub lane_project_id: Option<String>,
    /// Rama del proyecto sobre la que se aplicaría.
    pub base_branch: Option<String>,
    pub files: Vec<LaneFile>,
    /// Rutas que chocarían al aplicarlo ahora.
    pub conflicts: Vec<String>,
    /// Última sesión de agente detectada en el worktree.
    pub last_activity: Option<AgentSighting>,
    pub checkpoint_count: usize,
    pub last_checkpoint_at: Option<String>,
    /// El worktree no se pudo leer (p. ej. se borró a mano).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneDiffResponse {
    pub lane_id: String,
    #[serde(flatten)]
    pub diff: git::LaneDiff,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeLaneResult {
    /// `false` = había conflictos y no se tocó nada (ver `conflicts`).
    pub merged: bool,
    pub conflicts: Vec<String>,
    pub commit_id: Option<String>,
    pub branch: Option<String>,
    pub files_merged: usize,
    /// Checkpoints `manual` del proyecto antes/después de aplicar.
    pub checkpoints: Vec<Checkpoint>,
    /// El merge se hizo, pero no se pudo borrar el worktree o la rama.
    pub cleanup_error: Option<String>,
}

fn lane_or_404(db: &Database, lane_id: &str) -> NudeResult<AgentLane> {
    db.get_lane(lane_id)?
        .ok_or_else(|| NudeError::NotFound(format!("carril {lane_id}")))
}

fn active_lane(db: &Database, lane_id: &str) -> NudeResult<AgentLane> {
    let lane = lane_or_404(db, lane_id)?;
    if lane.status != LaneStatus::Active {
        return Err(NudeError::GitOperation(format!("el carril «{}» ya está cerrado", lane.name)));
    }
    Ok(lane)
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Crea el worktree, lo registra y empieza a vigilarlo.
pub(crate) fn create_lane(
    db: &Database,
    watcher: &WatcherHandle,
    base: &Path,
    project_id: &str,
    name: &str,
    agent_hint: Option<&str>,
) -> NudeResult<AgentLane> {
    let project = project_or_404(db, project_id)?;
    let name = match name.trim() {
        "" => format!("Carril {}", db.list_active_lanes(project_id)?.len() + 1),
        name => name.chars().take(80).collect(),
    };
    let agent_hint = agent_hint.map(str::trim).filter(|h| !h.is_empty()).map(str::to_owned);
    let root = Path::new(&project.root_path);
    let created = git::create_lane_worktree(root, &base.join(&project.id), &name)?;

    let lane = AgentLane {
        id: new_id(),
        project_id: project.id.clone(),
        name: name.clone(),
        branch_name: created.branch.clone(),
        worktree_path: created.path.clone(),
        agent_hint,
        status: LaneStatus::Active,
        created_at: now_millis(),
    };
    let registered = db.insert_lane(&lane).and_then(|()| {
        let (inner, _) = db.upsert_project(&format!("{} · {name}", project.name), &lane.worktree_path)?;
        db.copy_guardrails(&project.id, &inner.id)?;
        Ok(inner)
    });
    match registered {
        Ok(inner) => {
            watcher.watch(inner);
            Ok(lane)
        }
        Err(e) => {
            // Sin registro no hay forma de gestionarlo desde la UI: se deshace.
            let _ = git::remove_lane_worktree(root, base, Path::new(&lane.worktree_path), &lane.branch_name);
            let _ = db.set_lane_status(&lane.id, LaneStatus::Discarded);
            Err(e)
        }
    }
}

fn summarize(db: &Database, home: &Path, project: &Project, lane: AgentLane) -> LaneSummary {
    let lane_path = Path::new(&lane.worktree_path);
    let inner = db.lane_project(&lane).ok().flatten();
    let checkpoints = inner
        .as_ref()
        .and_then(|p| db.list_checkpoints(&p.id).ok())
        .unwrap_or_default();
    let (base_branch, files, conflicts, error) = match git::lane_diff(Path::new(&project.root_path), lane_path) {
        Ok(diff) => (
            diff.base_branch,
            diff.files
                .into_iter()
                .map(|f| LaneFile { file_path: f.file_path, change_type: f.change_type })
                .collect(),
            diff.conflicts,
            None,
        ),
        Err(e) => (None, Vec::new(), Vec::new(), Some(e.to_string())),
    };
    LaneSummary {
        lane_project_id: inner.map(|p| p.id),
        base_branch,
        files,
        conflicts,
        last_activity: sighting(home, lane_path, SystemTime::now()),
        checkpoint_count: checkpoints.len(),
        last_checkpoint_at: checkpoints.first().map(|c| c.created_at.clone()),
        error,
        lane,
    }
}

pub(crate) fn list_lanes(db: &Database, home: &Path, project_id: &str) -> NudeResult<Vec<LaneSummary>> {
    let project = project_or_404(db, project_id)?;
    Ok(db
        .list_active_lanes(project_id)?
        .into_iter()
        .map(|lane| summarize(db, home, &project, lane))
        .collect())
}

pub(crate) fn lane_diff(db: &Database, lane_id: &str) -> NudeResult<LaneDiffResponse> {
    let lane = lane_or_404(db, lane_id)?;
    let project = project_or_404(db, &lane.project_id)?;
    let diff = git::lane_diff(Path::new(&project.root_path), Path::new(&lane.worktree_path))?;
    Ok(LaneDiffResponse { lane_id: lane.id, diff })
}

/// Deja de vigilar el carril y borra su worktree y su rama.
fn close_lane(db: &Database, watcher: &WatcherHandle, base: &Path, project: &Project, lane: &AgentLane) -> NudeResult<()> {
    if let Some(inner) = db.lane_project(lane)? {
        watcher.remove_project(db, inner.id)?;
    }
    git::remove_lane_worktree(
        Path::new(&project.root_path),
        base,
        Path::new(&lane.worktree_path),
        &lane.branch_name,
    )
}

pub(crate) fn merge_lane(
    db: &Database,
    watcher: &WatcherHandle,
    home: &Path,
    base: &Path,
    lane_id: &str,
    resolve_conflicts: bool,
) -> NudeResult<MergeLaneResult> {
    let lane = active_lane(db, lane_id)?;
    let project = project_or_404(db, &lane.project_id)?;
    let lane_path = Path::new(&lane.worktree_path);

    // Comprobación previa (solo lectura): con conflictos sin confirmar no se
    // crea ni siquiera el checkpoint de seguridad.
    let preview = git::lane_diff(Path::new(&project.root_path), lane_path)?;
    if !preview.conflicts.is_empty() && !resolve_conflicts {
        return Ok(MergeLaneResult {
            merged: false,
            conflicts: preview.conflicts,
            commit_id: None,
            branch: preview.base_branch,
            files_merged: 0,
            checkpoints: Vec::new(),
            cleanup_error: None,
        });
    }

    let change = watcher::guarded_change(
        db,
        home,
        &project,
        &format!("Antes de aplicar el carril «{}»", lane.name),
        &format!("Carril «{}» aplicado", lane.name),
        |root| git::merge_lane(root, lane_path, &lane.name, &lane.branch_name, resolve_conflicts),
    )?;
    let checkpoints: Vec<Checkpoint> = change.before.into_iter().chain(change.after).collect();
    match change.value {
        LaneMerge::Conflicts(conflicts) => Ok(MergeLaneResult {
            merged: false,
            conflicts,
            commit_id: None,
            branch: preview.base_branch,
            files_merged: 0,
            checkpoints,
            cleanup_error: None,
        }),
        LaneMerge::Merged { commit_id, branch, files } => {
            db.set_lane_status(&lane.id, LaneStatus::Merged)?;
            let cleanup_error = close_lane(db, watcher, base, &project, &lane).err().map(|e| e.to_string());
            Ok(MergeLaneResult {
                merged: true,
                conflicts: Vec::new(),
                commit_id: Some(commit_id),
                branch: Some(branch),
                files_merged: files,
                checkpoints,
                cleanup_error,
            })
        }
    }
}

pub(crate) fn discard_lane(db: &Database, watcher: &WatcherHandle, base: &Path, lane_id: &str) -> NudeResult<AgentLane> {
    let lane = active_lane(db, lane_id)?;
    let project = project_or_404(db, &lane.project_id)?;
    close_lane(db, watcher, base, &project, &lane)?;
    db.set_lane_status(&lane.id, LaneStatus::Discarded)?;
    Ok(AgentLane { status: LaneStatus::Discarded, ..lane })
}

/// Al quitar un proyecto de Nude se dejan de vigilar sus carriles. Sus
/// worktrees se conservan en disco (`git worktree list` los sigue mostrando).
pub(crate) fn forget_lanes_of(db: &Database, watcher: &WatcherHandle, project_id: &str) -> NudeResult<()> {
    for inner in db.lane_projects_of(project_id)? {
        watcher.remove_project(db, inner)?;
    }
    Ok(())
}

fn emit_lanes_changed(app: &AppHandle, project_id: &str) {
    if let Err(e) = app.emit(EVENT_LANES_CHANGED, project_id) {
        eprintln!("nude: no se pudo emitir {EVENT_LANES_CHANGED}: {e}");
    }
}

// ------------------------------------------------------------------- comandos

#[tauri::command]
pub async fn create_parallel_lane(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    watcher: State<'_, WatcherHandle>,
    lanes: State<'_, LaneRoot>,
    project_id: String,
    name: String,
    agent_hint: Option<String>,
) -> NudeResult<LaneSummary> {
    let (db, home, watcher, base) = (db.inner().clone(), home.0.clone(), watcher.inner().clone(), lanes.0.clone());
    let summary = blocking(move || {
        let lane = create_lane(&db, &watcher, &base, &project_id, &name, agent_hint.as_deref())?;
        let project = project_or_404(&db, &lane.project_id)?;
        Ok(summarize(&db, &home, &project, lane))
    })
    .await?;
    emit_lanes_changed(&app, &summary.lane.project_id);
    Ok(summary)
}

#[tauri::command]
pub async fn list_parallel_lanes(
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    project_id: String,
) -> NudeResult<Vec<LaneSummary>> {
    let (db, home) = (db.inner().clone(), home.0.clone());
    blocking(move || list_lanes(&db, &home, &project_id)).await
}

#[tauri::command]
pub async fn get_lane_diff(db: State<'_, Database>, lane_id: String) -> NudeResult<LaneDiffResponse> {
    let db = db.inner().clone();
    blocking(move || lane_diff(&db, &lane_id)).await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn merge_parallel_lane(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    watcher: State<'_, WatcherHandle>,
    lanes: State<'_, LaneRoot>,
    lane_id: String,
    resolve_conflicts: Option<bool>,
) -> NudeResult<MergeLaneResult> {
    let (db, home, watcher, base) = (db.inner().clone(), home.0.clone(), watcher.inner().clone(), lanes.0.clone());
    let (project_id, result) = blocking(move || {
        let project_id = lane_or_404(&db, &lane_id)?.project_id;
        let result = merge_lane(&db, &watcher, &home, &base, &lane_id, resolve_conflicts.unwrap_or(false))?;
        Ok((project_id, result))
    })
    .await?;
    for checkpoint in &result.checkpoints {
        emit_checkpoint(&app, checkpoint);
    }
    if result.merged {
        emit_lanes_changed(&app, &project_id);
    }
    Ok(result)
}

#[tauri::command]
pub async fn discard_parallel_lane(
    app: AppHandle,
    db: State<'_, Database>,
    watcher: State<'_, WatcherHandle>,
    lanes: State<'_, LaneRoot>,
    lane_id: String,
) -> NudeResult<AgentLane> {
    let (db, watcher, base) = (db.inner().clone(), watcher.inner().clone(), lanes.0.clone());
    let lane = blocking(move || discard_lane(&db, &watcher, &base, &lane_id)).await?;
    emit_lanes_changed(&app, &lane.project_id);
    Ok(lane)
}

#[cfg(test)]
mod tests;

