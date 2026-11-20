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

    let snap = create_snapshot(dir.path(), "session-123", "checkpoint de prueba", None).expect("snapshot");

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
    let second = create_snapshot(dir.path(), "session-123", "otro", None).expect("snapshot 2");
    assert_ne!(second.git_ref, snap.git_ref);
}

#[test]
fn snapshot_works_on_unborn_branch() {
    let dir = TempDir::new();
    let repo = Repository::init(dir.path()).expect("init");
    write(dir.path(), "a.txt", "a\n");

    let snap = create_snapshot(dir.path(), "s1", "primer snapshot", None).expect("snapshot");
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
        assert!(create_snapshot(dir.path(), bad, "m", None).is_err(), "aceptó {bad:?}");
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

#[test]
fn restore_returns_working_tree_to_exact_checkpoint_state() {
    let (dir, repo) = repo_with_commit();
    write(dir.path(), "tracked.txt", "checkpoint\n");
    write(dir.path(), "keep/untracked.md", "u1\n");
    fs::remove_file(dir.path().join("gone.txt")).expect("rm");
    write(dir.path(), "swap", "file\n"); // fichero que luego será directorio
    let snap = create_snapshot(dir.path(), "s1", "A", None).expect("snapshot");

    // Cambios posteriores que el rollback debe deshacer.
    write(dir.path(), "tracked.txt", "later\n");
    fs::write(dir.path().join("gone.txt"), "back again\n").expect("write");
    fs::remove_file(dir.path().join("keep/untracked.md")).expect("rm");
    write(dir.path(), "extra/deep/new.txt", "x\n");
    fs::remove_file(dir.path().join("swap")).expect("rm");
    write(dir.path(), "swap/inner.txt", "dir now\n");
    write(dir.path(), "secret.env", "TOKEN=changed\n"); // ignorado: no se toca

    let head_before = repo.head().expect("head").target().expect("oid");
    let index_before = fs::read(repo.path().join("index")).expect("index");

    restore_working_tree(dir.path(), &snap.git_ref).expect("restore");

    assert_eq!(fs::read_to_string(dir.path().join("tracked.txt")).expect("r"), "checkpoint\n");
    assert!(!dir.path().join("gone.txt").exists());
    assert_eq!(fs::read_to_string(dir.path().join("keep/untracked.md")).expect("r"), "u1\n");
    assert!(!dir.path().join("extra").exists(), "directorios vacíos deben podarse");
    assert_eq!(fs::read_to_string(dir.path().join("swap")).expect("r"), "file\n");
    assert_eq!(fs::read_to_string(dir.path().join("secret.env")).expect("r"), "TOKEN=changed\n");

    // El árbol resultante es idéntico al del checkpoint.
    assert_eq!(
        working_tree_id(dir.path()).expect("id"),
        checkpoint_tree_id(dir.path(), &snap.git_ref).expect("id")
    );
    assert_eq!(repo.head().expect("head").target().expect("oid"), head_before);
    assert_eq!(fs::read(repo.path().join("index")).expect("index"), index_before);
}

#[test]
fn restore_rejects_refs_outside_nude_namespace() {
    let (dir, _repo) = repo_with_commit();
    assert!(restore_working_tree(dir.path(), "refs/heads/main").is_err());
    assert!(restore_working_tree(dir.path(), "HEAD").is_err());
}

#[test]
fn restore_preserves_ignored_files_inside_a_blocking_directory() {
    let (dir, _repo) = repo_with_commit();
    write(dir.path(), "swap", "file\n");
    let snapshot = create_snapshot(dir.path(), "s1", "checkpoint", None).expect("snapshot");
    fs::remove_file(dir.path().join("swap")).expect("rm");
    write(dir.path(), "swap/secret.env", "keep me\n");

    assert!(restore_working_tree(dir.path(), &snapshot.git_ref).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("swap/secret.env")).expect("ignored file"),
        "keep me\n"
    );
}

#[test]
fn tree_ids_detect_real_changes_only() {
    let (dir, _repo) = repo_with_commit();
    let head = head_tree_id(dir.path()).expect("head").expect("some");
    assert_eq!(working_tree_id(dir.path()).expect("w"), head);

    write(dir.path(), "secret.env", "ignored\n");
    assert_eq!(working_tree_id(dir.path()).expect("w"), head, "ignorados no cuentan");

    write(dir.path(), "tracked.txt", "v2\n");
    let changed = working_tree_id(dir.path()).expect("w");
    assert_ne!(changed, head);

    // Diff contra un checkpoint previo: solo lo ocurrido desde entonces.
    let first = create_snapshot(dir.path(), "s1", "uno", None).expect("s1");
    write(dir.path(), "other.txt", "o\n");
    let second = create_snapshot(dir.path(), "s1", "dos", Some(&first.git_ref)).expect("s2");
    assert_eq!(second.files.len(), 1, "{:?}", second.files);
    assert_eq!(second.files[0].file_path, "other.txt");
}

// ------------------------------------------------------------ Fase 4: grafo

fn commit_all(repo: &Repository, message: &str) -> git2::Oid {
    let mut index = repo.index().expect("index");
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None).expect("add");
    index.update_all(["*"].iter(), None).expect("update");
    index.write().expect("write");
    let tree = repo.find_tree(index.write_tree().expect("tree")).expect("find");
    let sig = Signature::now("Test", "test@example.com").expect("sig");
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents).expect("commit")
}

fn set_identity(repo: &Repository) {
    let mut cfg = repo.config().expect("cfg");
    cfg.set_str("user.name", "Test").expect("name");
    cfg.set_str("user.email", "test@example.com").expect("email");
}

#[test]
fn history_lists_branches_commits_parents_and_status() {
    let (dir, repo) = repo_with_commit();
    let first = repo.head().expect("h").target().expect("oid");
    create_branch(dir.path(), "feature/x", None, true).expect("branch");
    write(dir.path(), "feature.txt", "f\n");
    let second = commit_all(&repo, "feature commit");

    write(dir.path(), "tracked.txt", "staged\n");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("tracked.txt")).expect("stage");
    index.write().expect("write");
    write(dir.path(), "untracked.md", "u\n");

    let history = read_history(dir.path(), GRAPH_COMMIT_LIMIT).expect("history");
    assert_eq!(history.head.branch.as_deref(), Some("feature/x"));
    assert_eq!(history.head.commit_id, Some(second.to_string()));
    let names: Vec<_> = history.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names[0], "feature/x", "la rama actual va primero");
    assert_eq!(history.branches.len(), 2);
    assert_eq!(history.commits.len(), 2);
    assert_eq!(history.commits[0].id, second.to_string());
    assert_eq!(history.commits[0].parent_ids, vec![first.to_string()]);
    assert_eq!(history.commits[0].summary, "feature commit");
    assert_eq!(history.commits[0].author_name, "Test");
    assert!(history.status.staged.iter().any(|e| e.path == "tracked.txt"));
    assert!(history.status.unstaged.iter().any(|e| e.path == "untracked.md"));
    assert!(history.operation.is_none());
}

#[test]
fn history_of_unborn_repo_is_empty() {
    let dir = TempDir::new();
    Repository::init(dir.path()).expect("init");
    write(dir.path(), "a.txt", "a");
    let history = read_history(dir.path(), 50).expect("history");
    assert!(history.head.unborn);
    assert!(history.commits.is_empty());
    assert_eq!(history.status.unstaged.len(), 1);
}

#[test]
fn checkout_is_safe_and_switches_head() {
    let (dir, repo) = repo_with_commit();
    let main = repo.head().expect("h").shorthand().expect("name").to_owned();
    create_branch(dir.path(), "prueba", None, false).expect("branch");
    assert_eq!(repo.head().expect("h").shorthand().ok(), Some(main.as_str()), "sin checkout");

    checkout_branch(dir.path(), "prueba").expect("checkout");
    write(dir.path(), "tracked.txt", "en prueba\n");
    commit_all(&repo, "cambio en prueba");

    // Cambio local que chocaría con la otra rama → error y nada se toca.
    write(dir.path(), "tracked.txt", "local sin guardar\n");
    let err = checkout_branch(dir.path(), &main).expect_err("debe negarse");
    assert!(err.to_string().contains("cambios sin commit"), "{err}");
    assert_eq!(fs::read_to_string(dir.path().join("tracked.txt")).expect("r"), "local sin guardar\n");
    assert_eq!(repo.head().expect("h").shorthand().ok(), Some("prueba"));

    // Un cambio que no choca se conserva al cambiar.
    write(dir.path(), "tracked.txt", "en prueba\n");
    write(dir.path(), "notas.md", "wip\n");
    checkout_branch(dir.path(), &main).expect("checkout main");
    let repo = Repository::open(dir.path()).expect("reopen");
    assert_eq!(repo.head().expect("h").shorthand().ok(), Some(main.as_str()));
    assert_eq!(fs::read_to_string(dir.path().join("tracked.txt")).expect("r"), "v1\n");
    assert!(dir.path().join("notas.md").exists());
}

#[test]
fn create_branch_rejects_bad_or_duplicate_names() {
    let (dir, _repo) = repo_with_commit();
    for bad in ["", "con espacio", "a..b", "-x", "x.lock", "a~b"] {
        assert!(create_branch(dir.path(), bad, None, false).is_err(), "aceptó {bad:?}");
    }
    create_branch(dir.path(), "ok", None, false).expect("ok");
    assert!(create_branch(dir.path(), "ok", None, false).is_err());
}

#[test]
fn quick_commit_includes_only_selected_files() {
    let (dir, repo) = repo_with_commit();
    set_identity(&repo);
    write(dir.path(), "tracked.txt", "v2\n");
    write(dir.path(), "other.txt", "no elegido\n");
    fs::remove_file(dir.path().join("gone.txt")).expect("rm");
    // Algo preparado de antes que no se elige: debe seguir preparado.
    write(dir.path(), "staged-before.txt", "s\n");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("staged-before.txt")).expect("stage");
    index.write().expect("write");

    let result = quick_commit(
        dir.path(),
        "Cambio visual",
        &["tracked.txt".into(), "gone.txt".into()],
    )
    .expect("commit");
    assert_eq!(result.files_committed, 2);

    let repo = Repository::open(dir.path()).expect("reopen");
    let head = repo.head().expect("h").peel_to_commit().expect("c");
    assert_eq!(head.id().to_string(), result.commit_id);
    assert_eq!(head.summary().ok().flatten(), Some("Cambio visual"));
    let tree = head.tree().expect("tree");
    assert!(tree.get_path(Path::new("gone.txt")).is_err());
    assert!(tree.get_path(Path::new("other.txt")).is_err());
    assert!(tree.get_path(Path::new("staged-before.txt")).is_err());

    let history = read_history(dir.path(), 50).expect("h");
    let staged: Vec<_> = history.status.staged.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(staged, vec!["staged-before.txt"]);
    assert!(history.status.unstaged.iter().any(|e| e.path == "other.txt"));
    assert!(!history.status.unstaged.iter().any(|e| e.path == "tracked.txt"));
}

#[test]
fn quick_commit_validates_input() {
    let (dir, repo) = repo_with_commit();
    set_identity(&repo);
    assert!(quick_commit(dir.path(), "  ", &["tracked.txt".into()]).is_err());
    assert!(quick_commit(dir.path(), "m", &[]).is_err());
    assert!(quick_commit(dir.path(), "m", &["../fuera".into()]).is_err());
    assert!(quick_commit(dir.path(), "m", &[".git/config".into()]).is_err());
    assert!(quick_commit(dir.path(), "m", &["tracked.txt".into()]).is_err(), "sin cambios");
}

#[test]
fn restore_to_base_returns_to_head_of_snapshot_time() {
    let (dir, _repo) = repo_with_commit();
    write(dir.path(), "tracked.txt", "agente\n");
    write(dir.path(), "nuevo.rs", "x\n");
    let snap = create_snapshot(dir.path(), "s1", "agente", None).expect("snap");
    assert!(checkpoint_base_commit(dir.path(), &snap.git_ref).expect("base").is_some());

    restore_working_tree_to_base(dir.path(), &snap.git_ref).expect("restore");
    assert_eq!(fs::read_to_string(dir.path().join("tracked.txt")).expect("r"), "v1\n");
    assert!(!dir.path().join("nuevo.rs").exists());
    assert_eq!(working_tree_id(dir.path()).expect("w"), head_tree_id(dir.path()).expect("h").expect("s"));
}
