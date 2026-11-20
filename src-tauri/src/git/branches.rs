//! Operaciones Git **explícitas** que el usuario lanza desde la UI: crear
//! rama, cambiar de rama y commit con selección de ficheros.
//!
//! A diferencia de los checkpoints automáticos, estas sí tocan `HEAD`, ramas o
//! `.git/index`, pero solo porque el usuario lo pide (AGENTS.md §2: nada de
//! commits *no solicitados*). Todas fallan antes de escribir si algo no cuadra.

use std::path::{Component, Path};

use git2::{Branch, BranchType, ErrorCode, Index, IndexAddOption, Repository, RepositoryState, build::CheckoutBuilder};
use serde::Serialize;

use super::history::head_info;
use super::repository::open_worktree;
use crate::error::{NudeError, NudeResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitResult {
    pub commit_id: String,
    pub short_id: String,
    pub branch: Option<String>,
    pub files_committed: usize,
}

fn ensure_clean_state(repo: &Repository) -> NudeResult<()> {
    if repo.state() != RepositoryState::Clean {
        return Err(NudeError::GitOperation(format!(
            "el repositorio está en mitad de una operación ({:?}); termínala o abórtala en la terminal",
            repo.state()
        )));
    }
    Ok(())
}

fn validate_branch_name(name: &str) -> NudeResult<()> {
    let name = name.trim();
    if name.is_empty() || name.starts_with('-') || !Branch::name_is_valid(name).unwrap_or(false) {
        return Err(NudeError::GitOperation(format!("«{name}» no es un nombre de rama válido")));
    }
    Ok(())
}

/// Crea `name` en `from_commit` (o en `HEAD`). Si `checkout`, cambia a ella.
pub fn create_branch(
    root: &Path,
    name: &str,
    from_commit: Option<&str>,
    checkout: bool,
) -> NudeResult<String> {
    let name = name.trim();
    validate_branch_name(name)?;
    let repo = open_worktree(root)?;
    ensure_clean_state(&repo)?;

    if repo.find_branch(name, BranchType::Local).is_ok() {
        return Err(NudeError::GitOperation(format!("la rama «{name}» ya existe")));
    }
    let base = match from_commit {
        Some(id) => repo.find_commit(git2::Oid::from_str(id)?)?,
        None => repo.head().and_then(|h| h.peel_to_commit()).map_err(|_| {
            NudeError::GitOperation("el repositorio no tiene commits todavía; haz el primero".into())
        })?,
    };
    repo.branch(name, &base, false)?;
    drop(base);
    drop(repo);

    if checkout {
        checkout_branch(root, name)?;
    }
    Ok(name.to_owned())
}

/// Cambia a la rama local `name` de forma segura: los cambios locales que no
/// chocan se conservan (como `git switch`) y, si alguno chocaría, no se toca
/// nada y se devuelve un error explicativo.
pub fn checkout_branch(root: &Path, name: &str) -> NudeResult<()> {
    let repo = open_worktree(root)?;
    ensure_clean_state(&repo)?;
    let branch = repo
        .find_branch(name, BranchType::Local)
        .map_err(|_| NudeError::NotFound(format!("rama {name}")))?;
    let refname = branch
        .get()
        .name()
        .map_err(|_| NudeError::GitOperation("nombre de rama no UTF-8".into()))?
        .to_owned();
    if branch.is_head() {
        return Ok(());
    }
    let target = branch.get().peel_to_commit()?;

    let mut opts = CheckoutBuilder::new();
    opts.safe();
    match repo.checkout_tree(target.as_object(), Some(&mut opts)) {
        Ok(()) => {}
        Err(e) if e.code() == ErrorCode::Conflict => {
            return Err(NudeError::GitOperation(format!(
                "tienes cambios sin commit que chocarían con «{name}». Haz commit o un checkpoint y \
                 descarta esos cambios antes de cambiar de rama ({})",
                e.message()
            )));
        }
        Err(e) => return Err(e.into()),
    }
    repo.set_head(&refname)?;
    Ok(())
}

fn validate_rel_path(path: &str) -> NudeResult<&Path> {
    let p = Path::new(path);
    let ok = !path.is_empty()
        && p.components().all(|c| matches!(c, Component::Normal(n) if n != ".git"));
    if ok {
        Ok(p)
    } else {
        Err(NudeError::GitOperation(format!("ruta no válida: {path}")))
    }
}

/// Commit en la rama actual con **exactamente** los ficheros indicados.
///
/// El árbol se construye a partir de `HEAD` + las rutas elegidas (en un índice
/// en memoria), así que otros cambios ya preparados con `git add` no se cuelan
/// en el commit y siguen preparados. Después se sincroniza el índice real solo
/// para esas rutas.
pub fn quick_commit(root: &Path, message: &str, paths: &[String]) -> NudeResult<CommitResult> {
    let message = message.trim();
    if message.is_empty() {
        return Err(NudeError::GitOperation("el mensaje del commit no puede estar vacío".into()));
    }
    if paths.is_empty() {
        return Err(NudeError::GitOperation("selecciona al menos un archivo".into()));
    }
    let rel_paths = paths
        .iter()
        .map(|p| validate_rel_path(p))
        .collect::<NudeResult<Vec<_>>>()?;

    let repo = open_worktree(root)?;
    ensure_clean_state(&repo)?;
    let signature = repo.signature().map_err(|_| {
        NudeError::GitOperation(
            "Git no tiene autor configurado. Ejecuta `git config --global user.name \"Tu Nombre\"` \
             y `git config --global user.email tu@correo` y vuelve a intentarlo"
                .into(),
        )
    })?;

    let parent = match repo.head() {
        Ok(h) => Some(h.peel_to_commit()?),
        Err(e) if matches!(e.code(), ErrorCode::UnbornBranch | ErrorCode::NotFound) => None,
        Err(e) => return Err(e.into()),
    };

    // Índice temporal = HEAD + rutas seleccionadas.
    let mut scratch = Index::new()?;
    if let Some(parent) = &parent {
        scratch.read_tree(&parent.tree()?)?;
    }
    repo.set_index(&mut scratch)?;
    let (present, removed): (Vec<&Path>, Vec<&Path>) = rel_paths
        .iter()
        .copied()
        .partition(|rel| root.join(rel).symlink_metadata().is_ok());
    for rel in &removed {
        // Borrado: puede no estar en HEAD (añadido y luego borrado), se ignora.
        let _ = scratch.remove_path(rel);
    }
    if !present.is_empty() {
        // Rutas exactas (sin globs) y aunque coincidan con `.gitignore`: el
        // usuario las eligió explícitamente de la lista de cambios.
        scratch.add_all(
            present.iter(),
            IndexAddOption::DISABLE_PATHSPEC_MATCH | IndexAddOption::FORCE,
            None,
        )?;
    }
    let tree_id = scratch.write_tree_to(&repo)?;
    let tree = repo.find_tree(tree_id)?;
    if parent.as_ref().is_some_and(|p| p.tree_id() == tree_id) {
        return Err(NudeError::GitOperation("los archivos elegidos no tienen cambios respecto a HEAD".into()));
    }

    let parents: Vec<_> = parent.iter().collect();
    let commit_id = repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &parents)?;

    // Sincroniza el índice real solo para las rutas confirmadas. Se abre desde
    // un `Repository` nuevo: `set_index` desvincula el índice original.
    let fresh = open_worktree(root)?;
    let mut real_index = fresh.index()?;
    for rel in &rel_paths {
        match tree.get_path(rel) {
            Ok(_) => real_index.add_path(rel)?,
            Err(_) => {
                let _ = real_index.remove_path(rel);
            }
        }
    }
    real_index.write()?;

    let id = commit_id.to_string();
    Ok(CommitResult {
        short_id: id.chars().take(7).collect(),
        commit_id: id,
        branch: head_info(&fresh)?.branch,
        files_committed: rel_paths.len(),
    })
}
