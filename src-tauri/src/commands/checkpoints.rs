use tauri::{AppHandle, Emitter, State};

use super::blocking;
use crate::db::Database;
use crate::db::models::{Checkpoint, FileDiff};
use crate::error::{NudeError, NudeResult};
use crate::watcher::{self, EVENT_CHECKPOINT_CREATED, HomeDir, RollbackResult};

#[tauri::command]
pub async fn create_manual_checkpoint(
    app: AppHandle,
    db: State<'_, Database>,
    project_id: String,
    summary: String,
) -> NudeResult<Checkpoint> {
    let db = db.inner().clone();
    let checkpoint =
        blocking(move || watcher::create_manual_checkpoint(&db, &project_id, &summary)).await?;
    emit_checkpoint(&app, &checkpoint);
    Ok(checkpoint)
}

fn emit_checkpoint(app: &AppHandle, checkpoint: &Checkpoint) {
    if let Err(e) = app.emit(EVENT_CHECKPOINT_CREATED, checkpoint) {
        eprintln!("nude: no se pudo emitir {EVENT_CHECKPOINT_CREATED}: {e}");
    }
}

/// Ficheros cambiados por un checkpoint (respecto al checkpoint anterior del
/// proyecto, o a `HEAD` si es el primero) con su parche unificado.
#[tauri::command]
pub async fn get_checkpoint_diffs(
    db: State<'_, Database>,
    checkpoint_id: String,
) -> NudeResult<Vec<FileDiff>> {
    let db = db.inner().clone();
    blocking(move || {
        if db.get_checkpoint(&checkpoint_id)?.is_none() {
            return Err(NudeError::NotFound(format!("checkpoint {checkpoint_id}")));
        }
        db.list_file_diffs(&checkpoint_id)
    })
    .await
}

/// Restaura el árbol de trabajo al estado exacto de la ref interna del
/// checkpoint. Antes guarda el estado actual como checkpoint de seguridad,
/// de modo que el rollback también se puede deshacer.
#[tauri::command]
pub async fn rollback_checkpoint(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    checkpoint_id: String,
) -> NudeResult<RollbackResult> {
    let db = db.inner().clone();
    let home = home.inner().0.clone();
    let result = blocking(move || watcher::rollback_to_checkpoint(&db, &home, &checkpoint_id)).await?;

    for checkpoint in [&result.safety_checkpoint, &result.restored_checkpoint]
        .into_iter()
        .flatten()
    {
        emit_checkpoint(&app, checkpoint);
    }
    Ok(result)
}
