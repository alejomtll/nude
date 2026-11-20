//! Consultas sobre `nude.db`.

use rusqlite::{OptionalExtension, Row, params};

use super::Database;
use super::models::{AgentSession, AgentTool, Checkpoint, FileDiff, Project, new_id, now_rfc3339};
use crate::error::{NudeError, NudeResult};

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        root_path: row.get("root_path")?,
        created_at: row.get("created_at")?,
    })
}

fn checkpoint_from_row(row: &Row<'_>) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        project_id: row.get("project_id")?,
        git_ref: row.get("git_ref")?,
        prompt_summary: row.get("prompt_summary")?,
        files_changed_count: row.get("files_changed_count")?,
        created_at: row.get("created_at")?,
    })
}

impl Database {
    // ---------------------------------------------------------------- projects

    /// Inserta el proyecto o, si `root_path` ya existe, devuelve el registrado.
    /// El booleano indica si se creó en esta llamada.
    pub fn upsert_project(&self, name: &str, root_path: &str) -> NudeResult<(Project, bool)> {
        let conn = self.lock()?;
        let inserted = conn.execute(
            "INSERT INTO projects (id, name, root_path, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(root_path) DO NOTHING",
            params![new_id(), name, root_path, now_rfc3339()],
        )?;
        let project = conn.query_row(
            "SELECT id, name, root_path, created_at FROM projects WHERE root_path = ?1",
            params![root_path],
            project_from_row,
        )?;
        Ok((project, inserted == 1))
    }

    pub fn list_projects(&self) -> NudeResult<Vec<Project>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, created_at FROM projects
             ORDER BY name COLLATE NOCASE, created_at",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_project(&self, project_id: &str) -> NudeResult<Option<Project>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT id, name, root_path, created_at FROM projects WHERE id = ?1",
                params![project_id],
                project_from_row,
            )
            .optional()?)
    }

    // ---------------------------------------------------------- agent_sessions

    /// Abre una sesión de agente. Lo usará el detector de sesiones (Fase 2).
    #[allow(dead_code)]
    pub fn create_session(
        &self,
        project_id: &str,
        agent_tool: AgentTool,
        model_name: Option<&str>,
    ) -> NudeResult<AgentSession> {
        let session = AgentSession {
            id: new_id(),
            project_id: project_id.to_owned(),
            agent_tool: agent_tool.as_str().to_owned(),
            model_name: model_name.map(str::to_owned),
            started_at: now_rfc3339(),
            ended_at: None,
        };
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO agent_sessions (id, project_id, agent_tool, model_name, started_at, ended_at)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            params![
                session.id,
                session.project_id,
                session.agent_tool,
                session.model_name,
                session.started_at
            ],
        )?;
        Ok(session)
    }

    // ------------------------------------------------------------- checkpoints

    /// Checkpoints de un proyecto, del más reciente al más antiguo.
    pub fn list_checkpoints(&self, project_id: &str) -> NudeResult<Vec<Checkpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project_id, git_ref, prompt_summary,
                    files_changed_count, created_at
             FROM checkpoints
             WHERE project_id = ?1
             ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map(params![project_id], checkpoint_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Registra un checkpoint y sus diffs en una sola transacción.
    /// `git_ref` debe venir de `git::create_snapshot`.
    #[allow(dead_code)]
    pub fn insert_checkpoint(
        &self,
        session_id: &str,
        project_id: &str,
        git_ref: &str,
        prompt_summary: Option<&str>,
        diffs: &[FileDiff],
    ) -> NudeResult<Checkpoint> {
        let checkpoint = Checkpoint {
            id: new_id(),
            session_id: session_id.to_owned(),
            project_id: project_id.to_owned(),
            git_ref: git_ref.to_owned(),
            prompt_summary: prompt_summary.map(str::to_owned),
            files_changed_count: i64::try_from(diffs.len())
                .map_err(|_| NudeError::Task("demasiados ficheros en el diff".into()))?,
            created_at: now_rfc3339(),
        };

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO checkpoints
                (id, session_id, project_id, git_ref, prompt_summary, files_changed_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                checkpoint.id,
                checkpoint.session_id,
                checkpoint.project_id,
                checkpoint.git_ref,
                checkpoint.prompt_summary,
                checkpoint.files_changed_count,
                checkpoint.created_at
            ],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO file_diffs (checkpoint_id, file_path, change_type, patch_text)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for diff in diffs {
                stmt.execute(params![
                    checkpoint.id,
                    diff.file_path,
                    diff.change_type.as_str(),
                    diff.patch_text
                ])?;
            }
        }
        tx.commit()?;
        Ok(checkpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::ChangeType;

    #[test]
    fn migrations_create_schema_and_are_idempotent() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.lock().expect("lock");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .expect("version");
        assert_eq!(version, migrations::latest_version());

        for table in ["projects", "agent_sessions", "checkpoints", "file_diffs"] {
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |r| r.get(0),
                )
                .expect("query");
            assert_eq!(exists, 1, "falta la tabla {table}");
        }
        drop(conn);

        // Reaplicar no debe fallar ni duplicar nada.
        let mut conn = db.lock().expect("lock");
        migrations::run(&mut conn).expect("re-run");
    }

    #[test]
    fn upsert_project_is_idempotent_by_root_path() {
        let db = Database::open_in_memory().expect("db");
        let (a, created_a) = db.upsert_project("demo", "/tmp/demo").expect("insert");
        let (b, created_b) = db.upsert_project("otro-nombre", "/tmp/demo").expect("upsert");
        assert!(created_a);
        assert!(!created_b);
        assert_eq!(a, b);
        assert_eq!(db.list_projects().expect("list").len(), 1);
        assert_eq!(db.get_project(&a.id).expect("get"), Some(a));
        assert_eq!(db.get_project("no-existe").expect("get"), None);
    }

    #[test]
    fn checkpoints_are_stored_with_diffs_and_listed_newest_first() {
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");
        let session = db
            .create_session(&project.id, AgentTool::ClaudeCode, Some("claude-opus-5-5"))
            .expect("session");

        let diffs = vec![
            FileDiff {
                file_path: "src/main.rs".into(),
                change_type: ChangeType::Modified,
                patch_text: Some("@@ -1 +1 @@\n-a\n+b\n".into()),
            },
            FileDiff {
                file_path: "logo.png".into(),
                change_type: ChangeType::Added,
                patch_text: None,
            },
        ];
        let first = db
            .insert_checkpoint(&session.id, &project.id, "refs/nude/checkpoints/s/1", Some("uno"), &diffs)
            .expect("cp1");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = db
            .insert_checkpoint(&session.id, &project.id, "refs/nude/checkpoints/s/2", None, &[])
            .expect("cp2");

        let listed = db.list_checkpoints(&project.id).expect("list");
        assert_eq!(listed, vec![second, first.clone()]);
        assert_eq!(first.files_changed_count, 2);

        let conn = db.lock().expect("lock");
        let stored: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM file_diffs WHERE checkpoint_id = ?1",
                params![first.id],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(stored, 2);
    }

    #[test]
    fn rejects_unknown_agent_tool_values() {
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");
        let conn = db.lock().expect("lock");
        let result = conn.execute(
            "INSERT INTO agent_sessions (id, project_id, agent_tool, started_at)
             VALUES ('x', ?1, 'cursor', '2026-01-01T00:00:00Z')",
            params![project.id],
        );
        assert!(result.is_err());
    }
}
