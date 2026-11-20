//! Comandos del "Grafo Git": historial real del repo y acciones explícitas
//! del usuario (crear/cambiar rama, commit con selección de archivos).

use std::path::Path;

use serde::Serialize;
use tauri::{AppHandle, State};

use super::checkpoints::emit_checkpoint;
use super::{blocking, project_or_404};
use crate::db::Database;
use crate::db::models::Checkpoint;
use crate::error::NudeResult;
use crate::git::{self, CommitResult, RepoHistory};
use crate::watcher::{self, HomeDir};

/// Cuántos checkpoints recientes se superponen sobre el grafo.
const GRAPH_CHECKPOINT_LIMIT: usize = 200;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointAnchor {
    pub checkpoint_id: String,
    /// Commit de `HEAD` cuando se tomó el checkpoint (dónde colgarlo en el grafo).
    pub base_commit: Option<String>,
    pub agent_tool: String,
    pub model_name: Option<String>,
    pub prompt_summary: Option<String>,
    pub files_changed_count: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitGraph {
    #[serde(flatten)]
    pub history: RepoHistory,
    pub checkpoints: Vec<CheckpointAnchor>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchActionResult {
    pub branch: String,
    /// Checkpoints `manual` creados alrededor del cambio (antes/después).
    pub checkpoints: Vec<Checkpoint>,
}

#[tauri::command]
pub async fn get_git_graph(db: State<'_, Database>, project_id: String) -> NudeResult<GitGraph> {
    let db = db.inner().clone();
    blocking(move || {
        let project = project_or_404(&db, &project_id)?;
        let root = Path::new(&project.root_path);
        let history = git::read_history(root, git::GRAPH_COMMIT_LIMIT)?;

        let checkpoints: Vec<Checkpoint> = db
            .list_checkpoints(&project_id)?
            .into_iter()
            .take(GRAPH_CHECKPOINT_LIMIT)
            .collect();
        let refs: Vec<&str> = checkpoints.iter().map(|c| c.git_ref.as_str()).collect();
        let bases = git::checkpoint_base_commits(root, &refs)?;
        let checkpoints = checkpoints
            .into_iter()
            .zip(bases)
            .map(|(cp, base_commit)| CheckpointAnchor {
                checkpoint_id: cp.id,
                base_commit,
                agent_tool: cp.agent_tool,
                model_name: cp.model_name,
                prompt_summary: cp.prompt_summary,
                files_changed_count: cp.files_changed_count,
                created_at: cp.created_at,
            })
            .collect();
        Ok(GitGraph { history, checkpoints })
    })
    .await
}

/// Crea una rama (por defecto en `HEAD`, o en `fromCommit`). Con `checkout`,
/// cambia a ella guardando checkpoints antes y después.
#[tauri::command]
pub async fn git_create_branch(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    project_id: String,
    name: String,
    from_commit: Option<String>,
    checkout: bool,
) -> NudeResult<BranchActionResult> {
    let db = db.inner().clone();
    let home = home.inner().0.clone();
    let result = blocking(move || {
        let project = project_or_404(&db, &project_id)?;
        if !checkout {
            let branch = git::create_branch(Path::new(&project.root_path), &name, from_commit.as_deref(), false)?;
            return Ok(BranchActionResult { branch, checkpoints: Vec::new() });
        }
        let label = name.trim().to_owned();
        let change = watcher::guarded_change(
            &db,
            &home,
            &project,
            &format!("Antes de crear y cambiar a la rama {label}"),
            &format!("Rama {label}"),
            |root| git::create_branch(root, &name, from_commit.as_deref(), true),
        )?;
        Ok(BranchActionResult {
            branch: change.value,
            checkpoints: change.before.into_iter().chain(change.after).collect(),
        })
    })
    .await?;
    result.checkpoints.iter().for_each(|cp| emit_checkpoint(&app, cp));
    Ok(result)
}

/// Cambia de rama de forma segura (se niega si se perderían cambios locales).
/// El estado previo queda guardado como checkpoint.
#[tauri::command]
pub async fn git_checkout_branch(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    project_id: String,
    name: String,
) -> NudeResult<BranchActionResult> {
    let db = db.inner().clone();
    let home = home.inner().0.clone();
    let result = blocking(move || {
        let project = project_or_404(&db, &project_id)?;
        let change = watcher::guarded_change(
            &db,
            &home,
            &project,
            &format!("Antes de cambiar a la rama {name}"),
            &format!("Cambio a la rama {name}"),
            |root| git::checkout_branch(root, &name).map(|()| name.clone()),
        )?;
        Ok(BranchActionResult {
            branch: change.value,
            checkpoints: change.before.into_iter().chain(change.after).collect(),
        })
    })
    .await?;
    result.checkpoints.iter().for_each(|cp| emit_checkpoint(&app, cp));
    Ok(result)
}

/// Commit en la rama actual con exactamente los archivos elegidos.
#[tauri::command]
pub async fn git_quick_commit(
    db: State<'_, Database>,
    project_id: String,
    message: String,
    paths: Vec<String>,
) -> NudeResult<CommitResult> {
    let db = db.inner().clone();
    blocking(move || {
        let project = project_or_404(&db, &project_id)?;
        git::quick_commit(Path::new(&project.root_path), &message, &paths)
    })
    .await
}
