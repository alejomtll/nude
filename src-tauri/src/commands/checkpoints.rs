use tauri::{AppHandle, Emitter, Manager, State};

use super::blocking;
use crate::db::Database;
use crate::db::models::{Checkpoint, FileDiff};
use crate::error::{NudeError, NudeResult};
use crate::safety::{self, FileAlert};
use crate::watcher::{
    self, EVENT_CHECKPOINT_CREATED, HomeDir, RollbackResult, UndoAgentPlan, UndoAgentResult,
};

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

pub(crate) fn emit_checkpoint(app: &AppHandle, checkpoint: &Checkpoint) {
    if let Err(e) = app.emit(EVENT_CHECKPOINT_CREATED, checkpoint) {
        eprintln!("nude: no se pudo emitir {EVENT_CHECKPOINT_CREATED}: {e}");
    }
    let service = app.state::<crate::preview::PreviewService>().inner().clone();
    let (app, project, checkpoint) = (app.clone(), checkpoint.project_id.clone(), checkpoint.id.clone());
    tauri::async_runtime::spawn(async move {
        // Una URL desactivada o fuera de servicio no afecta a los checkpoints.
        let _ = service.capture(&app, project, checkpoint).await;
    });
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

/// Avisos del checkpoint: archivos sensibles y posibles bucles del agente.
#[tauri::command]
pub async fn get_checkpoint_alerts(
    db: State<'_, Database>,
    checkpoint_id: String,
) -> NudeResult<Vec<FileAlert>> {
    let db = db.inner().clone();
    blocking(move || safety::checkpoint_alerts(&db, &checkpoint_id)).await
}

/// Qué haría "Deshacer último agente" (`null` si no hay ninguna sesión de IA).
#[tauri::command]
pub async fn preview_undo_last_agent(
    db: State<'_, Database>,
    project_id: String,
) -> NudeResult<Option<UndoAgentPlan>> {
    let db = db.inner().clone();
    blocking(move || watcher::preview_undo_last_agent(&db, &project_id)).await
}

/// Vuelve al checkpoint inmediatamente anterior a la última sesión de IA.
#[tauri::command]
pub async fn undo_last_agent(
    app: AppHandle,
    db: State<'_, Database>,
    home: State<'_, HomeDir>,
    project_id: String,
) -> NudeResult<UndoAgentResult> {
    let db = db.inner().clone();
    let home = home.inner().0.clone();
    let result = blocking(move || watcher::undo_last_agent(&db, &home, &project_id)).await?;
    for checkpoint in [&result.rollback.safety_checkpoint, &result.rollback.restored_checkpoint]
        .into_iter()
        .flatten()
    {
        emit_checkpoint(&app, checkpoint);
    }
    Ok(result)
}
