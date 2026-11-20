use std::fs;
use std::path::{Path, PathBuf};

use git2::{Repository, Signature};

use super::*;
use crate::db::models::ChangeType;

/// Directorio temporal que se borra al salir del test.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("nude-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("tmp dir");
        Self(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, contents).expect("write");
}

/// Repo con un commit inicial: `tracked.txt`, `gone.txt`, `.gitignore`.
fn repo_with_commit() -> (TempDir, Repository) {
    let dir = TempDir::new();
    let repo = Repository::init(dir.path()).expect("init");
    write(dir.path(), "tracked.txt", "v1\n");
    write(dir.path(), "gone.txt", "bye\n");
    write(dir.path(), ".gitignore", "secret.env\n");
    {
        let mut index = repo.index().expect("index");
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .expect("add");
        index.write().expect("write index");
        let tree_id = index.write_tree().expect("tree");
        let tree = repo.find_tree(tree_id).expect("find tree");
        let sig = Signature::now("Test", "test@example.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .expect("commit");
    }
    (dir, repo)
}

fn find<'a>(files: &'a [crate::db::models::FileDiff], path: &str) -> Option<&'a crate::db::models::FileDiff> {
    files.iter().find(|f| f.file_path == path)
}

#[test]
fn register_repository_resolves_root_from_subdirectory() {
    let (dir, _repo) = repo_with_commit();
    write(dir.path(), "nested/deep/file.txt", "x");
    let info = register_repository(&dir.path().join("nested/deep")).expect("register");
    let canonical = fs::canonicalize(dir.path()).expect("canon");
    assert_eq!(info.root_path, canonical.to_str().expect("utf8"));
    assert_eq!(info.name, canonical.file_name().and_then(|n| n.to_str()).expect("name"));
}

#[test]
fn register_repository_rejects_non_repo() {
    let dir = TempDir::new();
    assert!(register_repository(dir.path()).is_err());
}

#[test]
fn working_tree_diff_reports_modified_deleted_and_untracked() {
    let (dir, _repo) = repo_with_commit();
    write(dir.path(), "tracked.txt", "v2\n");
    fs::remove_file(dir.path().join("gone.txt")).expect("rm");
    write(dir.path(), "new/file.rs", "fn main() {}\n");
    write(dir.path(), "secret.env", "TOKEN=x\n");

    let files = working_tree_diff(dir.path()).expect("diff");

    let tracked = find(&files, "tracked.txt").expect("tracked");
    assert_eq!(tracked.change_type, ChangeType::Modified);
    let patch = tracked.patch_text.as_deref().expect("patch");
    assert!(patch.contains("-v1") && patch.contains("+v2"), "{patch}");

    assert_eq!(find(&files, "gone.txt").expect("gone").change_type, ChangeType::Deleted);
    assert_eq!(find(&files, "new/file.rs").expect("new").change_type, ChangeType::Added);
    assert!(find(&files, "secret.env").is_none(), "los ignorados no deben aparecer");
    assert_eq!(files.len(), 3);
}

#[test]
fn snapshot_does_not_touch_head_branch_or_index() {
    let (dir, repo) = repo_with_commit();

    // Estado del usuario: un cambio en staging y otros sin stage.
    write(dir.path(), "tracked.txt", "staged\n");
    {
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("tracked.txt")).expect("stage");
        index.write().expect("write");
    }
    write(dir.path(), "tracked.txt", "staged + unstaged\n");
    fs::remove_file(dir.path().join("gone.txt")).expect("rm");
    write(dir.path(), "untracked.md", "hola\n");
    write(dir.path(), "secret.env", "TOKEN=x\n");

    let status_before = porcelain(&repo);
    let index_path = repo.path().join("index");
    let index_before = fs::read(&index_path).expect("read index");
    let head_before = fs::read_to_string(repo.path().join("HEAD")).expect("HEAD");
    let head_oid_before = repo.head().expect("head").target().expect("oid");

    let snap = create_snapshot(dir.path(), "session-123", "checkpoint de prueba").expect("snapshot");

    // Nada del usuario cambia.
    assert_eq!(fs::read(&index_path).expect("index"), index_before, ".git/index modificado");
    assert_eq!(fs::read_to_string(repo.path().join("HEAD")).expect("HEAD"), head_before);
    let repo = Repository::open(dir.path()).expect("reopen");
    assert_eq!(repo.head().expect("head").target().expect("oid"), head_oid_before);
    assert_eq!(porcelain(&repo), status_before);
    assert_eq!(fs::read_to_string(dir.path().join("tracked.txt")).expect("f"), "staged + unstaged\n");

    // La ref interna existe y apunta a un commit hijo de HEAD.
    assert!(snap.git_ref.starts_with("refs/nude/checkpoints/session-123/"), "{}", snap.git_ref);
    let reference = repo.find_reference(&snap.git_ref).expect("ref");
    let commit = reference.peel_to_commit().expect("commit");
    assert_eq!(commit.id().to_string(), snap.commit_id);
    assert_eq!(commit.parent_id(0).expect("parent"), head_oid_before);

    // El árbol captura el working tree real.
    let tree = commit.tree().expect("tree");
    let blob_text = |name: &str| -> Option<String> {
        let entry = tree.get_path(Path::new(name)).ok()?;
        let blob = repo.find_blob(entry.id()).ok()?;
        Some(String::from_utf8_lossy(blob.content()).into_owned())
    };
    assert_eq!(blob_text("tracked.txt").as_deref(), Some("staged + unstaged\n"));
    assert_eq!(blob_text("untracked.md").as_deref(), Some("hola\n"));
    assert_eq!(blob_text("gone.txt"), None);
    assert_eq!(blob_text("secret.env"), None);

    // Diff del snapshot respecto a HEAD.
    assert_eq!(snap.files.len(), 3, "{:?}", snap.files);
    assert_eq!(find(&snap.files, "tracked.txt").expect("t").change_type, ChangeType::Modified);
    assert_eq!(find(&snap.files, "gone.txt").expect("g").change_type, ChangeType::Deleted);
    assert_eq!(find(&snap.files, "untracked.md").expect("u").change_type, ChangeType::Added);

    // Dos snapshots seguidos generan refs distintas.
    let second = create_snapshot(dir.path(), "session-123", "otro").expect("snapshot 2");
    assert_ne!(second.git_ref, snap.git_ref);
}

#[test]
fn snapshot_works_on_unborn_branch() {
    let dir = TempDir::new();
    let repo = Repository::init(dir.path()).expect("init");
    write(dir.path(), "a.txt", "a\n");

    let snap = create_snapshot(dir.path(), "s1", "primer snapshot").expect("snapshot");
    let commit = repo
        .find_reference(&snap.git_ref)
        .expect("ref")
        .peel_to_commit()
        .expect("commit");
    assert_eq!(commit.parent_count(), 0);
    assert!(repo.head().is_err(), "HEAD debe seguir sin nacer");
    assert!(!repo.path().join("index").exists(), "no debe crearse .git/index");
    assert_eq!(snap.files.len(), 1);
    assert_eq!(snap.files[0].change_type, ChangeType::Added);
}

#[test]
fn snapshot_rejects_unsafe_session_ids() {
    let (dir, _repo) = repo_with_commit();
    for bad in ["", "../x", "a/b", "a b", ".hidden", "x.lock", "a..b", "ñ"] {
        assert!(create_snapshot(dir.path(), bad, "m").is_err(), "aceptó {bad:?}");
    }
}

fn porcelain(repo: &Repository) -> Vec<(String, git2::Status)> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    let statuses = repo.statuses(Some(&mut opts)).expect("status");
    let mut out: Vec<_> = statuses
        .iter()
        .map(|s| (s.path().unwrap_or_default().to_owned(), s.status()))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}
