use serde::Serialize;
use tauri::{AppHandle, State};

use super::blocking;
use crate::db::{Database, PreviewInfo};
use crate::error::NudeResult;
use crate::preview::{PreviewService, normalize_url};

#[tauri::command]
pub async fn set_project_preview_url(
    db: State<'_, Database>,
    project_id: String,
    url: Option<String>,
) -> NudeResult<Option<String>> {
    let db = db.inner().clone();
    blocking(move || {
        let url = normalize_url(url.as_deref())?;
        db.set_preview_url(&project_id, url.as_deref())?;
        Ok(url)
    })
    .await
}

#[tauri::command]
pub async fn capture_checkpoint_preview(
    app: AppHandle,
    preview: State<'_, PreviewService>,
    project_id: String,
    checkpoint_id: String,
) -> NudeResult<Option<PreviewInfo>> {
    preview
        .inner()
        .clone()
        .capture(&app, project_id, checkpoint_id)
        .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewImage {
    #[serde(flatten)]
    pub info: PreviewInfo,
    /// Ruta absoluta del PNG (bajo `<app data>/previews/`). El frontend la
    /// carga con `convertFileSrc`; nunca viaja la imagen por IPC.
    pub image_path: String,
}

#[tauri::command]
pub async fn get_checkpoint_preview(
    db: State<'_, Database>,
    project_id: String,
    checkpoint_id: String,
) -> NudeResult<Option<PreviewImage>> {
    let db = db.inner().clone();
    blocking(move || {
        Ok(db
            .read_preview(&project_id, &checkpoint_id)?
            .map(|(info, path)| PreviewImage {
                info,
                image_path: path.to_string_lossy().into_owned(),
            }))
    })
    .await
}
