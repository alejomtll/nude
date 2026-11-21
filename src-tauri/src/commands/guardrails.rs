//! Firewall de archivos: reglas por proyecto (`project_guardrails`).

use tauri::State;

use super::{blocking, project_or_404};
use crate::db::Database;
use crate::db::models::{Guardrail, GuardrailAction};
use crate::error::NudeResult;
use crate::safety::normalize_guardrail_pattern;
use crate::watcher::WatcherHandle;

#[tauri::command]
pub async fn list_guardrails(db: State<'_, Database>, project_id: String) -> NudeResult<Vec<Guardrail>> {
    let db = db.inner().clone();
    blocking(move || {
        project_or_404(&db, &project_id)?;
        db.list_guardrails(&project_id)
    })
    .await
}

/// Crea la regla o cambia la acción de un patrón existente.
#[tauri::command]
pub async fn set_guardrail(
    db: State<'_, Database>,
    watcher: State<'_, WatcherHandle>,
    project_id: String,
    pattern: String,
    action: GuardrailAction,
) -> NudeResult<Guardrail> {
    let db = db.inner().clone();
    let watcher = watcher.inner().clone();
    blocking(move || {
        project_or_404(&db, &project_id)?;
        let pattern = normalize_guardrail_pattern(&pattern)?;
        let guardrail = db.upsert_guardrail(&project_id, &pattern, action)?;
        watcher.guardrails_changed(project_id);
        Ok(guardrail)
    })
    .await
}

#[tauri::command]
pub async fn remove_guardrail(
    db: State<'_, Database>,
    watcher: State<'_, WatcherHandle>,
    guardrail_id: i64,
) -> NudeResult<()> {
    let db = db.inner().clone();
    let watcher = watcher.inner().clone();
    blocking(move || {
        if let Some(project_id) = db.delete_guardrail(guardrail_id)? {
            watcher.guardrails_changed(project_id);
        }
        Ok(())
    })
    .await
}
