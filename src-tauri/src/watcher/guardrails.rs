//! Firewall de archivos (`project_guardrails`).
//!
//! Cuando un agente toca una ruta protegida, el watcher lo detecta en
//! `GUARD_DEBOUNCE` (sin esperar al debounce de los checkpoints), emite
//! `guardrail-triggered` y, con `auto_revert`, deja el archivo como en el
//! último snapshot:
//!
//! - Archivos que Git versiona o podría versionar: el árbol del último
//!   checkpoint del proyecto (o `HEAD` si no hay ninguno).
//! - Archivos ignorados por `.gitignore` (típicamente `.env*`): no tienen
//!   snapshot Git, así que el watcher guarda una copia **solo en memoria** de los
//!   que están protegidos. Nunca se escribe en disco, en SQLite ni en `.git`.
//!
//! Los cambios que no son de un agente (sin sesión escrita en los últimos
//! `AGENT_WINDOW`) no se bloquean: actualizan la copia en memoria y se
//! registran como siempre en el siguiente checkpoint.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Duration;

use git2::{ObjectType, Oid};
use serde::Serialize;

use super::capture::lock_capture;
use super::sessions::DetectedSession;
use crate::db::Database;
use crate::db::models::{AgentTool, ChangeType, Guardrail, GuardrailAction, Project, now_rfc3339};
use crate::error::NudeResult;
use crate::git;
use crate::safety::guardrail_matches;

/// Evento Tauri (payload: `GuardrailAlert`).
pub const EVENT_GUARDRAIL_TRIGGERED: &str = "guardrail-triggered";
/// Espera tras el último evento de una ruta protegida (deja terminar la escritura).
pub const GUARD_DEBOUNCE: Duration = Duration::from_millis(250);
/// Tope si la ruta no deja de cambiar.
pub const GUARD_MAX_WAIT: Duration = Duration::from_secs(1);
/// Un cambio es "del agente" si su sesión se escribió en este intervalo.
pub const AGENT_WINDOW: Duration = Duration::from_secs(120);

const SHADOW_MAX_BYTES: u64 = 1024 * 1024;
const SHADOW_MAX_FILES: usize = 256;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardrailViolation {
    pub file_path: String,
    pub pattern: String,
    pub action: GuardrailAction,
    pub change_type: ChangeType,
    /// El archivo volvió al último snapshot.
    pub reverted: bool,
    /// Por qué no se pudo restaurar (si `action` era `auto_revert`).
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GuardrailAlert {
    pub project_id: String,
    pub project_name: String,
    pub agent_tool: String,
    pub model_name: Option<String>,
    pub detected_at: String,
    pub violations: Vec<GuardrailViolation>,
}

/// Estado del firewall de un proyecto (vive en el hilo del watcher).
#[derive(Default)]
pub(super) struct GuardState {
    /// Copia en memoria de los archivos protegidos ignorados por Git.
    shadow: HashMap<String, Vec<u8>>,
    /// Ignorados protegidos que existían pero no caben en la copia.
    unknown: HashSet<String>,
    /// Huella ya notificada por archivo (`None` = borrado), para no repetir avisos.
    reported: HashMap<String, Option<Oid>>,
}

impl GuardState {
    /// Copia los archivos ignorados que protegen las reglas actuales. Se llama
    /// al empezar a vigilar y cada vez que cambian las reglas.
    pub(super) fn prime(&mut self, db: &Database, project: &Project) {
        self.shadow.clear();
        self.unknown.clear();
        self.reported.clear();
        let root = Path::new(&project.root_path);
        let rules = match db.list_guardrails(&project.id) {
            Ok(rules) if !rules.is_empty() => rules,
            Ok(_) => return,
            Err(e) => {
                eprintln!("nude: no se pudieron leer los guardrails de {}: {e}", project.name);
                return;
            }
        };
        let files = match git::ignored_files(root) {
            Ok(files) => files,
            Err(e) => {
                eprintln!("nude: no se pudieron listar los ignorados de {}: {e}", project.name);
                return;
            }
        };
        for rel in files.into_iter().filter(|rel| matching_rule(&rules, rel).is_some()) {
            match read_small(&root.join(&rel)) {
                Some(bytes) if self.shadow.len() < SHADOW_MAX_FILES => {
                    self.shadow.insert(rel, bytes);
                }
                _ => {
                    self.unknown.insert(rel);
                }
            }
        }
    }
}

fn read_small(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > SHADOW_MAX_BYTES {
        return None;
    }
    fs::read(path).ok()
}

/// Regla aplicable a `rel`; si varias coinciden gana `auto_revert`.
fn matching_rule<'a>(rules: &'a [Guardrail], rel: &str) -> Option<&'a Guardrail> {
    let mut found: Option<&Guardrail> = None;
    for rule in rules.iter().filter(|r| guardrail_matches(&r.pattern, rel)) {
        if rule.action == GuardrailAction::AutoRevert {
            return Some(rule);
        }
        found = found.or(Some(rule));
    }
    found
}

fn change_type(before: bool, after: bool) -> ChangeType {
    match (before, after) {
        (false, _) => ChangeType::Added,
        (true, false) => ChangeType::Deleted,
        (true, true) => ChangeType::Modified,
    }
}

/// Revisa las rutas cambiadas (relativas a la raíz, con `/`). `detect` solo se
/// llama si alguna ruta está protegida. Devuelve la alerta si hubo infracciones.
pub(super) fn enforce(
    db: &Database,
    project: &Project,
    paths: impl IntoIterator<Item = String>,
    state: &mut GuardState,
    detect: impl FnOnce() -> DetectedSession,
) -> NudeResult<Option<GuardrailAlert>> {
    let rules = db.list_guardrails(&project.id)?;
    let root = Path::new(&project.root_path);
    if rules.is_empty() || !root.is_dir() {
        return Ok(None);
    }

    let _guard = lock_capture();
    let latest = db.latest_checkpoint(&project.id)?;
    // Si la ref del checkpoint ya no existe, se compara con `HEAD`.
    let baseline = latest
        .as_ref()
        .map(|cp| cp.git_ref.as_str())
        .filter(|r| git::checkpoint_tree_id(root, r).is_ok());

    // Si desapareció una carpeta entera, el sistema puede avisar solo de ella.
    let mut candidates = BTreeSet::new();
    for rel in paths {
        if fs::symlink_metadata(root.join(&rel)).is_err() {
            candidates.extend(git::baseline_files_under(root, baseline, &rel).unwrap_or_default());
            let prefix = format!("{rel}/");
            candidates.extend(state.shadow.keys().filter(|k| k.starts_with(&prefix)).cloned());
        }
        candidates.insert(rel);
    }
    let protected: Vec<(String, &Guardrail)> = candidates
        .into_iter()
        .filter_map(|rel| matching_rule(&rules, &rel).map(|rule| (rel, rule)))
        .collect();
    if protected.is_empty() {
        return Ok(None);
    }

    let session = detect();
    let by_agent = session.tool != AgentTool::Manual;
    let mut violations = Vec::new();
    for (rel, rule) in protected {
        let ignored = git::is_path_ignored(root, &rel).unwrap_or(false)
            || state.shadow.contains_key(&rel)
            || state.unknown.contains(&rel);
        let outcome = if ignored {
            check_ignored(root, &rel, rule, by_agent, state)
        } else {
            check_tracked(root, baseline, &rel, rule, by_agent, state)?
        };
        violations.extend(outcome);
    }

    if violations.is_empty() {
        return Ok(None);
    }
    Ok(Some(GuardrailAlert {
        project_id: project.id.clone(),
        project_name: project.name.clone(),
        agent_tool: session.tool.as_str().to_owned(),
        model_name: session.model_name,
        detected_at: now_rfc3339(),
        violations,
    }))
}

fn check_tracked(
    root: &Path,
    baseline: Option<&str>,
    rel: &str,
    rule: &Guardrail,
    by_agent: bool,
    state: &mut GuardState,
) -> NudeResult<Option<GuardrailViolation>> {
    let cmp = git::compare_with_baseline(root, baseline, rel)?;
    if cmp.unchanged || !by_agent {
        // Sin cambio real, o es del usuario: el checkpoint normal lo registra.
        state.reported.remove(rel);
        return Ok(None);
    }
    if state.reported.get(rel) == Some(&cmp.current_id) {
        return Ok(None); // ya avisado de este mismo contenido
    }
    let change_type = change_type(cmp.in_baseline, cmp.on_disk);
    let (reverted, note) = match rule.action {
        GuardrailAction::Warn => (false, None),
        GuardrailAction::AutoRevert => match git::restore_path_from_baseline(root, baseline, rel) {
            Ok(_) => (true, None),
            Err(e) => (false, Some(format!("no se pudo restaurar: {e}"))),
        },
    };
    if reverted {
        state.reported.remove(rel);
    } else {
        state.reported.insert(rel.to_owned(), cmp.current_id);
    }
    Ok(Some(GuardrailViolation {
        file_path: rel.to_owned(),
        pattern: rule.pattern.clone(),
        action: rule.action,
        change_type,
        reverted,
        note,
    }))
}

fn check_ignored(
    root: &Path,
    rel: &str,
    rule: &Guardrail,
    by_agent: bool,
    state: &mut GuardState,
) -> Option<GuardrailViolation> {
    let path = root.join(rel);
    let on_disk = fs::symlink_metadata(&path).is_ok_and(|m| !m.is_dir());
    let current = if on_disk { read_small(&path) } else { None };
    let too_big = on_disk && current.is_none();
    let known = state.shadow.get(rel);
    let was_unknown = state.unknown.contains(rel);

    let unchanged = !too_big
        && !was_unknown
        && match (known, &current) {
            (Some(before), Some(now)) => before == now,
            (None, None) => true,
            _ => false,
        };
    if unchanged {
        state.reported.remove(rel);
        return None;
    }
    if !by_agent {
        // Cambio del usuario: pasa a ser la copia de referencia.
        state.reported.remove(rel);
        state.unknown.remove(rel);
        state.shadow.remove(rel);
        match current {
            Some(bytes) => {
                state.shadow.insert(rel.to_owned(), bytes);
            }
            None if too_big => {
                state.unknown.insert(rel.to_owned());
            }
            None => {}
        }
        return None;
    }

    let fingerprint = match &current {
        Some(bytes) => Oid::hash_object(ObjectType::Blob, bytes).ok(),
        None if too_big => Some(Oid::ZERO_SHA1),
        None => None,
    };
    if state.reported.get(rel) == Some(&fingerprint) {
        return None;
    }
    let change_type = change_type(known.is_some() || was_unknown, on_disk);
    let (reverted, note) = match rule.action {
        GuardrailAction::Warn => (false, None),
        GuardrailAction::AutoRevert if was_unknown => (
            false,
            Some("ignorado por Git y demasiado grande para la copia en memoria".to_owned()),
        ),
        GuardrailAction::AutoRevert => match restore_ignored(root, &path, known) {
            Ok(()) => (true, None),
            Err(e) => (false, Some(format!("no se pudo restaurar: {e}"))),
        },
    };
    if reverted {
        state.reported.remove(rel);
    } else {
        state.reported.insert(rel.to_owned(), fingerprint);
    }
    Some(GuardrailViolation {
        file_path: rel.to_owned(),
        pattern: rule.pattern.clone(),
        action: rule.action,
        change_type,
        reverted,
        note,
    })
}

/// Reescribe la copia en memoria o, si el archivo no existía, lo borra.
fn restore_ignored(root: &Path, path: &Path, known: Option<&Vec<u8>>) -> std::io::Result<()> {
    match known {
        Some(bytes) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
                fs::remove_dir_all(path)?;
            }
            fs::write(path, bytes)
        }
        None => {
            match fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
            // Poda carpetas que hayan quedado vacías, sin salir de la raíz.
            let mut dir = path.parent();
            while let Some(d) = dir {
                if d == root || !d.starts_with(root) || fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watcher::capture_if_changed;
    use git2::{Repository, Signature};
    use std::path::PathBuf;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn setup() -> (Tmp, PathBuf, Database, Project) {
        let base = std::env::temp_dir().join(format!("nude-fw-{}", uuid::Uuid::new_v4()));
        let root = base.join("repo");
        fs::create_dir_all(root.join("migrations")).expect("mkdir");
        let root = fs::canonicalize(&root).expect("canon");
        let repo = Repository::init(&root).expect("init");
        for (path, body) in [
            (".gitignore", ".env*\n"),
            ("Cargo.lock", "lock v1\n"),
            ("migrations/001.sql", "create table a;\n"),
            ("src.rs", "fn main() {}\n"),
        ] {
            fs::write(root.join(path), body).expect("write");
        }
        let mut index = repo.index().expect("index");
        for path in [".gitignore", "Cargo.lock", "migrations/001.sql", "src.rs"] {
            index.add_path(Path::new(path)).expect("add");
        }
        index.write().expect("idx");
        let tree = repo.find_tree(index.write_tree().expect("t")).expect("tree");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");
        fs::write(root.join(".env"), "SECRET=original\n").expect("env");

        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("project");
        (Tmp(base), root, db, project)
    }

    fn agent() -> DetectedSession {
        DetectedSession {
            tool: AgentTool::ClaudeCode,
            external_id: Some("s1".into()),
            model_name: Some("claude-opus-5-5".into()),
            last_prompt: None,
            cwd: None,
        }
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn auto_revert_restores_tracked_files_and_removes_new_ones() {
        let (_tmp, root, db, project) = setup();
        db.upsert_guardrail(&project.id, "Cargo.lock", GuardrailAction::AutoRevert).expect("rule");
        db.upsert_guardrail(&project.id, "migrations/**", GuardrailAction::AutoRevert).expect("rule");
        let mut state = GuardState::default();
        state.prime(&db, &project);

        fs::write(root.join("Cargo.lock"), "lock v2\n").expect("write");
        fs::write(root.join("migrations/002.sql"), "drop table a;\n").expect("write");
        fs::write(root.join("src.rs"), "fn main() { println!(); }\n").expect("write");

        let alert = enforce(&db, &project, paths(&["Cargo.lock", "migrations/002.sql", "src.rs"]), &mut state, agent)
            .expect("enforce")
            .expect("alert");
        assert_eq!(alert.agent_tool, "claude-code");
        assert_eq!(alert.violations.len(), 2, "src.rs no está protegido");
        assert!(alert.violations.iter().all(|v| v.reverted));
        assert_eq!(fs::read_to_string(root.join("Cargo.lock")).expect("r"), "lock v1\n");
        assert!(!root.join("migrations/002.sql").exists());
        assert_eq!(fs::read_to_string(root.join("src.rs")).expect("r"), "fn main() { println!(); }\n");

        // Los eventos que provoca la propia restauración no generan otro aviso.
        let again = enforce(&db, &project, paths(&["Cargo.lock", "migrations/002.sql"]), &mut state, agent)
            .expect("enforce");
        assert!(again.is_none());
    }

    #[test]
    fn baseline_is_the_latest_checkpoint_and_folder_deletions_are_expanded() {
        let (_tmp, root, db, project) = setup();
        db.upsert_guardrail(&project.id, "migrations/", GuardrailAction::AutoRevert).expect("rule");
        // Checkpoint con una migración nueva hecha por el usuario.
        fs::write(root.join("migrations/002.sql"), "alter;\n").expect("write");
        capture_if_changed(&db, &root.join("no-home"), &project, Some("usuario")).expect("cap").expect("cp");
        let mut state = GuardState::default();

        fs::remove_dir_all(root.join("migrations")).expect("rm");
        let alert = enforce(&db, &project, paths(&["migrations"]), &mut state, agent)
            .expect("enforce")
            .expect("alert");
        let mut files: Vec<_> = alert.violations.iter().map(|v| v.file_path.as_str()).collect();
        files.sort();
        assert_eq!(files, ["migrations/001.sql", "migrations/002.sql"]);
        assert!(alert.violations.iter().all(|v| v.change_type == ChangeType::Deleted && v.reverted));
        assert_eq!(fs::read_to_string(root.join("migrations/002.sql")).expect("r"), "alter;\n");
    }

    #[test]
    fn warn_mode_keeps_the_change_and_reports_each_content_once() {
        let (_tmp, root, db, project) = setup();
        let rule = db.upsert_guardrail(&project.id, "Cargo.lock", GuardrailAction::Warn).expect("rule");
        let mut state = GuardState::default();

        fs::write(root.join("Cargo.lock"), "lock v2\n").expect("write");
        let alert = enforce(&db, &project, paths(&["Cargo.lock"]), &mut state, agent).expect("e").expect("a");
        assert_eq!(alert.violations[0].action, GuardrailAction::Warn);
        assert_eq!(alert.violations[0].pattern, rule.pattern);
        assert!(!alert.violations[0].reverted);
        assert_eq!(fs::read_to_string(root.join("Cargo.lock")).expect("r"), "lock v2\n");

        assert!(enforce(&db, &project, paths(&["Cargo.lock"]), &mut state, agent).expect("e").is_none());
        fs::write(root.join("Cargo.lock"), "lock v3\n").expect("write");
        assert!(enforce(&db, &project, paths(&["Cargo.lock"]), &mut state, agent).expect("e").is_some());
    }

    #[test]
    fn ignored_env_files_are_restored_from_the_in_memory_copy() {
        let (_tmp, root, db, project) = setup();
        db.upsert_guardrail(&project.id, ".env*", GuardrailAction::AutoRevert).expect("rule");
        let mut state = GuardState::default();
        state.prime(&db, &project);

        fs::write(root.join(".env"), "SECRET=filtrado\n").expect("write");
        fs::write(root.join(".env.production"), "NEW=1\n").expect("write");
        let alert = enforce(&db, &project, paths(&[".env", ".env.production"]), &mut state, agent)
            .expect("e")
            .expect("a");
        assert_eq!(alert.violations.len(), 2);
        assert_eq!(fs::read_to_string(root.join(".env")).expect("r"), "SECRET=original\n");
        assert!(!root.join(".env.production").exists());

        // Un cambio del usuario (sin agente) no se bloquea y pasa a ser la referencia.
        fs::write(root.join(".env"), "SECRET=nuevo\n").expect("write");
        assert!(enforce(&db, &project, paths(&[".env"]), &mut state, DetectedSession::manual).expect("e").is_none());
        fs::write(root.join(".env"), "SECRET=agente\n").expect("write");
        enforce(&db, &project, paths(&[".env"]), &mut state, agent).expect("e").expect("a");
        assert_eq!(fs::read_to_string(root.join(".env")).expect("r"), "SECRET=nuevo\n");
    }

    #[test]
    fn changes_outside_agent_sessions_or_without_rules_are_ignored() {
        let (_tmp, root, db, project) = setup();
        let mut state = GuardState::default();
        fs::write(root.join("Cargo.lock"), "lock v2\n").expect("write");
        // Sin reglas: ni siquiera se consulta la sesión.
        let none = enforce(&db, &project, paths(&["Cargo.lock"]), &mut state, || panic!("no debe detectar"));
        assert!(none.expect("e").is_none());

        db.upsert_guardrail(&project.id, "Cargo.lock", GuardrailAction::AutoRevert).expect("rule");
        let manual = enforce(&db, &project, paths(&["Cargo.lock"]), &mut state, DetectedSession::manual);
        assert!(manual.expect("e").is_none());
        assert_eq!(fs::read_to_string(root.join("Cargo.lock")).expect("r"), "lock v2\n");
    }
}
