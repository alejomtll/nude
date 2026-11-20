//! Error común del backend. Se serializa como texto plano para que el
//! frontend lo reciba directamente en el `catch` de `invoke()`.

use serde::{Serialize, Serializer};

#[derive(Debug, thiserror::Error)]
pub enum NudeError {
    #[error("error de base de datos: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("error de git: {0}")]
    Git(#[from] git2::Error),

    #[error("error de E/S: {0}")]
    Io(#[from] std::io::Error),

    #[error("la ruta no es un repositorio Git válido: {0}")]
    NotARepository(String),

    #[error("el repositorio es bare (sin árbol de trabajo): {0}")]
    BareRepository(String),

    #[error("la ruta contiene caracteres no UTF-8: {0}")]
    NonUtf8Path(String),

    #[error("identificador no válido para una referencia Git: {0}")]
    InvalidRefComponent(String),

    #[error("no encontrado: {0}")]
    NotFound(String),

    #[error("el estado interno quedó inconsistente (mutex envenenado)")]
    LockPoisoned,

    #[error("la tarea en segundo plano falló: {0}")]
    Task(String),
}

impl Serialize for NudeError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

pub type NudeResult<T> = Result<T, NudeError>;
