//! Persistencia SQLite (`nude.db`).
//!
//! - `migrations`: esquema versionado con `PRAGMA user_version`.
//! - `models`: structs serializables que cruzan el IPC.
//! - `queries`: operaciones CRUD sobre `Database`.

mod migrations;
pub mod models;
mod queries;

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{NudeError, NudeResult};

/// Nombre del fichero dentro del directorio de datos de la app
/// (`~/Library/Application Support/com.nude.app/`).
pub const DB_FILE_NAME: &str = "nude.db";

/// Conexión compartida. Es barata de clonar (Arc) para poder moverla a
/// `spawn_blocking` desde los comandos async.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Abre (o crea) la base de datos en `path` y aplica las migraciones pendientes.
    pub fn open(path: &Path) -> NudeResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Base de datos en memoria (tests).
    #[cfg(test)]
    pub fn open_in_memory() -> NudeResult<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> NudeResult<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        migrations::run(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> NudeResult<MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| NudeError::LockPoisoned)
    }
}
