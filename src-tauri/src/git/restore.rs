use std::fs;
use std::path::{Component, Path, PathBuf};

use git2::{Delta, FileMode, Repository};

use super::repository::open_worktree;
use super::snapshot::{resolve_checkpoint_tree, snapshot_tree};
use crate::error::{NudeError, NudeResult};

/// Deja el árbol de trabajo (ficheros no ignorados) idéntico al árbol de la
/// ref de checkpoint: reescribe lo modificado, recrea lo borrado y elimina lo
/// que no existía entonces.
///
/// No toca `HEAD`, ramas ni `.git/index`, y respeta los ficheros ignorados.
/// Como no mantiene el índice, `git status` mostrará la diferencia respecto al
/// índice como cambios normales. Devuelve el nº de ficheros afectados.
pub fn restore_working_tree(root: &Path, git_ref: &str) -> NudeResult<usize> {
    let repo = open_worktree(root)?;
    let target = resolve_checkpoint_tree(&repo, git_ref)?;
    let current = repo.find_tree(snapshot_tree(&repo)?)?;

    let diff = repo.diff_tree_to_tree(Some(&current), Some(&target), None)?;
    let mut touched = 0usize;

    // 1) Borrados primero: así un fichero que pasa a ser directorio (o al revés)
    //    no estorba a las escrituras.
    for delta in diff.deltas() {
        if delta.status() == Delta::Deleted {
            let old = delta.old_file();
            if old.mode() == FileMode::Commit {
                continue;
            }
            let path = safe_join(root, old.path())?;
            remove_path(&path, root)?;
            touched += 1;
        }
    }

    // 2) Altas y modificaciones.
    for delta in diff.deltas() {
        match delta.status() {
            Delta::Deleted => {}
            _ => {
                let new = delta.new_file();
                let path = safe_join(root, new.path())?;
                write_entry(&repo, root, &path, new.id(), new.mode())?;
                touched += 1;
            }
        }
    }
    Ok(touched)
}

fn safe_join(root: &Path, rel: Option<&Path>) -> NudeResult<PathBuf> {
    let rel = rel.ok_or_else(|| NudeError::NotFound("ruta vacía en el diff".into()))?;
    let valid = rel.components().all(|c| match c {
        Component::Normal(name) => name != ".git",
        _ => false,
    });
    if !valid {
        return Err(NudeError::InvalidRefComponent(rel.display().to_string()));
    }
    Ok(root.join(rel))
}

fn remove_path(path: &Path, root: &Path) -> NudeResult<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // Poda directorios que hayan quedado vacíos (sin salir de la raíz).
    let mut dir = path.parent();
    while let Some(d) = dir {
        if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

fn write_entry(
    repo: &Repository,
    root: &Path,
    path: &Path,
    id: git2::Oid,
    mode: FileMode,
) -> NudeResult<()> {
    if matches!(mode, FileMode::Commit | FileMode::Tree | FileMode::Unreadable) {
        return Ok(()); // submódulos: fuera de alcance
    }
    let blob = repo.find_blob(id)?;
    if let Some(parent) = path.parent() {
        // Si algún ancestro (dentro de la raíz) es un fichero o symlink, se
        // sustituye por un directorio real. Del más cercano a la raíz hacia abajo.
        let mut ancestors: Vec<&Path> = parent.ancestors().filter(|a| a.starts_with(root)).collect();
        ancestors.retain(|a| *a != root);
        ancestors.reverse();
        for ancestor in ancestors {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if !meta.is_dir() => {
                    fs::remove_file(ancestor)?;
                    break;
                }
                _ => {}
            }
        }
        fs::create_dir_all(parent)?;
    }
    match fs::symlink_metadata(path) {
        // Tras borrar los archivos registrados, un directorio no vacío puede
        // contener archivos ignorados que el checkpoint no puede recuperar.
        Ok(meta) if meta.is_dir() => fs::remove_dir(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(_) => {}
    }

    if mode == FileMode::Link {
        #[cfg(unix)]
        {
            let target = String::from_utf8_lossy(blob.content()).into_owned();
            std::os::unix::fs::symlink(target, path)?;
            return Ok(());
        }
    }

    fs::write(path, blob.content())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let bits = if mode == FileMode::BlobExecutable { 0o755 } else { 0o644 };
        fs::set_permissions(path, fs::Permissions::from_mode(bits))?;
    }
    Ok(())
}
