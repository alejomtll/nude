use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

use super::Database;
use crate::error::{NudeError, NudeResult};

pub const MAX_PREVIEW_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewInfo {
    pub project_id: String,
    pub checkpoint_id: String,
    pub captured_at: i64,
    pub url: String,
}

impl Database {
    pub fn set_preview_url(&self, project_id: &str, url: Option<&str>) -> NudeResult<()> {
        let conn = self.lock()?;
        if conn.execute(
            "UPDATE projects SET preview_url = ?2 WHERE id = ?1",
            params![project_id, url],
        )? == 0
        {
            return Err(NudeError::NotFound(format!("proyecto {project_id}")));
        }
        Ok(())
    }

    fn preview_path(&self, project_id: &str, checkpoint_id: &str) -> Option<PathBuf> {
        if valid_id(project_id) && valid_id(checkpoint_id) {
            return Some(
                self.preview_dir
                    .as_ref()?
                    .join(project_id)
                    .join(format!("{checkpoint_id}.png")),
            );
        }
        None
    }

    pub fn save_preview(&self, info: &PreviewInfo, png: &[u8]) -> NudeResult<bool> {
        if png.len() > MAX_PREVIEW_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(NudeError::Task("miniatura PNG no válida".into()));
        }
        let path = self
            .preview_path(&info.project_id, &info.checkpoint_id)
            .ok_or_else(|| NudeError::Task("ruta de captura no válida".into()))?;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Una captura pendiente no puede reaparecer después de borrar el proyecto,
        // cambiar la URL o guardar un nuevo estado.
        let changed = tx.execute(
            "INSERT INTO checkpoint_previews (checkpoint_id, image_path, captured_at, url)
             SELECT c.id, ?3, ?4, ?5 FROM checkpoints c
             JOIN projects p ON p.id = c.project_id
             WHERE c.id = ?2 AND p.id = ?1 AND p.preview_url = ?5
               AND c.id = (SELECT id FROM checkpoints WHERE project_id = p.id
                           ORDER BY created_at DESC, rowid DESC LIMIT 1)
             ON CONFLICT(checkpoint_id) DO UPDATE SET
                image_path = excluded.image_path, captured_at = excluded.captured_at, url = excluded.url",
            params![info.project_id, info.checkpoint_id, path.to_string_lossy(), info.captured_at, info.url],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        let directory = path
            .parent()
            .ok_or_else(|| NudeError::Task("ruta de captura sin directorio".into()))?;
        fs::create_dir_all(directory)?;
        let temporary = directory.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = fs::write(&temporary, png).and_then(|()| fs::rename(&temporary, &path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        tx.commit()?;
        Ok(true)
    }

    /// Metadatos y ruta en disco de la captura del checkpoint. No lee la
    /// imagen: el frontend la carga con `convertFileSrc` (protocolo `asset`),
    /// así el PNG no viaja por IPC en Base64 al mover el scrubber.
    pub fn read_preview(
        &self,
        project_id: &str,
        checkpoint_id: &str,
    ) -> NudeResult<Option<(PreviewInfo, PathBuf)>> {
        let Some(path) = self.preview_path(project_id, checkpoint_id) else {
            return Ok(None);
        };
        let conn = self.lock()?;
        let info = conn
            .query_row(
                "SELECT cp.captured_at, cp.url FROM checkpoint_previews cp
             JOIN checkpoints c ON c.id = cp.checkpoint_id
             WHERE c.project_id = ?1 AND cp.checkpoint_id = ?2",
                params![project_id, checkpoint_id],
                |row| {
                    Ok(PreviewInfo {
                        project_id: project_id.to_owned(),
                        checkpoint_id: checkpoint_id.to_owned(),
                        captured_at: row.get(0)?,
                        url: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let Some(info) = info else {
            return Ok(None);
        };
        drop(conn);
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        // Solo se ofrece una captura válida: PNG y dentro del tamaño máximo.
        use std::io::Read;
        let mut header = [0u8; 8];
        let valid = file.metadata()?.len() <= MAX_PREVIEW_BYTES as u64
            && file.read_exact(&mut header).is_ok()
            && &header == b"\x89PNG\r\n\x1a\n";
        Ok(valid.then_some((info, path)))
    }

    pub(super) fn cleanup_project_previews(&self, project_id: &str) -> NudeResult<()> {
        if let Some(root) = self.preview_dir.as_ref().filter(|_| valid_id(project_id)) {
            ignore_missing(fs::remove_dir_all(root.join(project_id)))?;
        }
        Ok(())
    }

    /// Recolecta PNG huérfanos por cascada SQLite, y temporales de una captura interrumpida.
    pub fn prune_previews(&self) -> NudeResult<()> {
        let Some(root) = &self.preview_dir else {
            return Ok(());
        };
        let dirs = match fs::read_dir(root.as_ref()) {
            Ok(dirs) => dirs,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for dir in dirs {
            let dir = dir?;
            let name = dir.file_name();
            let Some(project_id) = name.to_str().filter(|id| valid_id(id)) else {
                continue;
            };
            if !dir.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(dir.path())? {
                let path = entry?.path();
                let Some(id) = path
                    .file_stem()
                    .and_then(|id| id.to_str())
                    .filter(|id| valid_id(id))
                else {
                    continue;
                };
                match path.extension().and_then(|ext| ext.to_str()) {
                    Some("tmp") => ignore_missing(fs::remove_file(&path))?,
                    Some("png") if !has_preview(&tx, project_id, id)? => {
                        ignore_missing(fs::remove_file(&path))?
                    }
                    _ => {}
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn has_preview(
    conn: &rusqlite::Connection,
    project_id: &str,
    checkpoint_id: &str,
) -> NudeResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM checkpoint_previews p JOIN checkpoints c
         ON c.id = p.checkpoint_id WHERE c.project_id = ?1 AND c.id = ?2)",
        params![project_id, checkpoint_id],
        |row| row.get(0),
    )?)
}

fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok() && id.len() == 36
}

fn ignore_missing(result: std::io::Result<()>) -> std::io::Result<()> {
    result.or_else(|error| {
        if error.kind() == ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{AgentTool, Checkpoint, Project};

    // El almacenamiento solo inspecciona la firma. La codificación la hace WebKit.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
    const URL: &str = "http://localhost:5173/";

    struct DataDirectory(PathBuf);
    impl Drop for DataDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture() -> (DataDirectory, Database, Project, Checkpoint) {
        let data = DataDirectory(
            std::env::temp_dir().join(format!("nude-previews-{}", uuid::Uuid::new_v4())),
        );
        let db = Database::open_in_app_dir(&data.0).expect("database");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");
        let session = db
            .get_or_create_session(&project.id, AgentTool::Manual, None, None)
            .expect("session");
        let cp = db
            .insert_checkpoint(
                &session.id,
                &project.id,
                "refs/nude/checkpoints/s/1",
                None,
                &[],
            )
            .expect("checkpoint");
        (data, db, project, cp)
    }

    fn info(project: &Project, cp: &Checkpoint) -> PreviewInfo {
        PreviewInfo {
            project_id: project.id.clone(),
            checkpoint_id: cp.id.clone(),
            captured_at: 1234,
            url: URL.into(),
        }
    }

    #[test]
    fn preview_url_round_trips_and_can_be_disabled() {
        let (data, db, project, _) = fixture();
        assert_eq!(project.preview_url, None);
        assert!(db.set_preview_url("missing", Some(URL)).is_err());
        db.set_preview_url(&project.id, Some(URL)).expect("enable");
        drop(db);
        let db = Database::open_in_app_dir(&data.0).expect("reopen");
        assert_eq!(
            db.list_projects().expect("list")[0].preview_url.as_deref(),
            Some(URL)
        );
        db.set_preview_url(&project.id, None).expect("disable");
        assert_eq!(
            db.get_project(&project.id)
                .expect("get")
                .expect("project")
                .preview_url,
            None
        );
    }

    #[test]
    fn preview_files_follow_project_and_checkpoint_cascades() {
        let (data, db, project, cp) = fixture();
        db.set_preview_url(&project.id, Some(URL)).expect("enable");
        let first = info(&project, &cp);
        assert!(db.save_preview(&first, PNG).expect("save"));
        let path = data
            .0
            .join("previews")
            .join(&project.id)
            .join(format!("{}.png", cp.id));
        assert!(path.is_file());
        let (stored, stored_path) = db
            .read_preview(&project.id, &cp.id)
            .expect("read")
            .expect("preview");
        assert_eq!(stored.captured_at, first.captured_at);
        assert_eq!(stored.url, URL);
        assert_eq!(stored_path, path);
        assert_eq!(fs::read(&stored_path).expect("png"), PNG);

        // Un archivo corrupto no se ofrece al frontend.
        fs::write(&path, b"no es png").expect("corrupt");
        assert!(db.read_preview(&project.id, &cp.id).expect("read").is_none());
        fs::write(&path, PNG).expect("restore");

        db.lock()
            .expect("db")
            .execute("DELETE FROM checkpoints WHERE id = ?1", [&cp.id])
            .expect("delete checkpoint");
        assert!(
            db.read_preview(&project.id, &cp.id)
                .expect("read")
                .is_none()
        );
        db.prune_previews().expect("prune checkpoint file");
        assert!(!path.exists());

        let next = db
            .insert_checkpoint(
                &cp.session_id,
                &project.id,
                "refs/nude/checkpoints/s/2",
                None,
                &[],
            )
            .expect("second checkpoint");
        assert!(
            db.save_preview(&info(&project, &next), PNG)
                .expect("save second")
        );
        db.delete_project(&project.id).expect("delete project");
        assert!(!data.0.join("previews").join(&project.id).exists());
        assert!(
            db.read_preview(&project.id, &next.id)
                .expect("read")
                .is_none()
        );
        assert!(
            !db.save_preview(&info(&project, &next), PNG)
                .expect("discard late capture")
        );
    }

    #[test]
    fn stale_captures_are_discarded() {
        let (_data, db, project, cp) = fixture();
        let capture = info(&project, &cp);
        db.set_preview_url(&project.id, Some("http://localhost:8080/"))
            .expect("change URL");
        assert!(
            !db.save_preview(&capture, PNG)
                .expect("discard previous URL")
        );
        db.set_preview_url(&project.id, Some(URL)).expect("enable");
        let next = db
            .insert_checkpoint(
                &cp.session_id,
                &project.id,
                "refs/nude/checkpoints/s/2",
                None,
                &[],
            )
            .expect("second checkpoint");
        assert!(
            !db.save_preview(&capture, PNG)
                .expect("discard historical capture")
        );
        assert!(
            db.save_preview(&info(&project, &next), PNG)
                .expect("save current capture")
        );
    }
}
