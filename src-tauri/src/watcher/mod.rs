//! Observador de ficheros + detector de sesiones de agentes.
//!
//! Un hilo propio vigila las raíces de los proyectos registrados con `notify`.
//! Los eventos se agrupan con un debounce de `DEBOUNCE`; cuando un proyecto
//! queda en calma se compara su árbol de trabajo con el último checkpoint y,
//! si hay cambios reales, se crea uno nuevo (ver `capture`).
//!
//! En paralelo, el firewall de archivos (`guardrails`) revisa con un debounce
//! mucho más corto las rutas protegidas que toque un agente.

mod capture;
mod filter;
mod guardrails;
mod sessions;
mod status;

pub use capture::{
    RollbackResult, UndoAgentPlan, UndoAgentResult, capture_if_changed,
    create_manual_checkpoint, guarded_change, preview_undo_last_agent, rollback_to_checkpoint,
    undo_last_agent,
};
pub use guardrails::{AGENT_WINDOW, EVENT_GUARDRAIL_TRIGGERED, GuardrailAlert};
pub use sessions::{ACTIVE_WINDOW, latest_session, normalize_path};
pub use status::{StatusBoard, WatchStatus};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::db::Database;
use crate::db::models::{Checkpoint, Project};
use crate::error::{NudeError, NudeResult};

/// Nombre del evento Tauri emitido hacia el frontend (payload: `Checkpoint`).
pub const EVENT_CHECKPOINT_CREATED: &str = "checkpoint-created";

/// Silencio necesario tras el último evento antes de evaluar un proyecto.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Si el proyecto no para de cambiar, se evalúa igualmente pasado este tiempo.
const MAX_WAIT: Duration = Duration::from_secs(15);

/// Rutas pendientes de revisar por el firewall, como máximo (una ráfaga enorme
/// —p. ej. un `npm install` fuera de `node_modules`— no crece sin límite).
const GUARD_MAX_PENDING: usize = 10_000;

/// Cada cuánto se reintenta vigilar un proyecto que falló (permisos, carpeta
/// desmontada…).
const RETRY_EVERY: Duration = Duration::from_secs(30);

/// Evento Tauri con el `WatchStatus` de un proyecto cuando cambia su estado.
pub const EVENT_WATCHER_STATUS: &str = "watcher-status";

/// Lo que el watcher comunica a la capa Tauri.
pub enum WatcherEvent<'a> {
    Checkpoint(&'a Checkpoint),
    Guardrail(&'a GuardrailAlert),
    Status(&'a WatchStatus),
}

/// Directorio home del usuario (donde viven `~/.claude`, `~/.codex`…).
#[derive(Debug, Clone)]
pub struct HomeDir(pub PathBuf);

type ScanReply = Sender<NudeResult<Option<Checkpoint>>>;

enum Msg {
    Fs(notify::Result<notify::Event>),
    Watch(Project),
    Remove(String, Sender<NudeResult<()>>),
    GuardrailsChanged(String),
    /// "Forzar escaneo ahora": revisa el proyecto sin esperar a `notify`.
    Scan(String, ScanReply),
}

/// Control del hilo vigilante. Barato de clonar.
#[derive(Clone)]
pub struct WatcherHandle {
    tx: Sender<Msg>,
    status: StatusBoard,
}

impl WatcherHandle {
    /// Empieza a vigilar un proyecto (idempotente).
    pub fn watch(&self, project: Project) {
        if self.tx.send(Msg::Watch(project)).is_err() {
            eprintln!("nude: el watcher no está en ejecución");
        }
    }

    /// Las reglas del firewall cambiaron: rehace la copia en memoria de los
    /// archivos ignorados protegidos.
    pub fn guardrails_changed(&self, project_id: String) {
        if self.tx.send(Msg::GuardrailsChanged(project_id)).is_err() {
            eprintln!("nude: el watcher no está en ejecución");
        }
    }

    /// Estado de vigilancia del proyecto.
    pub fn status(&self, project_id: &str) -> WatchStatus {
        self.status.get(project_id)
    }

    /// Revisa el proyecto ahora mismo (reintenta la vigilancia si había
    /// fallado) y devuelve el checkpoint creado, si hubo cambios.
    pub fn force_scan(&self, project_id: String) -> NudeResult<Option<Checkpoint>> {
        let (reply, result) = mpsc::channel();
        self.tx
            .send(Msg::Scan(project_id, reply))
            .map_err(|_| NudeError::Task("el watcher no está en ejecución".into()))?;
        result
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| NudeError::Task("el watcher no respondió a tiempo".into()))?
    }

    /// Espera a que se borren los datos y se cancele la vigilancia pendiente.
    pub fn remove_project(&self, db: &Database, project_id: String) -> NudeResult<()> {
        let (reply, result) = mpsc::channel();
        if self.tx.send(Msg::Remove(project_id.clone(), reply)).is_ok() {
            if let Ok(result) = result.recv() {
                return result;
            }
        }
        // Si el watcher terminó, ya no quedan raíces bajo vigilancia.
        self.status.remove(&project_id);
        capture::delete_project(db, &project_id)
    }
}

struct Watched {
    project: Project,
    root: PathBuf,
    /// (primer evento, último evento) pendientes de evaluar.
    pending: Option<(Instant, Instant)>,
    /// Rutas relativas pendientes para el firewall y su ventana de debounce.
    guard_paths: HashSet<String>,
    guard_pending: Option<(Instant, Instant)>,
    guard: guardrails::GuardState,
}

/// Estado del hilo vigilante.
struct Engine<'a, F> {
    /// `None` si `notify` no pudo arrancar: no hay vigilancia en vivo, pero
    /// "Forzar escaneo" sigue funcionando.
    watcher: Option<RecommendedWatcher>,
    watched: HashMap<String, Watched>,
    /// Proyectos que no se pudieron vigilar; se reintentan cada `RETRY_EVERY`.
    failed: HashMap<String, Project>,
    last_retry: Instant,
    db: Database,
    home: PathBuf,
    board: StatusBoard,
    on_event: &'a F,
}

/// Arranca el hilo vigilante. `on_event` se invoca desde ese hilo por cada
/// checkpoint creado, alerta del firewall o cambio de estado de vigilancia
/// (la capa Tauri emite los eventos).
pub fn start<F>(db: Database, home: PathBuf, projects: Vec<Project>, on_event: F) -> WatcherHandle
where
    F: Fn(WatcherEvent<'_>) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Msg>();
    let board = StatusBoard::default();
    for project in &projects {
        board.update(&project.id, |_| {});
    }
    let handle = WatcherHandle { tx: tx.clone(), status: board.clone() };

    let thread_board = board.clone();
    let spawned = std::thread::Builder::new()
        .name("nude-watcher".into())
        .spawn(move || run(rx, tx, db, home, projects, thread_board, on_event));
    if let Err(e) = spawned {
        let message = format!("no se pudo lanzar el hilo del watcher: {e}");
        eprintln!("nude: {message}");
        board.set_engine_error(message);
    }
    handle
}

fn run<F>(
    rx: mpsc::Receiver<Msg>,
    tx: Sender<Msg>,
    db: Database,
    home: PathBuf,
    initial: Vec<Project>,
    board: StatusBoard,
    on_event: F,
) where
    F: Fn(WatcherEvent<'_>),
{
    let watcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(Msg::Fs(res));
    }) {
        Ok(w) => Some(w),
        Err(e) => {
            let message = format!("no se pudo iniciar la vigilancia de archivos del sistema: {e}");
            eprintln!("nude: {message}");
            board.set_engine_error(message);
            None
        }
    };
    let mut engine = Engine {
        watcher,
        watched: HashMap::new(),
        failed: HashMap::new(),
        last_retry: Instant::now(),
        db,
        home,
        board,
        on_event: &on_event,
    };
    // Todos los proyectos guardados empiezan a vigilarse al arrancar.
    for project in initial {
        engine.add_project(project);
    }

    loop {
        let timeout = engine.next_wakeup();
        match rx.recv_timeout(timeout) {
            Ok(Msg::Fs(Ok(event))) => engine.handle_event(&event),
            Ok(Msg::Fs(Err(e))) => {
                eprintln!("nude: error del watcher: {e}");
                // Si el error trae rutas, se marca el proyecto afectado.
                for path in &e.paths {
                    if let Some(id) = owner_of(&mut engine.watched, path).map(|w| w.project.id.clone()) {
                        let status = engine.board.failed(&id, format!("error de vigilancia: {e}"));
                        engine.emit_status(status);
                    }
                }
            }
            Ok(Msg::Watch(project)) => match engine.db.get_project(&project.id) {
                Ok(Some(project)) => engine.add_project(project),
                Ok(None) => {} // La solicitud puede llegar tras quitar el proyecto.
                Err(e) => eprintln!("nude: no se pudo consultar el proyecto: {e}"),
            },
            Ok(Msg::Remove(project_id, reply)) => {
                let result = capture::delete_project(&engine.db, &project_id);
                if result.is_ok() {
                    engine.failed.remove(&project_id);
                    if let Some(entry) = engine.watched.remove(&project_id) {
                        if let Some(watcher) = engine.watcher.as_mut() {
                            if let Err(e) = watcher.unwatch(&entry.root) {
                                eprintln!("nude: no se pudo cancelar la vigilancia de {}: {e}", entry.root.display());
                            }
                        }
                    }
                    engine.board.remove(&project_id);
                }
                let _ = reply.send(result);
            }
            Ok(Msg::GuardrailsChanged(project_id)) => {
                if let Some(entry) = engine.watched.get_mut(&project_id) {
                    entry.guard.prime(&engine.db, &entry.project);
                }
            }
            Ok(Msg::Scan(project_id, reply)) => {
                let _ = reply.send(engine.scan_now(&project_id));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        engine.retry_failed();
        engine.process_due();
    }
}

/// Comprueba que macOS deja leer la carpeta antes de vigilarla: sin permiso
/// (Privacidad › Archivos y carpetas) `notify` arranca, pero nada funciona.
fn check_readable(root: &Path) -> Result<(), String> {
    match std::fs::read_dir(root) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("la carpeta {} ya no existe", root.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "macOS no da permiso para leer {} ({e}). Concédelo en Ajustes del Sistema › Privacidad y seguridad › Archivos y carpetas (o Acceso total al disco) y pulsa «Forzar escaneo ahora».",
            root.display()
        )),
        Err(e) => Err(format!("no se puede leer {}: {e}", root.display())),
    }
}

impl<F> Engine<'_, F>
where
    F: Fn(WatcherEvent<'_>),
{
    fn emit_status(&self, status: Option<WatchStatus>) {
        if let Some(status) = status {
            (self.on_event)(WatcherEvent::Status(&status));
        }
    }

    /// Empieza a vigilar `project`. Si falla, registra el error exacto en el
    /// tablero (lo ve la UI) y lo deja para reintentar.
    fn add_project(&mut self, project: Project) {
        if self.watched.contains_key(&project.id) {
            return;
        }
        let root = PathBuf::from(&project.root_path);
        let result = check_readable(&root).and_then(|()| match self.watcher.as_mut() {
            Some(watcher) => watcher
                .watch(&root, RecursiveMode::Recursive)
                .map_err(|e| format!("no se pudo vigilar {}: {e}", root.display())),
            None => Err(self
                .board
                .get(&project.id)
                .error
                .unwrap_or_else(|| "la vigilancia de archivos no está disponible".into())),
        });
        if let Err(message) = result {
            eprintln!("nude: {message}");
            let status = self.board.failed(&project.id, message);
            self.emit_status(status);
            self.failed.insert(project.id.clone(), project);
            return;
        }
        self.failed.remove(&project.id);

        // Copia en memoria de los ignorados protegidos antes de que nadie los toque.
        let mut guard = guardrails::GuardState::default();
        guard.prime(&self.db, &project);
        let status = self.board.watching(&project.id);
        self.emit_status(status);
        // Evaluación inicial: puede haber cambios hechos con la app cerrada.
        let now = Instant::now();
        self.watched.insert(
            project.id.clone(),
            Watched {
                project,
                root,
                pending: Some((now, now)),
                guard_paths: HashSet::new(),
                guard_pending: None,
                guard,
            },
        );
    }

    fn retry_failed(&mut self) {
        if self.failed.is_empty() || self.last_retry.elapsed() < RETRY_EVERY {
            return;
        }
        self.last_retry = Instant::now();
        let pending: Vec<Project> = self.failed.values().cloned().collect();
        for project in pending {
            if matches!(self.db.get_project(&project.id), Ok(Some(_))) {
                self.add_project(project);
            } else {
                self.failed.remove(&project.id);
            }
        }
    }

    fn handle_event(&mut self, event: &notify::Event) {
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let now = Instant::now();
        let mut touched = HashSet::new();
        for path in &event.paths {
            if let Some(entry) = owner_of(&mut self.watched, path) {
                if filter::is_ignored(&entry.root, path) {
                    continue;
                }
                touched.insert(entry.project.id.clone());
                entry.pending = Some(match entry.pending {
                    Some((first, _)) => (first, now),
                    None => (now, now),
                });
                if entry.guard_paths.len() < GUARD_MAX_PENDING {
                    if let Some(rel) = relative_slash_path(&entry.root, path) {
                        entry.guard_paths.insert(rel);
                        entry.guard_pending = Some(match entry.guard_pending {
                            Some((first, _)) => (first, now),
                            None => (now, now),
                        });
                    }
                }
            }
        }
        for id in touched {
            self.board.event(&id);
        }
    }

    fn next_wakeup(&self) -> Duration {
        let now = Instant::now();
        let due = self
            .watched
            .values()
            .flat_map(|w| [w.pending.map(due_at), w.guard_pending.map(guard_due_at)])
            .flatten()
            .map(|at| at.saturating_duration_since(now))
            .min();
        let retry = (!self.failed.is_empty())
            .then(|| (self.last_retry + RETRY_EVERY).saturating_duration_since(now));
        [due, retry].into_iter().flatten().min().unwrap_or(Duration::from_secs(3600))
    }

    fn process_due(&mut self) {
        let now = Instant::now();
        let due: Vec<String> = self
            .watched
            .values()
            .filter(|w| {
                w.pending.is_some_and(|p| due_at(p) <= now) || w.guard_pending.is_some_and(|p| guard_due_at(p) <= now)
            })
            .map(|w| w.project.id.clone())
            .collect();
        for id in due {
            let Some(entry) = self.watched.get_mut(&id) else { continue };
            let capture_due = entry.pending.is_some_and(|p| due_at(p) <= now);
            // El firewall va siempre antes que la captura: así un cambio prohibido
            // nunca llega a ser "el último snapshot".
            if entry.guard_pending.is_some() {
                run_guardrails(entry, &self.db, &self.home, self.on_event);
            }
            if capture_due {
                entry.pending = None;
                let _ = self.capture(&id);
            }
        }
    }

    /// Compara el árbol con el último checkpoint y, si cambió, crea uno nuevo.
    fn capture(&mut self, project_id: &str) -> NudeResult<Option<Checkpoint>> {
        let project = match self.watched.get(project_id) {
            Some(entry) => entry.project.clone(),
            None => self
                .db
                .get_project(project_id)?
                .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))?,
        };
        let watching = self.watched.contains_key(project_id);
        let result = if Path::new(&project.root_path).is_dir() {
            capture_if_changed(&self.db, &self.home, &project, None)
        } else {
            Err(NudeError::NotFound(format!("la carpeta {} ya no existe", project.root_path)))
        };
        let status = match &result {
            Ok(checkpoint) => self.board.scanned(project_id, checkpoint.is_some(), None, watching),
            Err(e) => {
                eprintln!("nude: fallo al crear checkpoint de {}: {e}", project.name);
                self.board.scanned(project_id, false, Some(format!("la última revisión falló: {e}")), watching)
            }
        };
        self.emit_status(status);
        if let Ok(Some(checkpoint)) = &result {
            (self.on_event)(WatcherEvent::Checkpoint(checkpoint));
        }
        result
    }

    /// "Forzar escaneo ahora": reintenta la vigilancia si había fallado, pasa
    /// el firewall por lo pendiente y revisa el árbol sin esperar al debounce.
    fn scan_now(&mut self, project_id: &str) -> NudeResult<Option<Checkpoint>> {
        if !self.watched.contains_key(project_id) {
            let project = self
                .db
                .get_project(project_id)?
                .ok_or_else(|| NudeError::NotFound(format!("proyecto {project_id}")))?;
            self.add_project(project);
        }
        if let Some(entry) = self.watched.get_mut(project_id) {
            if entry.guard_pending.is_some() {
                run_guardrails(entry, &self.db, &self.home, self.on_event);
            }
            entry.pending = None;
        }
        self.capture(project_id)
    }
}

/// Proyecto cuya raíz contiene `path` (el más específico si hay anidados).
fn owner_of<'a>(watched: &'a mut HashMap<String, Watched>, path: &Path) -> Option<&'a mut Watched> {
    watched
        .values_mut()
        .filter(|w| path.starts_with(&w.root))
        .max_by_key(|w| w.root.components().count())
}

/// Ruta relativa a la raíz con `/` (`None` si no es UTF-8 o está fuera).
fn relative_slash_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    let rel = parts?.join("/");
    (!rel.is_empty()).then_some(rel)
}

fn due_at((first, last): (Instant, Instant)) -> Instant {
    (last + DEBOUNCE).min(first + MAX_WAIT)
}

fn guard_due_at((first, last): (Instant, Instant)) -> Instant {
    (last + guardrails::GUARD_DEBOUNCE).min(first + guardrails::GUARD_MAX_WAIT)
}

fn run_guardrails<F>(entry: &mut Watched, db: &Database, home: &Path, on_event: &F)
where
    F: Fn(WatcherEvent<'_>),
{
    entry.guard_pending = None;
    let paths = std::mem::take(&mut entry.guard_paths);
    let root = entry.root.clone();
    let detect = || {
        sessions::detect_session_within(home, &root, SystemTime::now(), guardrails::AGENT_WINDOW)
    };
    match guardrails::enforce(db, &entry.project, paths, &mut entry.guard, detect) {
        Ok(Some(alert)) => on_event(WatcherEvent::Guardrail(&alert)),
        Ok(None) => {}
        Err(e) => eprintln!("nude: fallo del firewall de archivos en {}: {e}", entry.project.name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use std::fs;
    use std::sync::mpsc::channel;

    #[test]
    fn debounced_changes_create_a_single_checkpoint() {
        let base = std::env::temp_dir().join(format!("nude-watch-{}", uuid::Uuid::new_v4()));
        let root = base.join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let root = fs::canonicalize(&root).expect("canon");
        let repo = Repository::init(&root).expect("init");
        fs::write(root.join("a.txt"), "1\n").expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.txt")).expect("add");
        index.write().expect("idx");
        let tree = repo.find_tree(index.write_tree().expect("t")).expect("tree");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");

        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("p");

        let (tx, rx) = channel::<Checkpoint>();
        let tx = std::sync::Mutex::new(tx);
        let handle = start(db.clone(), base.join("home"), vec![project.clone()], move |event| {
            if let WatcherEvent::Checkpoint(cp) = event {
                let _ = tx.lock().expect("lock").send(cp.clone());
            }
        });
        std::thread::sleep(Duration::from_millis(2500)); // evaluación inicial (sin cambios)
        assert!(rx.try_recv().is_err(), "árbol limpio: no debe haber checkpoint");

        // Ráfaga de escrituras + ruido ignorado.
        for i in 0..5 {
            fs::write(root.join(format!("f{i}.txt")), "x\n").expect("write");
            std::thread::sleep(Duration::from_millis(100));
        }
        fs::create_dir_all(root.join("node_modules/dep")).expect("mkdir");
        fs::write(root.join("node_modules/dep/index.js"), "x").expect("write");

        let cp = rx.recv_timeout(Duration::from_secs(10)).expect("checkpoint-created");
        assert_eq!(cp.project_id, project.id);
        assert_eq!(cp.files_changed_count, 6, "5 ficheros + node_modules (sin .gitignore)");
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_err(), "solo un checkpoint por ráfaga");

        handle.remove_project(&db, project.id.clone()).expect("remove project");
        assert!(db.get_project(&project.id).expect("project").is_none());
        assert!(db.get_checkpoint(&cp.id).expect("checkpoint").is_none());
        assert!(db.list_file_diffs(&cp.id).expect("diffs").is_empty());
        assert!(repo.find_reference(&cp.git_ref).is_ok());

        // Una orden de alta encolada con el ID anterior no reanuda la vigilancia.
        handle.watch(project);
        fs::write(root.join("after.txt"), "x\n").expect("write after removal");
        assert!(rx.recv_timeout(Duration::from_secs(3)).is_err(), "el watcher sigue detenido");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn agent_edit_of_a_protected_file_is_reverted_before_the_checkpoint() {
        use crate::db::models::GuardrailAction;
        let base = std::env::temp_dir().join(format!("nude-watch-fw-{}", uuid::Uuid::new_v4()));
        let root = base.join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let root = fs::canonicalize(&root).expect("canon");
        let repo = Repository::init(&root).expect("init");
        fs::write(root.join("Cargo.lock"), "v1\n").expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("Cargo.lock")).expect("add");
        index.write().expect("idx");
        let tree = repo.find_tree(index.write_tree().expect("t")).expect("tree");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");

        // Sesión de Claude Code recién escrita para este proyecto.
        let home = base.join("home");
        let session_dir = home.join(".claude/projects").join(sessions::claude_project_dir_name(&root));
        fs::create_dir_all(&session_dir).expect("mkdir");
        fs::write(
            session_dir.join("s1.jsonl"),
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[]}}"#,
        )
        .expect("session");

        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("p");
        db.upsert_guardrail(&project.id, "Cargo.lock", GuardrailAction::AutoRevert).expect("rule");

        let (tx, rx) = channel::<Result<GuardrailAlert, Checkpoint>>();
        let tx = std::sync::Mutex::new(tx);
        let _handle = start(db.clone(), home, vec![project.clone()], move |event| {
            let message = match event {
                WatcherEvent::Guardrail(alert) => Ok(alert.clone()),
                WatcherEvent::Checkpoint(cp) => Err(cp.clone()),
                WatcherEvent::Status(_) => return,
            };
            let _ = tx.lock().expect("lock").send(message);
        });
        std::thread::sleep(Duration::from_millis(2500));

        let started = Instant::now();
        fs::write(root.join("Cargo.lock"), "v2 del agente\n").expect("write");
        fs::write(root.join("notes.md"), "ok\n").expect("write");

        let alert = match rx.recv_timeout(Duration::from_secs(5)).expect("evento") {
            Ok(alert) => alert,
            Err(cp) => panic!("el checkpoint llegó antes que el firewall: {cp:?}"),
        };
        assert!(started.elapsed() < DEBOUNCE, "el aviso no espera al debounce de checkpoints");
        assert_eq!(alert.agent_tool, "claude-code");
        assert_eq!(alert.violations.len(), 1);
        assert!(alert.violations[0].reverted);
        assert_eq!(fs::read_to_string(root.join("Cargo.lock")).expect("r"), "v1\n");

        // El checkpoint posterior solo recoge lo permitido.
        let cp = match rx.recv_timeout(Duration::from_secs(10)).expect("checkpoint") {
            Err(cp) => cp,
            Ok(alert) => panic!("aviso repetido: {alert:?}"),
        };
        let files: Vec<_> = db.list_file_diffs(&cp.id).expect("diffs").into_iter().map(|d| d.file_path).collect();
        assert_eq!(files, ["notes.md"]);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn unreachable_folders_report_the_exact_error_and_force_scan_works_without_waiting() {
        let base = std::env::temp_dir().join(format!("nude-watch-st-{}", uuid::Uuid::new_v4()));
        let root = base.join("repo");
        fs::create_dir_all(&root).expect("mkdir");
        let root = fs::canonicalize(&root).expect("canon");
        let repo = Repository::init(&root).expect("init");
        fs::write(root.join("a.txt"), "1\n").expect("write");
        let mut index = repo.index().expect("index");
        index.add_path(Path::new("a.txt")).expect("add");
        index.write().expect("idx");
        let tree = repo.find_tree(index.write_tree().expect("t")).expect("tree");
        let sig = Signature::now("T", "t@e.com").expect("sig");
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).expect("commit");

        let db = Database::open_in_memory().expect("db");
        let (ok, _) = db.upsert_project("repo", root.to_str().expect("utf8")).expect("p");
        let gone_path = base.join("borrado");
        let (gone, _) = db.upsert_project("borrado", gone_path.to_str().expect("utf8")).expect("p");

        let (tx, rx) = channel::<WatchStatus>();
        let tx = std::sync::Mutex::new(tx);
        let handle = start(db.clone(), base.join("home"), vec![ok.clone(), gone.clone()], move |event| {
            if let WatcherEvent::Status(status) = event {
                let _ = tx.lock().expect("lock").send(status.clone());
            }
        });
        let mut seen = Vec::new();
        while let Ok(status) = rx.recv_timeout(Duration::from_secs(2)) {
            seen.push(status);
        }
        let failed = seen.iter().find(|s| s.project_id == gone.id).expect("estado del proyecto roto");
        assert_eq!(failed.state, status::WatchState::Error);
        assert!(failed.error.as_deref().is_some_and(|e| e.contains("ya no existe")), "{failed:?}");
        assert_eq!(handle.status(&gone.id).state, status::WatchState::Error);
        assert_eq!(handle.status(&ok.id).state, status::WatchState::Active);
        assert!(handle.status(&ok.id).watching_since.is_some());

        // Forzar escaneo: crea el checkpoint al momento, sin esperar al debounce.
        fs::write(root.join("b.txt"), "nuevo\n").expect("write");
        let started = Instant::now();
        let cp = handle.force_scan(ok.id.clone()).expect("scan").expect("checkpoint");
        assert!(started.elapsed() < DEBOUNCE);
        assert_eq!(cp.files_changed_count, 1);
        assert!(handle.status(&ok.id).last_checkpoint_at.is_some());
        assert!(handle.force_scan(ok.id.clone()).expect("scan").is_none(), "sin cambios nuevos");

        // La carpeta vuelve a existir: el escaneo forzado reintenta la vigilancia.
        assert!(handle.force_scan(gone.id.clone()).is_err());
        fs::create_dir_all(&gone_path).expect("mkdir");
        Repository::init(&gone_path).expect("init");
        handle.force_scan(gone.id.clone()).expect("rescan");
        assert_eq!(handle.status(&gone.id).state, status::WatchState::Active);
        let _ = fs::remove_dir_all(&base);
    }
}
