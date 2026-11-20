use std::path::{Path, PathBuf};

use git2::Repository;
use serde::Serialize;

use crate::error::{NudeError, NudeResult};

/// Datos mínimos de un repositorio validado.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RepoInfo {
    /// Nombre legible (carpeta raíz del árbol de trabajo).
    pub name: String,
    /// Raíz canónica del árbol de trabajo (sin symlinks ni barra final).
    pub root_path: String,
}

/// Valida que `path` (o alguno de sus padres) sea un repositorio Git con
/// árbol de trabajo y devuelve su raíz canónica. No modifica nada.
pub fn register_repository(path: &Path) -> NudeResult<RepoInfo> {
    let display = path.display().to_string();
    let repo = Repository::discover(path).map_err(|_| NudeError::NotARepository(display.clone()))?;
    let workdir = repo
        .workdir()
        .ok_or_else(|| NudeError::BareRepository(display.clone()))?;
    let root = std::fs::canonicalize(workdir)?;
    let root_path = path_to_string(&root)?;
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root_path.clone());
    Ok(RepoInfo { name, root_path })
}

/// Abre el repositorio cuya raíz de trabajo es exactamente `root`.
pub(super) fn open_worktree(root: &Path) -> NudeResult<Repository> {
    let display = root.display().to_string();
    let repo = Repository::open(root).map_err(|_| NudeError::NotARepository(display.clone()))?;
    if repo.is_bare() {
        return Err(NudeError::BareRepository(display));
    }
    Ok(repo)
}

pub(super) fn path_to_string(path: &Path) -> NudeResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| NudeError::NonUtf8Path(PathBuf::from(path).display().to_string()))
}
