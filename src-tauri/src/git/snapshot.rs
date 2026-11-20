use std::path::Path;

use git2::{ErrorCode, Index, IndexAddOption, Oid, Reference, Repository, Signature};
use serde::Serialize;

use super::diff::{collect_file_diffs, head_tree};
use super::repository::open_worktree;
use crate::db::models::FileDiff;
use crate::error::{NudeError, NudeResult};

pub const CHECKPOINT_REF_PREFIX: &str = "refs/nude/checkpoints";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// `refs/nude/checkpoints/<session_id>/<timestamp>`
    pub git_ref: String,
    /// Commit interno al que apunta la ref (padre = `HEAD` en ese momento).
    pub commit_id: String,
    /// Cambios del snapshot respecto a `HEAD`.
    pub files: Vec<FileDiff>,
}

/// Captura el estado completo del árbol de trabajo (tracked + untracked no
/// ignorados) como un commit colgante referenciado por
/// `refs/nude/checkpoints/<session_id>/<timestamp>`.
///
/// Garantías:
/// - No mueve `HEAD` ni la rama activa (`commit` con `update_ref = None`).
/// - No escribe `.git/index`: se trabaja sobre un índice en memoria, sin
///   fichero de respaldo, que parte de una copia del índice real.
/// - No toca ficheros del árbol de trabajo.
pub fn create_snapshot(root: &Path, session_id: &str, message: &str) -> NudeResult<Snapshot> {
    validate_ref_component(session_id)?;
    let repo = open_worktree(root)?;

    let tree_oid = snapshot_tree(&repo)?;
    let tree = repo.find_tree(tree_oid)?;

    let parent_commit = match repo.head() {
        Ok(head) => Some(head.peel_to_commit()?),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => None,
        Err(e) => return Err(e.into()),
    };
    let parents: Vec<_> = parent_commit.iter().collect();

    let signature = repo
        .signature()
        .or_else(|_| Signature::now("Nude", "nude@localhost"))?;
    let commit_oid = repo.commit(None, &signature, &signature, message, &tree, &parents)?;

    let git_ref = create_checkpoint_ref(&repo, session_id, commit_oid)?;

    let base_tree = head_tree(&repo)?;
    let diff = repo.diff_tree_to_tree(base_tree.as_ref(), Some(&tree), None)?;
    let files = collect_file_diffs(&diff)?;

    Ok(Snapshot {
        git_ref,
        commit_id: commit_oid.to_string(),
        files,
    })
}

/// Escribe en el ODB el árbol que representa el working tree actual.
fn snapshot_tree(repo: &Repository) -> NudeResult<Oid> {
    // Copiamos las entradas del índice real para reaprovechar su caché de
    // `stat` (solo se re-hashean ficheros modificados).
    let disk_index = repo.index()?;
    let mut scratch = Index::new()?;
    // Asociar el índice en memoria al repo (necesario para `add_all`) no
    // cambia `.git/index`; el índice en memoria no tiene ruta de escritura.
    repo.set_index(&mut scratch)?;
    for entry in disk_index.iter() {
        scratch.add(&entry)?;
    }
    drop(disk_index);

    scratch.add_all(["*"].iter(), IndexAddOption::DEFAULT, None)?;
    scratch.update_all(["*"].iter(), None)?;
    Ok(scratch.write_tree_to(repo)?)
}

fn create_checkpoint_ref(repo: &Repository, session_id: &str, oid: Oid) -> NudeResult<String> {
    let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%3fZ").to_string();
    let base = format!("{CHECKPOINT_REF_PREFIX}/{session_id}/{timestamp}");

    for attempt in 0..100u32 {
        let name = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        match repo.reference(&name, oid, false, "nude: checkpoint") {
            Ok(_) => return Ok(name),
            Err(e) if e.code() == ErrorCode::Exists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(NudeError::InvalidRefComponent(format!(
        "no se pudo generar una ref única bajo {base}"
    )))
}

fn validate_ref_component(component: &str) -> NudeResult<()> {
    let charset_ok = !component.is_empty()
        && component
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    let shape_ok = !component.starts_with('.')
        && !component.ends_with('.')
        && !component.ends_with(".lock")
        && !component.contains("..");
    let full = format!("{CHECKPOINT_REF_PREFIX}/{component}/x");
    if charset_ok && shape_ok && Reference::is_valid_name(&full) {
        Ok(())
    } else {
        Err(NudeError::InvalidRefComponent(component.to_owned()))
    }
}
