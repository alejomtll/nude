//! Ciclo completo de carriles paralelos con un repositorio Git real temporal:
//! dos carriles editan el mismo archivo a la vez, uno se aplica a `main` y el
//! otro se descarta, sin dejar carpetas ni ramas.

use std::fs;
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, RepositoryInitOptions, Signature};

use super::*;
use crate::pty::PtyManager;
use crate::watcher;

struct Sandbox(PathBuf);

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Repo con `main` y un commit inicial con `app.txt`.
fn repo_with_main(base: &Path) -> (PathBuf, Repository) {
    let root = base.join("repo");
    fs::create_dir_all(&root).expect("mkdir");
    let root = fs::canonicalize(&root).expect("canon");
    let mut opts = RepositoryInitOptions::new();
    opts.initial_head("main");
    let repo = Repository::init_opts(&root, &opts).expect("init");
    {
        let mut config = repo.config().expect("config");
        config.set_str("user.name", "Tester").expect("name");
        config.set_str("user.email", "tester@example.com").expect("email");
    }
    fs::write(root.join("app.txt"), "línea 1\n").expect("write");
    fs::write(root.join("README.md"), "# Demo\n").expect("write");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("app.txt")).expect("add");
    index.add_path(Path::new("README.md")).expect("add");
    index.write().expect("index write");
    let tree = repo.find_tree(index.write_tree().expect("tree")).expect("tree");
    let sig = Signature::now("Tester", "tester@example.com").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "inicio", &tree, &[]).expect("commit");
    drop(tree);
    (root, repo)
}

fn head_id(repo: &Repository) -> git2::Oid {
    repo.head().expect("head").peel_to_commit().expect("commit").id()
}

fn lane_branches(repo: &Repository) -> Vec<String> {
    repo.branches(Some(BranchType::Local))
        .expect("branches")
        .flatten()
        .filter_map(|(b, _)| b.name().ok().flatten().map(str::to_owned))
        .filter(|name| name.starts_with("nude/lane-"))
        .collect()
}

#[test]
fn two_parallel_lanes_edit_the_same_file_one_is_merged_and_the_other_discarded() {
    let sandbox = Sandbox(std::env::temp_dir().join(format!("nude-lanes-{}", uuid::Uuid::new_v4())));
    let (root, repo) = repo_with_main(&sandbox.0);
    let home = sandbox.0.join("home");
    let lanes_base = sandbox.0.join("Application Support/com.nude.app/worktrees");

    let db = Database::open_in_memory().expect("db");
    let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("project");
    let watcher = watcher::start(db.clone(), home.clone(), vec![project.clone()], |_| {});
    let main_before = head_id(&repo);

    // 1. Dos carriles desde el mismo HEAD.
    let a = create_lane(&db, &watcher, &lanes_base, &project.id, "Agente A", Some("claude-code")).expect("lane A");
    let b = create_lane(&db, &watcher, &lanes_base, &project.id, "Agente B", Some("codex")).expect("lane B");
    assert_eq!(a.branch_name, "nude/lane-agente-a");
    assert_eq!(b.branch_name, "nude/lane-agente-b");
    let lane_dir = fs::canonicalize(&lanes_base).expect("base").join(&project.id);
    assert_eq!(Path::new(&a.worktree_path), lane_dir.join("agente-a"));
    assert_eq!(fs::read_to_string(Path::new(&a.worktree_path).join("app.txt")).expect("checkout"), "línea 1\n");
    assert_eq!(db.list_projects().expect("projects"), vec![project.clone()], "los carriles no son proyectos visibles");
    let inner_a = db.lane_project(&a).expect("lane project").expect("watched lane");
    assert_eq!(watcher.status(&inner_a.id).project_id, inner_a.id);

    // 2. Cada agente cambia el mismo archivo con contenido distinto.
    fs::write(Path::new(&a.worktree_path).join("app.txt"), "línea 1\nversión del agente A\n").expect("write A");
    fs::write(Path::new(&b.worktree_path).join("app.txt"), "versión del agente B\n").expect("write B");
    fs::write(Path::new(&b.worktree_path).join("extra.txt"), "solo en B\n").expect("write B extra");

    let lanes = list_lanes(&db, &home, &PtyManager::new(|_| {}), &project.id).expect("list");
    assert_eq!(lanes.len(), 2);
    let files = |s: &LaneSummary| s.files.iter().map(|f| f.file_path.clone()).collect::<Vec<_>>();
    assert_eq!(files(&lanes[0]), ["app.txt"]);
    assert_eq!(files(&lanes[1]), ["app.txt", "extra.txt"]);
    assert!(lanes.iter().all(|l| l.conflicts.is_empty()), "aislados: ninguno choca con main todavía");
    assert_eq!(lanes[0].base_branch.as_deref(), Some("main"));
    let diff = lane_diff(&db, &a.id).expect("diff");
    assert!(diff.diff.files[0].patch_text.as_deref().is_some_and(|p| p.contains("+versión del agente A")));

    // El repositorio principal no se ha enterado de nada.
    assert_eq!(head_id(&repo), main_before);
    assert_eq!(fs::read_to_string(root.join("app.txt")).expect("main"), "línea 1\n");
    assert!(repo.statuses(None).expect("status").is_empty());

    // 3. Se aplica A a main con un merge --no-ff.
    let merged = merge_lane(&db, &watcher, &PtyManager::new(|_| {}), &home, &lanes_base, &a.id, false).expect("merge A");
    assert!(merged.merged, "{merged:?}");
    assert_eq!(merged.branch.as_deref(), Some("main"));
    assert_eq!(merged.cleanup_error, None);
    let head = repo.head().expect("head").peel_to_commit().expect("commit");
    assert_eq!(head.id().to_string(), merged.commit_id.clone().expect("commit id"));
    assert_eq!(head.parent_count(), 2, "merge --no-ff");
    assert_eq!(head.parent_id(0).expect("p0"), main_before);
    assert_eq!(fs::read_to_string(root.join("app.txt")).expect("main"), "línea 1\nversión del agente A\n");
    assert!(repo.statuses(None).expect("status").is_empty(), "árbol e índice al día con el merge");
    assert!(!Path::new(&a.worktree_path).exists(), "worktree de A borrado");
    assert_eq!(db.get_lane(&a.id).expect("lane").map(|l| l.status), Some(LaneStatus::Merged));
    assert!(db.get_project(&inner_a.id).expect("inner").is_none(), "ya no se vigila");

    // B tocó lo mismo: ahora choca y no se aplica sin confirmación.
    let lanes = list_lanes(&db, &home, &PtyManager::new(|_| {}), &project.id).expect("list");
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].conflicts, ["app.txt"]);
    let refused = merge_lane(&db, &watcher, &PtyManager::new(|_| {}), &home, &lanes_base, &b.id, false).expect("merge B");
    assert!(!refused.merged);
    assert_eq!(refused.conflicts, ["app.txt"]);
    assert!(refused.checkpoints.is_empty(), "sin confirmar no se toca nada");
    assert_eq!(head_id(&repo), head.id());

    // 4. Se descarta B (con su agente aún en marcha en la terminal): sin
    // proceso, carpetas ni ramas, y main intacto.
    let pty = PtyManager::new(|_| {});
    let mut agent = portable_pty::CommandBuilder::new("/bin/sh");
    agent.args(["-c", "sleep 30"]);
    agent.cwd(&b.worktree_path);
    let session = pty
        .spawn_command(&b.id, crate::pty::LaneProgram::Codex, agent, 80, 24)
        .expect("terminal de B");
    let pid = session.status.pid.expect("pid") as i32;
    assert!(list_lanes(&db, &home, &pty, &project.id).expect("list")[0].terminal.as_ref().is_some_and(|t| t.running));
    let discarded = discard_lane(&db, &watcher, &pty, &lanes_base, &b.id).expect("discard B");
    assert!(pty.status(&b.id).is_none(), "terminal cerrada al descartar");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    // SAFETY: la señal 0 solo comprueba si el proceso existe.
    while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(unsafe { libc::kill(pid, 0) } != 0, "el agente del carril sigue vivo");
    assert_eq!(discarded.status, LaneStatus::Discarded);
    assert!(!Path::new(&b.worktree_path).exists());
    assert!(!lane_dir.exists(), "carpeta del proyecto en worktrees/ eliminada");
    assert!(lane_branches(&repo).is_empty(), "ramas nude/lane-* eliminadas");
    assert!(repo.worktrees().expect("worktrees").is_empty(), ".git/worktrees limpio");
    assert_eq!(head_id(&repo), head.id());
    assert_eq!(fs::read_to_string(root.join("app.txt")).expect("main"), "línea 1\nversión del agente A\n");
    assert!(!root.join("extra.txt").exists());
    assert!(list_lanes(&db, &home, &PtyManager::new(|_| {}), &project.id).expect("list").is_empty());
    assert!(discard_lane(&db, &watcher, &PtyManager::new(|_| {}), &lanes_base, &b.id).is_err(), "un carril cerrado no se cierra dos veces");
}

#[test]
fn applying_a_lane_never_overwrites_unsaved_work_and_conflicts_can_take_the_lane_side() {
    let sandbox = Sandbox(std::env::temp_dir().join(format!("nude-lanes-{}", uuid::Uuid::new_v4())));
    let (root, repo) = repo_with_main(&sandbox.0);
    let home = sandbox.0.join("home");
    let lanes_base = sandbox.0.join("worktrees");
    let db = Database::open_in_memory().expect("db");
    let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("project");
    let watcher = watcher::start(db.clone(), home.clone(), vec![project.clone()], |_| {});

    let lane = create_lane(&db, &watcher, &lanes_base, &project.id, "", None).expect("lane");
    assert_eq!(lane.name, "Carril 1", "nombre por defecto");
    fs::write(Path::new(&lane.worktree_path).join("app.txt"), "del carril\n").expect("write lane");

    // Cambio local sin guardar en main sobre el mismo archivo: no se pisa.
    fs::write(root.join("app.txt"), "sin guardar en main\n").expect("dirty main");
    let before = head_id(&repo);
    let err = merge_lane(&db, &watcher, &PtyManager::new(|_| {}), &home, &lanes_base, &lane.id, false).expect_err("checkout seguro");
    assert!(err.to_string().contains("cambios sin guardar"), "{err}");
    assert_eq!(head_id(&repo), before, "HEAD no se movió");
    assert_eq!(fs::read_to_string(root.join("app.txt")).expect("main"), "sin guardar en main\n");
    assert!(Path::new(&lane.worktree_path).exists(), "el carril sigue disponible");

    // Main avanza con un commit que choca; confirmar = quedarse con el carril.
    fs::write(root.join("app.txt"), "commit en main\n").expect("write");
    let mut index = repo.index().expect("index");
    index.add_path(Path::new("app.txt")).expect("add");
    index.write().expect("write index");
    let tree = repo.find_tree(index.write_tree().expect("tree")).expect("tree");
    let parent = repo.head().expect("head").peel_to_commit().expect("parent");
    let sig = Signature::now("Tester", "tester@example.com").expect("sig");
    repo.commit(Some("HEAD"), &sig, &sig, "main avanza", &tree, &[&parent]).expect("commit");

    let refused = merge_lane(&db, &watcher, &PtyManager::new(|_| {}), &home, &lanes_base, &lane.id, false).expect("preview");
    assert_eq!(refused.conflicts, ["app.txt"]);
    let merged = merge_lane(&db, &watcher, &PtyManager::new(|_| {}), &home, &lanes_base, &lane.id, true).expect("resolve");
    assert!(merged.merged, "{merged:?}");
    assert_eq!(fs::read_to_string(root.join("app.txt")).expect("main"), "del carril\n");
    assert!(repo.statuses(None).expect("status").is_empty());
    assert!(lane_branches(&repo).is_empty());
    assert!(!Path::new(&lane.worktree_path).exists());
}

#[test]
fn a_lane_whose_folder_was_deleted_by_hand_can_still_be_discarded() {
    let sandbox = Sandbox(std::env::temp_dir().join(format!("nude-lanes-{}", uuid::Uuid::new_v4())));
    let (root, repo) = repo_with_main(&sandbox.0);
    let home = sandbox.0.join("home");
    let lanes_base = sandbox.0.join("worktrees");
    let db = Database::open_in_memory().expect("db");
    let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("project");
    let watcher = watcher::start(db.clone(), home.clone(), vec![project.clone()], |_| {});

    let lane = create_lane(&db, &watcher, &lanes_base, &project.id, "Borrado a mano", None).expect("lane");
    fs::remove_dir_all(&lane.worktree_path).expect("rm");
    let lanes = list_lanes(&db, &home, &PtyManager::new(|_| {}), &project.id).expect("list");
    assert!(lanes[0].error.is_some(), "la columna muestra el error");

    discard_lane(&db, &watcher, &PtyManager::new(|_| {}), &lanes_base, &lane.id).expect("discard");
    assert!(lane_branches(&repo).is_empty());
    assert!(repo.worktrees().expect("worktrees").is_empty());
    assert!(list_lanes(&db, &home, &PtyManager::new(|_| {}), &project.id).expect("list").is_empty());
}
