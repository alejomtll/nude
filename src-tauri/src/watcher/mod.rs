//! Observador de ficheros + detector de sesiones de agentes.
//!
//! Un hilo propio vigila las raíces de los proyectos registrados con `notify`.
//! Los eventos se agrupan con un debounce de `DEBOUNCE`; cuando un proyecto
//! queda en calma se compara su árbol de trabajo con el último checkpoint y,
//! si hay cambios reales, se crea uno nuevo (ver `capture`).

mod capture;
mod filter;
mod sessions;

pub use capture::{RollbackResult, capture_if_changed, create_manual_checkpoint, rollback_to_checkpoint};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::db::Database;
use crate::db::models::{Checkpoint, Project};
use crate::error::NudeResult;

/// Nombre del evento Tauri emitido hacia el frontend (payload: `Checkpoint`).
pub const EVENT_CHECKPOINT_CREATED: &str = "checkpoint-created";

/// Silencio necesario tras el último evento antes de evaluar un proyecto.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);
/// Si el proyecto no para de cambiar, se evalúa igualmente pasado este tiempo.
const MAX_WAIT: Duration = Duration::from_secs(15);

/// Directorio home del usuario (donde viven `~/.claude`, `~/.codex`…).
#[derive(Debug, Clone)]
pub struct HomeDir(pub PathBuf);

enum Msg {
    Fs(notify::Result<notify::Event>),
    Watch(Project),
    Remove(String, Sender<NudeResult<()>>),
}

/// Control del hilo vigilante. Barato de clonar.
#[derive(Clone)]
pub struct WatcherHandle {
    tx: Sender<Msg>,
}

impl WatcherHandle {
    /// Empieza a vigilar un proyecto (idempotente).
    pub fn watch(&self, project: Project) {
        if self.tx.send(Msg::Watch(project)).is_err() {
            eprintln!("nude: el watcher no está en ejecución");
        }
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
        capture::delete_project(db, &project_id)
    }
}

struct Watched {
    project: Project,
    root: PathBuf,
    /// (primer evento, último evento) pendientes de evaluar.
    pending: Option<(Instant, Instant)>,
}

/// Arranca el hilo vigilante. `on_checkpoint` se invoca desde ese hilo por
/// cada checkpoint creado (la capa Tauri lo usa para emitir el evento).
pub fn start<F>(db: Database, home: PathBuf, projects: Vec<Project>, on_checkpoint: F) -> WatcherHandle
where
    F: Fn(&Checkpoint) + Send + 'static,
{
    let (tx, rx) = mpsc::channel::<Msg>();
    let handle = WatcherHandle { tx: tx.clone() };

    let spawned = std::thread::Builder::new()
        .name("nude-watcher".into())
        .spawn(move || run(rx, tx, db, home, projects, on_checkpoint));
    if let Err(e) = spawned {
        eprintln!("nude: no se pudo lanzar el watcher: {e}");
    }
    handle
}

fn run<F>(
    rx: mpsc::Receiver<Msg>,
    tx: Sender<Msg>,
    db: Database,
    home: PathBuf,
    initial: Vec<Project>,
    on_checkpoint: F,
) where
    F: Fn(&Checkpoint),
{
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(move |res| {
        let _ = tx.send(Msg::Fs(res));
    }) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("nude: no se pudo crear el watcher de ficheros: {e}");
            return;
        }
    };

    let mut watched: HashMap<String, Watched> = HashMap::new();
    for project in initial {
        add_project(&mut watcher, &mut watched, project);
    }

    loop {
        let timeout = next_wakeup(&watched).unwrap_or(Duration::from_secs(3600));
        match rx.recv_timeout(timeout) {
            Ok(Msg::Fs(Ok(event))) => handle_event(&mut watched, &event),
            Ok(Msg::Fs(Err(e))) => eprintln!("nude: error del watcher: {e}"),
            Ok(Msg::Watch(project)) => match db.get_project(&project.id) {
                Ok(Some(project)) => add_project(&mut watcher, &mut watched, project),
                Ok(None) => {} // La solicitud puede llegar tras quitar el proyecto.
                Err(e) => eprintln!("nude: no se pudo consultar el proyecto: {e}"),
            },
            Ok(Msg::Remove(project_id, reply)) => {
                let result = capture::delete_project(&db, &project_id);
                if result.is_ok() {
                    if let Some(entry) = watched.remove(&project_id) {
                        if let Err(e) = watcher.unwatch(&entry.root) {
                            eprintln!("nude: no se pudo cancelar la vigilancia de {}: {e}", entry.root.display());
                        }
                    }
                }
                let _ = reply.send(result);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        process_due(&mut watched, &db, &home, &on_checkpoint);
    }
}

fn add_project(watcher: &mut RecommendedWatcher, watched: &mut HashMap<String, Watched>, project: Project) {
    if watched.contains_key(&project.id) {
        return;
    }
    let root = PathBuf::from(&project.root_path);
    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        eprintln!("nude: no se pudo vigilar {}: {e}", root.display());
        return;
    }
    // Evaluación inicial: puede haber cambios hechos con la app cerrada.
    let now = Instant::now();
    watched.insert(
        project.id.clone(),
        Watched {
            project,
            root,
            pending: Some((now, now)),
        },
    );
}

fn handle_event(watched: &mut HashMap<String, Watched>, event: &notify::Event) {
    if matches!(event.kind, EventKind::Access(_)) {
        return;
    }
    let now = Instant::now();
    for path in &event.paths {
        if let Some(entry) = owner_of(watched, path) {
            if filter::is_ignored(&entry.root, path) {
                continue;
            }
            entry.pending = Some(match entry.pending {
                Some((first, _)) => (first, now),
                None => (now, now),
            });
        }
    }
}

/// Proyecto cuya raíz contiene `path` (el más específico si hay anidados).
fn owner_of<'a>(watched: &'a mut HashMap<String, Watched>, path: &Path) -> Option<&'a mut Watched> {
    watched
        .values_mut()
        .filter(|w| path.starts_with(&w.root))
        .max_by_key(|w| w.root.components().count())
}

fn due_at((first, last): (Instant, Instant)) -> Instant {
    (last + DEBOUNCE).min(first + MAX_WAIT)
}

fn next_wakeup(watched: &HashMap<String, Watched>) -> Option<Duration> {
    let now = Instant::now();
    watched
        .values()
        .filter_map(|w| w.pending)
        .map(|p| due_at(p).saturating_duration_since(now))
        .min()
}

fn process_due<F>(watched: &mut HashMap<String, Watched>, db: &Database, home: &Path, on_checkpoint: &F)
where
    F: Fn(&Checkpoint),
{
    let now = Instant::now();
    for entry in watched.values_mut() {
        let Some(pending) = entry.pending else { continue };
        if due_at(pending) > now {
            continue;
        }
        entry.pending = None;
        match capture_if_changed(db, home, &entry.project, None) {
            Ok(Some(checkpoint)) => on_checkpoint(&checkpoint),
            Ok(None) => {}
            Err(e) => eprintln!("nude: fallo al crear checkpoint de {}: {e}", entry.project.name),
        }
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
        let handle = start(db.clone(), base.join("home"), vec![project.clone()], move |cp| {
            let _ = tx.lock().expect("lock").send(cp.clone());
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
}
