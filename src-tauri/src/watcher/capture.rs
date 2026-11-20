//! Creación automática de checkpoints y rollback.
//!
//! Todo pasa por `CAPTURE_LOCK` para que el watcher y los comandos IPC nunca
//! manipulen el árbol de trabajo o las refs a la vez.

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Serialize;

use super::sessions::detect_active_session;
use crate::db::Database;
use crate::db::models::{AgentTool, AgentSession, Checkpoint, Project};
use crate::error::{NudeError, NudeResult};
use crate::git;

static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// Id del árbol vacío de Git (repositorio sin commits y sin ficheros).
const EMPTY_TREE_ID: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub(super) fn delete_project(db: &Database, project_id: &str) -> NudeResult<()> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    db.delete_project(project_id)
}

/// Guarda el estado actual incluso si el watcher ya capturó el mismo árbol.
pub fn create_manual_checkpoint(
    db: &Database,
    project_id: &str,
    summary: &str,
) -> NudeResult<Checkpoint> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let project = db
        .get_project(project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))?;
    let latest = db.latest_checkpoint(project_id)?;
    let session = db.get_or_create_session(project_id, AgentTool::Manual, None, None)?;
    let summary = summary.trim();
    let summary = if summary.is_empty() { "Checkpoint manual" } else { summary };
    save_checkpoint(db, &session, Path::new(&project.root_path), latest.as_ref(), Some(summary))
}

/// Crea un checkpoint si el árbol de trabajo difiere del último checkpoint del
/// proyecto (o de `HEAD` si todavía no hay ninguno). `None` = nada que guardar.
pub fn capture_if_changed(
    db: &Database,
    home: &Path,
    project: &Project,
    summary_override: Option<&str>,
) -> NudeResult<Option<Checkpoint>> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    capture_locked(db, home, project, summary_override)
}

fn capture_locked(
    db: &Database,
    home: &Path,
    project: &Project,
    summary_override: Option<&str>,
) -> NudeResult<Option<Checkpoint>> {
    let root = Path::new(&project.root_path);
    if !root.is_dir() {
        return Ok(None); // proyecto movido o borrado
    }

    let latest = db.latest_checkpoint(&project.id)?;
    let baseline_tree = match &latest {
        Some(cp) => git::checkpoint_tree_id(root, &cp.git_ref).ok(),
        None => None,
    };
    let baseline_tree = match baseline_tree {
        Some(id) => Some(id),
        None => git::head_tree_id(root)?,
    };

    let current_tree = git::working_tree_id(root)?;
    if baseline_tree.as_deref() == Some(current_tree.as_str())
        || (baseline_tree.is_none() && current_tree == EMPTY_TREE_ID)
    {
        return Ok(None);
    }

    let detected = detect_active_session(home, root, SystemTime::now());
    let session = db.get_or_create_session(
        &project.id,
        detected.tool,
        detected.external_id.as_deref(),
        detected.model_name.as_deref(),
    )?;

    let summary = summary_override
        .map(str::to_owned)
        .or(detected.last_prompt);
    save_checkpoint(db, &session, root, latest.as_ref(), summary.as_deref()).map(Some)
}

fn save_checkpoint(
    db: &Database,
    session: &AgentSession,
    root: &Path,
    latest: Option<&Checkpoint>,
    summary: Option<&str>,
) -> NudeResult<Checkpoint> {
    let message = summary.unwrap_or("Nude checkpoint");
    let diff_base = latest.map(|cp| cp.git_ref.as_str());
    let snapshot = git::create_snapshot(root, &session.id, message, diff_base)?;

    db.insert_checkpoint(
        &session.id,
        &session.project_id,
        &snapshot.git_ref,
        summary,
        &snapshot.files,
    )
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackResult {
    /// Checkpoint del estado previo al rollback (permite deshacerlo). `None` si
    /// el árbol ya coincidía con el último checkpoint.
    pub safety_checkpoint: Option<Checkpoint>,
    /// Checkpoint que registra el estado restaurado, si difiere del último.
    pub restored_checkpoint: Option<Checkpoint>,
    pub files_restored: usize,
}

/// Restaura el árbol de trabajo al estado exacto de `checkpoint_id`.
/// Antes guarda el estado actual como checkpoint de seguridad.
pub fn rollback_to_checkpoint(
    db: &Database,
    home: &Path,
    checkpoint_id: &str,
) -> NudeResult<RollbackResult> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let checkpoint = db
        .get_checkpoint(checkpoint_id)?
        .ok_or_else(|| NudeError::NotFound(format!("checkpoint {checkpoint_id}")))?;
    let project = db
        .get_project(&checkpoint.project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {}", checkpoint.project_id)))?;
    let root = Path::new(&project.root_path);

    // Falla antes de tocar nada si la ref ya no existe.
    git::checkpoint_tree_id(root, &checkpoint.git_ref)?;

    let short = checkpoint.created_at.as_str();
    let safety_checkpoint = capture_locked(
        db,
        home,
        &project,
        Some(&format!("Antes del rollback al checkpoint de {short}")),
    )?;
    let files_restored = git::restore_working_tree(root, &checkpoint.git_ref)?;
    let restored_checkpoint = capture_locked(
        db,
        home,
        &project,
        Some(&format!("Rollback al checkpoint de {short}")),
    )?;

    Ok(RollbackResult {
        safety_checkpoint,
        restored_checkpoint,
        files_restored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn setup() -> (Tmp, Tmp, Database, Project) {
        let base = std::env::temp_dir().join(format!("nude-cap-{}", uuid::Uuid::new_v4()));
        let root = base.join("repo");
        let home = base.join("home");
        fs::create_dir_all(&root).expect("mkdir");
        fs::create_dir_all(&home).expect("mkdir");
        let root = fs::canonicalize(&root).expect("canon");
        let repo = Repository::init(&root).expect("init");
        fs::write(root.join("a.txt"), "1\n").expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.txt")).expect("add");
        index.write().expect("write idx");
        let tree = repo.find_tree(index.write_tree().expect("tree")).expect("find");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");

        let db = Database::open_in_memory().expect("db");
        let (project, _) = db
            .upsert_project("repo", root.to_str().expect("utf8"))
            .expect("project");
        (Tmp(base), Tmp(home), db, project)
    }

    #[test]
    fn clean_tree_creates_nothing_and_changes_create_one_checkpoint() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);

        assert!(capture_if_changed(&db, &home.0, &project, None).expect("c").is_none());

        fs::write(root.join("a.txt"), "2\n").expect("write");
        fs::write(root.join("b.txt"), "new\n").expect("write");
        let cp = capture_if_changed(&db, &home.0, &project, Some("prompt de prueba"))
            .expect("c")
            .expect("checkpoint");
        assert_eq!(cp.files_changed_count, 2);
        assert_eq!(cp.prompt_summary.as_deref(), Some("prompt de prueba"));
        assert!(cp.git_ref.starts_with("refs/nude/checkpoints/"));
        assert_eq!(db.list_file_diffs(&cp.id).expect("diffs").len(), 2);

        // Sin cambios nuevos → no hay segundo checkpoint.
        assert!(capture_if_changed(&db, &home.0, &project, None).expect("c").is_none());

        // El siguiente diff es solo lo ocurrido desde el checkpoint anterior.
        fs::write(root.join("c.txt"), "x\n").expect("write");
        let cp2 = capture_if_changed(&db, &home.0, &project, None).expect("c").expect("cp2");
        let diffs = db.list_file_diffs(&cp2.id).expect("diffs");
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].file_path, "c.txt");
        drop(base);
    }

    #[test]
    fn manual_checkpoint_preserves_the_summary_even_on_an_unchanged_tree() {
        let (_base, _home, db, project) = setup();
        db.get_or_create_session(&project.id, AgentTool::Codex, Some("active"), Some("gpt"))
            .expect("agent session");
        let first = create_manual_checkpoint(&db, &project.id, "  Punto de partida  ")
            .expect("manual checkpoint");
        let second = create_manual_checkpoint(&db, &project.id, "")
            .expect("second manual checkpoint");

        assert_eq!(first.agent_tool, "manual");
        assert_eq!(first.model_name, None);
        assert_eq!(first.prompt_summary.as_deref(), Some("Punto de partida"));
        assert_eq!(second.prompt_summary.as_deref(), Some("Checkpoint manual"));
        assert_eq!(second.files_changed_count, 0);
        assert_ne!(second.git_ref, first.git_ref);
        assert_eq!(db.list_checkpoints(&project.id).expect("history"), vec![second, first]);
    }

    #[test]
    fn rollback_restores_exact_state_and_keeps_a_safety_checkpoint() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);

        fs::write(root.join("a.txt"), "estado A\n").expect("write");
        let a = capture_if_changed(&db, &home.0, &project, None).expect("c").expect("a");

        fs::write(root.join("a.txt"), "estado B\n").expect("write");
        fs::write(root.join("extra.txt"), "extra\n").expect("write");
        let b = capture_if_changed(&db, &home.0, &project, None).expect("c").expect("b");
        fs::write(root.join("sin-checkpoint.txt"), "wip\n").expect("write");

        let result = rollback_to_checkpoint(&db, &home.0, &a.id).expect("rollback");

        assert_eq!(fs::read_to_string(root.join("a.txt")).expect("r"), "estado A\n");
        assert!(!root.join("extra.txt").exists());
        assert!(!root.join("sin-checkpoint.txt").exists());
        let safety = result.safety_checkpoint.expect("safety");
        let restored = result.restored_checkpoint.expect("el estado restaurado se registra");
        assert!(restored.prompt_summary.as_deref().is_some_and(|s| s.starts_with("Rollback")));
        // Comparten árbol con A, y el checkpoint de seguridad tiene el WIP.
        let root_s = root.as_path();
        assert_eq!(
            git::checkpoint_tree_id(root_s, &restored.git_ref).expect("t"),
            git::checkpoint_tree_id(root_s, &a.git_ref).expect("t")
        );
        let _ = b;

        // El estado previo al rollback sigue recuperable.
        rollback_to_checkpoint(&db, &home.0, &safety.id).expect("undo");
        assert_eq!(fs::read_to_string(root.join("a.txt")).expect("r"), "estado B\n");
        assert_eq!(fs::read_to_string(root.join("sin-checkpoint.txt")).expect("r"), "wip\n");
        drop(base);
    }

    #[test]
    fn rollback_of_unknown_checkpoint_fails_cleanly() {
        let (base, home, db, _project) = setup();
        assert!(rollback_to_checkpoint(&db, &home.0, "nope").is_err());
        drop(base);
    }
}
