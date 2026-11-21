//! Operaciones Git (git2) de Nude.
//!
//! Invariante (AGENTS.md §2): los checkpoints automáticos (`snapshot`,
//! `restore`, `diff`, `history`) nunca mueven `HEAD`, tocan la rama activa ni
//! escriben `.git/index`; viven bajo `refs/nude/checkpoints/<session_id>/<ts>`.
//! Solo `branches` (crear/cambiar rama, commit) modifica el repo del usuario, y
//! únicamente cuando el usuario lo pide desde la UI.

// `working_tree_diff` se expondrá como comando más adelante; hasta entonces solo
// lo usan los tests.
mod branches;
mod guard;
#[cfg_attr(not(test), allow(dead_code))]
mod diff;
mod history;
mod repository;
mod restore;
mod snapshot;

pub use repository::{linked_worktrees, register_repository};
#[cfg_attr(not(test), allow(unused_imports))]
pub use diff::working_tree_diff;
pub use guard::{
    baseline_files_under, compare_with_baseline, ignored_files, is_path_ignored,
    restore_path_from_baseline,
};
pub use branches::{CommitResult, checkout_branch, create_branch, quick_commit};
pub use history::{
    GRAPH_COMMIT_LIMIT, RepoHistory, checkpoint_base_commit, checkpoint_base_commits,
    checkpoint_base_tree_id, read_history,
};
pub use restore::{restore_working_tree, restore_working_tree_to_base};
pub use snapshot::{
    checkpoint_tree_id, create_snapshot, head_tree_id, working_tree_id,
};

#[cfg(test)]
mod tests;
