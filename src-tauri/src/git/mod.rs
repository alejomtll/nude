//! Operaciones Git (git2) de Nude.
//!
//! Invariante (AGENTS.md §2): nada de lo que hay aquí mueve `HEAD`, toca la
//! rama activa ni escribe `.git/index`. Los snapshots viven bajo
//! `refs/nude/checkpoints/<session_id>/<timestamp>`.

// `working_tree_diff` se expondrá como comando más adelante; hasta entonces solo
// lo usan los tests.
#[cfg_attr(not(test), allow(dead_code))]
mod diff;
mod repository;
mod restore;
mod snapshot;

pub use repository::register_repository;
#[cfg_attr(not(test), allow(unused_imports))]
pub use diff::working_tree_diff;
pub use restore::restore_working_tree;
pub use snapshot::{
    checkpoint_tree_id, create_snapshot, head_tree_id, working_tree_id,
};

#[cfg(test)]
mod tests;
