use std::path::Path;

use git2::{Delta, Diff, DiffOptions, ErrorCode, Patch, Repository, Tree};

use super::repository::{open_worktree, path_to_string};
use crate::db::models::{ChangeType, FileDiff};
use crate::error::NudeResult;

/// Parches mayores que esto se guardan sin texto (lockfiles, artefactos generados…).
pub const MAX_PATCH_BYTES: usize = 512 * 1024;

/// Diff del árbol de trabajo (incluidos ficheros sin seguimiento no ignorados)
/// respecto a `HEAD`. Equivale a `git diff HEAD` + untracked. Solo lectura.
pub fn working_tree_diff(root: &Path) -> NudeResult<Vec<FileDiff>> {
    let repo = open_worktree(root)?;
    let head_tree = head_tree(&repo)?;

    let mut opts = DiffOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .include_typechange(true);

    let diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    collect_file_diffs(&diff)
}

/// Árbol de `HEAD`, o `None` si la rama aún no tiene commits.
pub(super) fn head_tree(repo: &Repository) -> NudeResult<Option<Tree<'_>>> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_tree()?)),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(super) fn collect_file_diffs(diff: &Diff<'_>) -> NudeResult<Vec<FileDiff>> {
    let mut files = Vec::with_capacity(diff.deltas().len());

    for (idx, delta) in diff.deltas().enumerate() {
        let change_type = match delta.status() {
            Delta::Added | Delta::Untracked | Delta::Copied => ChangeType::Added,
            Delta::Deleted => ChangeType::Deleted,
            Delta::Modified | Delta::Renamed | Delta::Typechange | Delta::Conflicted => {
                ChangeType::Modified
            }
            Delta::Unmodified | Delta::Ignored | Delta::Unreadable => continue,
        };

        let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) else {
            continue;
        };
        let file_path = path_to_string(path)?;

        let patch_text = if delta.flags().is_binary() {
            None
        } else {
            match Patch::from_diff(diff, idx)? {
                Some(mut patch) => {
                    let buf = patch.to_buf()?;
                    if buf.len() > MAX_PATCH_BYTES {
                        None
                    } else {
                        Some(String::from_utf8_lossy(&buf).into_owned())
                    }
                }
                None => None,
            }
        };

        files.push(FileDiff {
            file_path,
            change_type,
            patch_text,
        });
    }

    Ok(files)
}
