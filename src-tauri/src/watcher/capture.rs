//! Creación automática de checkpoints y rollback.
//!
//! Todo pasa por `CAPTURE_LOCK` para que el watcher y los comandos IPC nunca
//! manipulen el árbol de trabajo o las refs a la vez.

use std::path::Path;
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Serialize;

use super::sessions::{DetectedSession, detect_active_session};
use crate::db::Database;
use crate::db::models::{AgentTool, AgentSession, Checkpoint, Project};
use crate::error::{NudeError, NudeResult};
use crate::git;

static CAPTURE_LOCK: Mutex<()> = Mutex::new(());

/// El firewall de archivos restaura bajo el mismo bloqueo que las capturas.
pub(super) fn lock_capture() -> std::sync::MutexGuard<'static, ()> {
    CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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
    capture_locked(db, home, project, summary_override, SessionMode::Detect)
}

/// Cómo atribuir un checkpoint a una sesión.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionMode {
    /// Sesión de agente activa (o `manual` si no hay ninguna).
    Detect,
    /// Siempre `manual`: acciones del usuario (rollback, cambio de rama…), que
    /// no deben atribuirse al agente que esté abierto en ese momento.
    Manual,
}

fn capture_locked(
    db: &Database,
    home: &Path,
    project: &Project,
    summary_override: Option<&str>,
    mode: SessionMode,
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

    let detected = match mode {
        SessionMode::Detect => detect_active_session(home, root, SystemTime::now()),
        SessionMode::Manual => DetectedSession::manual(),
    };
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

/// Ejecuta `change` (que modifica el árbol de trabajo) bajo `CAPTURE_LOCK`,
/// guardando antes un checkpoint de seguridad y después uno del estado
/// resultante, ambos `manual`. Así ningún cambio pedido por el usuario se
/// pierde ni se atribuye al agente, y el watcher no lo captura de nuevo.
pub fn guarded_change<T>(
    db: &Database,
    home: &Path,
    project: &Project,
    before_label: &str,
    after_label: &str,
    change: impl FnOnce(&Path) -> NudeResult<T>,
) -> NudeResult<GuardedChange<T>> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    guarded_change_locked(db, home, project, before_label, after_label, change)
}

pub struct GuardedChange<T> {
    pub value: T,
    pub before: Option<Checkpoint>,
    pub after: Option<Checkpoint>,
}

fn guarded_change_locked<T>(
    db: &Database,
    home: &Path,
    project: &Project,
    before_label: &str,
    after_label: &str,
    change: impl FnOnce(&Path) -> NudeResult<T>,
) -> NudeResult<GuardedChange<T>> {
    let root = Path::new(&project.root_path);
    let before = capture_locked(db, home, project, Some(before_label), SessionMode::Manual)?;
    let value = change(root)?;
    let after = capture_locked(db, home, project, Some(after_label), SessionMode::Manual)?;
    Ok(GuardedChange { value, before, after })
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
    let project = project_of(db, &checkpoint)?;
    let root = Path::new(&project.root_path);

    // Falla antes de tocar nada si la ref ya no existe.
    git::checkpoint_tree_id(root, &checkpoint.git_ref)?;

    let short = checkpoint.created_at.as_str();
    let change = guarded_change_locked(
        db,
        home,
        &project,
        &format!("Antes del rollback al checkpoint de {short}"),
        &format!("Rollback al checkpoint de {short}"),
        |root| git::restore_working_tree(root, &checkpoint.git_ref),
    )?;
    Ok(RollbackResult {
        safety_checkpoint: change.before,
        restored_checkpoint: change.after,
        files_restored: change.value,
    })
}

fn project_of(db: &Database, checkpoint: &Checkpoint) -> NudeResult<Project> {
    db.get_project(&checkpoint.project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {}", checkpoint.project_id)))
}

// --------------------------------------------------- Deshacer último agente

/// Qué haría "Deshacer último agente" (para mostrarlo antes de confirmar).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoAgentPlan {
    pub session_id: String,
    pub agent_tool: String,
    pub model_name: Option<String>,
    /// Checkpoints de esa sesión que se descartan.
    pub session_checkpoints: usize,
    /// Checkpoints posteriores a la sesión (de cualquier tipo) que también se descartan.
    pub later_checkpoints: usize,
    pub started_at: String,
    /// Checkpoint al que se vuelve: el inmediatamente anterior a la sesión.
    pub target_checkpoint: Option<Checkpoint>,
    /// Si no hay checkpoint anterior, se vuelve al commit de `HEAD` de entonces.
    pub base_commit: Option<String>,
    /// El árbol de trabajo ya está en ese estado (no hay nada que deshacer).
    pub already_undone: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoAgentResult {
    pub plan: UndoAgentPlan,
    pub rollback: RollbackResult,
}

fn undo_plan_locked(db: &Database, project: &Project) -> NudeResult<Option<(UndoAgentPlan, Checkpoint)>> {
    let Some(session) = db.last_agent_session(&project.id)? else {
        return Ok(None);
    };
    let session_cps = db.session_checkpoints(&session.id)?;
    let Some(first) = session_cps.first().cloned() else {
        return Ok(None);
    };
    let target = db.checkpoint_before(&project.id, &first.id)?;
    let root = Path::new(&project.root_path);

    let (target_tree, base_commit) = match &target {
        Some(cp) => (git::checkpoint_tree_id(root, &cp.git_ref).ok(), None),
        None => (
            git::checkpoint_base_tree_id(root, &first.git_ref).ok().flatten(),
            git::checkpoint_base_commit(root, &first.git_ref).ok().flatten(),
        ),
    };
    let current = git::working_tree_id(root)?;
    let already_undone = target_tree.as_deref() == Some(current.as_str());

    let all = db.list_checkpoints(&project.id)?;
    let later = all
        .iter()
        .take_while(|cp| cp.id != first.id)
        .filter(|cp| cp.session_id != session.id)
        .count();

    Ok(Some((
        UndoAgentPlan {
            session_id: session.id.clone(),
            agent_tool: session.agent_tool.clone(),
            model_name: session.model_name.clone(),
            session_checkpoints: session_cps.len(),
            later_checkpoints: later,
            started_at: first.created_at.clone(),
            target_checkpoint: target,
            base_commit,
            already_undone,
        },
        first,
    )))
}

pub fn preview_undo_last_agent(db: &Database, project_id: &str) -> NudeResult<Option<UndoAgentPlan>> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let project = db
        .get_project(project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))?;
    Ok(undo_plan_locked(db, &project)?.map(|(plan, _)| plan))
}

/// Vuelve al checkpoint inmediatamente anterior a la última sesión de IA (o,
/// si no lo hay, al commit sobre el que empezó). Reversible: guarda antes un
/// checkpoint de seguridad.
pub fn undo_last_agent(db: &Database, home: &Path, project_id: &str) -> NudeResult<UndoAgentResult> {
    let _guard = CAPTURE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let project = db
        .get_project(project_id)?
        .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))?;
    let (plan, first) = undo_plan_locked(db, &project)?.ok_or_else(|| {
        NudeError::NotFound("ninguna sesión de agente con checkpoints en este proyecto".into())
    })?;

    let agent = plan.agent_tool.clone();
    let change = guarded_change_locked(
        db,
        home,
        &project,
        &format!("Antes de deshacer la sesión de {agent}"),
        &format!("Deshecha la sesión de {agent}"),
        |root| match &plan.target_checkpoint {
            Some(cp) => git::restore_working_tree(root, &cp.git_ref),
            None => git::restore_working_tree_to_base(root, &first.git_ref),
        },
    )?;
    Ok(UndoAgentResult {
        plan,
        rollback: RollbackResult {
            safety_checkpoint: change.before,
            restored_checkpoint: change.after,
            files_restored: change.value,
        },
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

    fn agent_session(db: &Database, project: &Project, ext: &str) -> crate::db::models::AgentSession {
        db.get_or_create_session(&project.id, AgentTool::ClaudeCode, Some(ext), Some("claude-opus-5-5"))
            .expect("session")
    }

    #[test]
    fn undo_last_agent_returns_to_checkpoint_before_the_session() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);

        // Estado previo del usuario (manual).
        fs::write(root.join("a.txt"), "mío\n").expect("w");
        let before = create_manual_checkpoint(&db, &project.id, "antes del agente").expect("cp");

        // Sesión de agente con dos checkpoints.
        let session = agent_session(&db, &project, "s1");
        for (i, body) in ["agente 1\n", "agente 2\n"].iter().enumerate() {
            fs::write(root.join("a.txt"), body).expect("w");
            fs::write(root.join(format!("gen{i}.rs")), "x\n").expect("w");
            let latest = db.latest_checkpoint(&project.id).expect("l");
            save_checkpoint(&db, &session, &root, latest.as_ref(), Some("prompt")).expect("cp");
        }

        let plan = preview_undo_last_agent(&db, &project.id).expect("plan").expect("some");
        assert_eq!(plan.agent_tool, "claude-code");
        assert_eq!(plan.session_checkpoints, 2);
        assert_eq!(plan.target_checkpoint.as_ref().map(|c| c.id.as_str()), Some(before.id.as_str()));
        assert!(!plan.already_undone);

        let result = undo_last_agent(&db, &home.0, &project.id).expect("undo");
        assert_eq!(fs::read_to_string(root.join("a.txt")).expect("r"), "mío\n");
        assert!(!root.join("gen0.rs").exists() && !root.join("gen1.rs").exists());
        // Los checkpoints generados por el deshacer son manuales, no del agente.
        let restored = result.rollback.restored_checkpoint.expect("restored");
        assert_eq!(restored.agent_tool, "manual");

        // Idempotente: ya está deshecho.
        let again = preview_undo_last_agent(&db, &project.id).expect("p").expect("s");
        assert!(again.already_undone);
        drop(base);
    }

    #[test]
    fn undo_without_previous_checkpoint_goes_back_to_base_commit() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);
        let session = agent_session(&db, &project, "s1");
        fs::write(root.join("a.txt"), "agente\n").expect("w");
        save_checkpoint(&db, &session, &root, None, Some("prompt")).expect("cp");

        let plan = preview_undo_last_agent(&db, &project.id).expect("p").expect("s");
        assert!(plan.target_checkpoint.is_none());
        assert!(plan.base_commit.is_some());

        undo_last_agent(&db, &home.0, &project.id).expect("undo");
        assert_eq!(fs::read_to_string(root.join("a.txt")).expect("r"), "1\n");
        drop(base);
    }

    #[test]
    fn no_agent_sessions_means_nothing_to_undo() {
        let (base, home, db, project) = setup();
        assert!(preview_undo_last_agent(&db, &project.id).expect("p").is_none());
        assert!(undo_last_agent(&db, &home.0, &project.id).is_err());
        drop(base);
    }

    #[test]
    fn guarded_change_records_manual_checkpoints_around_a_branch_switch() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);
        fs::write(root.join("wip.txt"), "wip\n").expect("w");
        let change = guarded_change(&db, &home.0, &project, "antes", "después", |root| {
            git::create_branch(root, "prueba", None, true)
        })
        .expect("change");
        assert_eq!(change.value, "prueba");
        let before = change.before.expect("before");
        assert_eq!(before.agent_tool, "manual");
        assert!(change.after.is_none(), "el árbol no cambió al crear la rama");
        drop(base);
    }

    #[test]
    fn merging_a_claude_worktree_is_attributed_to_claude_with_model_and_prompt() {
        let (base, home, db, project) = setup();
        let root = PathBuf::from(&project.root_path);
        let worktree = root.join(".claude/worktrees/opus-tarea");
        let dir = home.0.join(".claude/projects").join(super::super::sessions::claude_project_dir_name(&worktree));
        fs::create_dir_all(&dir).expect("mkdir");
        let lines = [
            format!(r#"{{"type":"user","cwd":"{}","message":{{"role":"user","content":"fusiona la tarea en main"}}}}"#, worktree.display()),
            format!(r#"{{"type":"assistant","cwd":"{}","message":{{"model":"claude-opus-5-5","content":[]}}}}"#, worktree.display()),
        ];
        fs::write(dir.join("wt.jsonl"), lines.join("\n")).expect("session");

        // El merge del worktree cambia el árbol del proyecto.
        fs::write(root.join("a.txt"), "cambio fusionado\n").expect("write");
        let cp = capture_if_changed(&db, &home.0, &project, None).expect("capture").expect("checkpoint");
        assert_eq!(cp.agent_tool, "claude-code");
        assert_eq!(cp.model_name.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(cp.prompt_summary.as_deref(), Some("fusiona la tarea en main"));
        drop(base);
    }
}
