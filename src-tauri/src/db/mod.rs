//! Persistencia SQLite (`nude.db`).
//!
//! - `migrations`: esquema versionado con `PRAGMA user_version`.
//! - `models`: structs serializables que cruzan el IPC.
//! - `queries`: operaciones CRUD sobre `Database`.

mod migrations;
pub mod models;
mod previews;
pub use previews::{MAX_PREVIEW_BYTES, PreviewInfo};
mod queries;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{NudeError, NudeResult};

const DB_FILE_NAME: &str = "nude.db";

/// Conexión compartida. Es barata de clonar (Arc) para poder moverla a
/// `spawn_blocking` desde los comandos async.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    preview_dir: Option<Arc<PathBuf>>,
}

impl Database {
    /// Abre la base en el directorio de Tauri, independiente del binario instalado.
    /// En macOS: `~/Library/Application Support/com.nude.app/nude.db`.
    pub fn open_in_app_dir(data_dir: &Path) -> NudeResult<Self> {
        std::fs::create_dir_all(data_dir)?;
        let conn = Connection::open(data_dir.join(DB_FILE_NAME))?;
        Self::init(conn, Some(data_dir.join("previews")))
    }

    /// Base de datos en memoria (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> NudeResult<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn init(mut conn: Connection, preview_dir: Option<PathBuf>) -> NudeResult<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrations::run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            preview_dir: preview_dir.map(Arc::new),
        })
    }

    fn lock(&self) -> NudeResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| NudeError::LockPoisoned)
    }
}
