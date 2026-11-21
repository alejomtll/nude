//! Restauración quirúrgica de archivos sueltos para el firewall de archivos.
//!
//! El "snapshot de referencia" es el árbol del último checkpoint del proyecto
//! (o el de `HEAD` si todavía no hay ninguno). Igual que `restore`, nada de
//! esto mueve `HEAD`, toca ramas ni escribe `.git/index`.

use std::fs;
use std::path::Path;

use git2::{FileMode, ObjectType, Oid, Repository, Status, StatusOptions, Tree, TreeWalkMode, TreeWalkResult};

use super::diff::head_tree;
use super::repository::open_worktree;
use super::restore::{remove_path, safe_join, write_entry};
use super::snapshot::resolve_checkpoint_tree;
use crate::error::NudeResult;

/// Cómo está un archivo respecto al snapshot de referencia.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineComparison {
    pub in_baseline: bool,
    pub on_disk: bool,
    /// El contenido actual coincide con el del snapshot (o falta en ambos).
    pub unchanged: bool,
    /// Huella del contenido actual (`None` si no existe en disco).
    pub current_id: Option<Oid>,
}

fn baseline_tree<'r>(repo: &'r Repository, checkpoint_ref: Option<&str>) -> NudeResult<Option<Tree<'r>>> {
    match checkpoint_ref {
        Some(git_ref) => Ok(Some(resolve_checkpoint_tree(repo, git_ref)?)),
        None => head_tree(repo),
    }
}

fn baseline_entry(tree: Option<&Tree<'_>>, rel: &str) -> Option<(Oid, FileMode)> {
    let entry = tree?.get_path(Path::new(rel)).ok()?;
    let mode = match entry.filemode() {
        0o100755 => FileMode::BlobExecutable,
        0o120000 => FileMode::Link,
        0o100644 => FileMode::Blob,
        _ => return None, // directorios y submódulos no son archivos a restaurar
    };
    Some((entry.id(), mode))
}

/// Huella Git (blob) del archivo en disco, con los filtros del repositorio.
fn current_blob_id(repo: &Repository, root: &Path, rel: &str) -> NudeResult<Option<Oid>> {
    let path = root.join(rel);
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if meta.is_dir() {
        return Ok(None);
    }
    if meta.file_type().is_symlink() {
        let target = fs::read_link(&path)?;
        return Ok(Some(Oid::hash_object(ObjectType::Blob, target.as_os_str().as_encoded_bytes())?));
    }
    Ok(Some(repo.blob_path(&path)?))
}

/// Compara `rel` (ruta relativa con `/`) con el snapshot de referencia.
pub fn compare_with_baseline(root: &Path, checkpoint_ref: Option<&str>, rel: &str) -> NudeResult<BaselineComparison> {
    let repo = open_worktree(root)?;
    let tree = baseline_tree(&repo, checkpoint_ref)?;
    let baseline = baseline_entry(tree.as_ref(), rel).map(|(id, _)| id);
    let current = current_blob_id(&repo, root, rel)?;
    Ok(BaselineComparison {
        in_baseline: baseline.is_some(),
        on_disk: current.is_some(),
        unchanged: baseline == current,
        current_id: current,
    })
}

/// Deja `rel` como en el snapshot: lo reescribe si existía entonces o lo borra
/// si no. Devuelve `true` si tocó el disco.
pub fn restore_path_from_baseline(root: &Path, checkpoint_ref: Option<&str>, rel: &str) -> NudeResult<bool> {
    let repo = open_worktree(root)?;
    let tree = baseline_tree(&repo, checkpoint_ref)?;
    let path = safe_join(root, Some(Path::new(rel)))?;
    match baseline_entry(tree.as_ref(), rel) {
        Some((id, mode)) => {
            if current_blob_id(&repo, root, rel)? == Some(id) {
                return Ok(false);
            }
            write_entry(&repo, root, &path, id, mode)?;
            Ok(true)
        }
        None => {
            if fs::symlink_metadata(&path).is_err() {
                return Ok(false);
            }
            remove_path(&path, root)?;
            Ok(true)
        }
    }
}

/// Archivos del snapshot que viven bajo la carpeta `prefix` (para cuando el
/// sistema solo avisa del borrado de la carpeta entera).
pub fn baseline_files_under(root: &Path, checkpoint_ref: Option<&str>, prefix: &str) -> NudeResult<Vec<String>> {
    let repo = open_worktree(root)?;
    let Some(tree) = baseline_tree(&repo, checkpoint_ref)? else {
        return Ok(Vec::new());
    };
    let Ok(entry) = tree.get_path(Path::new(prefix)) else {
        return Ok(Vec::new());
    };
    let Ok(subtree) = repo.find_tree(entry.id()) else {
        return Ok(Vec::new()); // `prefix` es un archivo
    };
    let mut files = Vec::new();
    subtree.walk(TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(ObjectType::Blob) {
            if let Ok(name) = entry.name() {
                files.push(format!("{prefix}/{dir}{name}"));
            }
        }
        TreeWalkResult::Ok
    })?;
    Ok(files)
}

/// `true` si `.gitignore` excluye la ruta (no hay snapshot Git de ella).
pub fn is_path_ignored(root: &Path, rel: &str) -> NudeResult<bool> {
    let repo = open_worktree(root)?;
    Ok(repo.is_path_ignored(Path::new(rel))?)
}

/// Archivos ignorados por Git (sin entrar en carpetas ignoradas enteras como
/// `node_modules/`). El firewall guarda copia en memoria de los que protege.
pub fn ignored_files(root: &Path) -> NudeResult<Vec<String>> {
    let repo = open_worktree(root)?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(false)
        .exclude_submodules(true);
    let statuses = repo.statuses(Some(&mut options))?;
    Ok(statuses
        .iter()
        .filter(|s| s.status().contains(Status::IGNORED))
        .filter_map(|s| s.path().ok().map(str::to_owned))
        .filter(|p| !p.ends_with('/'))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::Signature;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn repo_with(files: &[(&str, &str)]) -> (Tmp, PathBuf) {
        let base = std::env::temp_dir().join(format!("nude-guard-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).expect("mkdir");
        let root = fs::canonicalize(&base).expect("canon");
        let repo = Repository::init(&root).expect("init");
        let mut index = repo.index().expect("index");
        for (path, body) in files {
            let full = root.join(path);
            fs::create_dir_all(full.parent().expect("parent")).expect("mkdir");
            fs::write(&full, body).expect("write");
            index.add_path(Path::new(path)).expect("add");
        }
        index.write().expect("idx");
        let tree = repo.find_tree(index.write_tree().expect("t")).expect("tree");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");
        (Tmp(base), root)
    }

    #[test]
    fn compares_and_restores_single_files_against_head() {
        let (_tmp, root) = repo_with(&[("Cargo.lock", "v1\n"), ("migrations/001.sql", "create\n")]);
        assert!(compare_with_baseline(&root, None, "Cargo.lock").expect("cmp").unchanged);

        fs::write(root.join("Cargo.lock"), "v2\n").expect("write");
        let cmp = compare_with_baseline(&root, None, "Cargo.lock").expect("cmp");
        assert!(!cmp.unchanged && cmp.in_baseline && cmp.on_disk);
        assert!(restore_path_from_baseline(&root, None, "Cargo.lock").expect("restore"));
        assert_eq!(fs::read_to_string(root.join("Cargo.lock")).expect("r"), "v1\n");
        assert!(!restore_path_from_baseline(&root, None, "Cargo.lock").expect("noop"));

        // Borrado → se recrea; archivo nuevo → se borra (y su carpeta vacía).
        fs::remove_dir_all(root.join("migrations")).expect("rm");
        assert_eq!(baseline_files_under(&root, None, "migrations").expect("under"), ["migrations/001.sql"]);
        assert!(restore_path_from_baseline(&root, None, "migrations/001.sql").expect("restore"));
        assert_eq!(fs::read_to_string(root.join("migrations/001.sql")).expect("r"), "create\n");

        fs::create_dir_all(root.join("migrations/extra")).expect("mkdir");
        fs::write(root.join("migrations/extra/002.sql"), "drop\n").expect("write");
        assert!(restore_path_from_baseline(&root, None, "migrations/extra/002.sql").expect("restore"));
        assert!(!root.join("migrations/extra").exists());
        assert!(root.join("migrations/001.sql").exists());
    }

    #[test]
    fn lists_ignored_files_without_descending_into_ignored_dirs() {
        let (_tmp, root) = repo_with(&[(".gitignore", ".env*\nnode_modules/\n")]);
        fs::write(root.join(".env"), "SECRET=1\n").expect("write");
        fs::create_dir_all(root.join("node_modules/x")).expect("mkdir");
        fs::write(root.join("node_modules/x/.env.local"), "x").expect("write");
        assert!(is_path_ignored(&root, ".env").expect("ignored"));
        assert!(!is_path_ignored(&root, "src/main.rs").expect("not ignored"));
        assert_eq!(ignored_files(&root).expect("ignored"), [".env"]);
    }
}
