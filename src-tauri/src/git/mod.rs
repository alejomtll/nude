//! Operaciones Git (git2) de Nude.
//!
//! Invariante (AGENTS.md §2): nada de lo que hay aquí mueve `HEAD`, toca la
//! rama activa ni escribe `.git/index`. Los snapshots viven bajo
//! `refs/nude/checkpoints/<session_id>/<timestamp>`.

// `diff` y `snapshot` los consumirá el watcher de la Fase 2; hasta entonces
// solo los usan los tests. Quitar estos `allow` al conectarlos.
#[cfg_attr(not(test), allow(dead_code))]
mod diff;
mod repository;
#[cfg_attr(not(test), allow(dead_code))]
mod snapshot;

pub use repository::register_repository;
#[cfg_attr(not(test), allow(unused_imports))]
pub use diff::working_tree_diff;
#[allow(unused_imports)]
pub use snapshot::{CHECKPOINT_REF_PREFIX, Snapshot, create_snapshot};

#[cfg(test)]
mod tests;
