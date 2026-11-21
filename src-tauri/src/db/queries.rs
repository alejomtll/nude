//! Consultas sobre `nude.db`.

use rusqlite::{OptionalExtension, Row, params};

use super::Database;
use super::models::{
    AgentSession, AgentTool, ChangeType, Checkpoint, FileDiff, Guardrail, GuardrailAction, Project,
    new_id, now_rfc3339,
};
use crate::error::{NudeError, NudeResult};

const CHECKPOINT_SELECT: &str =
    "SELECT c.*, s.agent_tool, s.model_name
     FROM checkpoints c JOIN agent_sessions s ON s.id = c.session_id";

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get("id")?,
        name: row.get("name")?,
        root_path: row.get("root_path")?,
        preview_url: row.get("preview_url")?,
        created_at: row.get("created_at")?,
    })
}

fn session_from_row(row: &Row<'_>) -> rusqlite::Result<AgentSession> {
    Ok(AgentSession {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        agent_tool: row.get("agent_tool")?,
        model_name: row.get("model_name")?,
        external_id: row.get("external_id")?,
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
    })
}

fn checkpoint_from_row(row: &Row<'_>) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        project_id: row.get("project_id")?,
        agent_tool: row.get("agent_tool")?,
        model_name: row.get("model_name")?,
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
            "SELECT id, name, root_path, created_at, preview_url FROM projects WHERE root_path = ?1",
            params![root_path],
            project_from_row,
        )?;
        Ok((project, inserted == 1))
    }

    pub fn list_projects(&self) -> NudeResult<Vec<Project>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, root_path, created_at, preview_url FROM projects
             ORDER BY name COLLATE NOCASE, created_at",
        )?;
        let rows = stmt.query_map([], project_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_project(&self, project_id: &str) -> NudeResult<Option<Project>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT id, name, root_path, created_at, preview_url FROM projects WHERE id = ?1",
                params![project_id],
                project_from_row,
            )
            .optional()?)
    }

    pub fn delete_project(&self, project_id: &str) -> NudeResult<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM projects WHERE id = ?1", params![project_id])?;
        if let Err(error) = self.cleanup_project_previews(project_id) {
            eprintln!("nude: no se pudieron limpiar las capturas del proyecto: {error}");
        }
        Ok(())
    }

    // ---------------------------------------------------------- agent_sessions

    /// Devuelve la sesión abierta que corresponde a `(agent_tool, external_id)`
    /// o la crea. Al crear una nueva se cierran las demás sesiones abiertas del
    /// proyecto. `external_id = None` ⇒ sesión `manual` (se reutiliza la abierta).
    pub fn get_or_create_session(
        &self,
        project_id: &str,
        agent_tool: AgentTool,
        external_id: Option<&str>,
        model_name: Option<&str>,
    ) -> NudeResult<AgentSession> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let existing = tx
            .query_row(
                "SELECT id, project_id, agent_tool, model_name, external_id, started_at, ended_at
                 FROM agent_sessions
                 WHERE project_id = ?1 AND agent_tool = ?2
                   AND ((?3 IS NULL AND external_id IS NULL AND ended_at IS NULL) OR external_id = ?3)
                 ORDER BY started_at DESC LIMIT 1",
                params![project_id, agent_tool.as_str(), external_id],
                session_from_row,
            )
            .optional()?;

        let session = match existing {
            Some(mut session) => {
                let mut changed = false;
                if let Some(model) = model_name {
                    if session.model_name.as_deref() != Some(model) {
                        session.model_name = Some(model.to_owned());
                        changed = true;
                    }
                }
                // Una sesión externa reactivada deja de estar cerrada.
                if external_id.is_some() && session.ended_at.is_some() {
                    session.ended_at = None;
                    changed = true;
                }
                if changed {
                    tx.execute(
                        "UPDATE agent_sessions SET model_name = ?2, ended_at = ?3 WHERE id = ?1",
                        params![session.id, session.model_name, session.ended_at],
                    )?;
                }
                session
            }
            None => {
                let session = AgentSession {
                    id: new_id(),
                    project_id: project_id.to_owned(),
                    agent_tool: agent_tool.as_str().to_owned(),
                    model_name: model_name.map(str::to_owned),
                    external_id: external_id.map(str::to_owned),
                    started_at: now_rfc3339(),
                    ended_at: None,
                };
                tx.execute(
                    "UPDATE agent_sessions SET ended_at = ?2 WHERE project_id = ?1 AND ended_at IS NULL",
                    params![project_id, session.started_at],
                )?;
                tx.execute(
                    "INSERT INTO agent_sessions
                        (id, project_id, agent_tool, model_name, external_id, started_at, ended_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
                    params![
                        session.id,
                        session.project_id,
                        session.agent_tool,
                        session.model_name,
                        session.external_id,
                        session.started_at
                    ],
                )?;
                session
            }
        };
        tx.commit()?;
        Ok(session)
    }

    // ------------------------------------------------------------- checkpoints

    /// Checkpoints de un proyecto, del más reciente al más antiguo.
    pub fn list_checkpoints(&self, project_id: &str) -> NudeResult<Vec<Checkpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{CHECKPOINT_SELECT} WHERE c.project_id = ?1
             ORDER BY c.created_at DESC, c.rowid DESC"
        ))?;
        let rows = stmt.query_map(params![project_id], checkpoint_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_checkpoint(&self, checkpoint_id: &str) -> NudeResult<Option<Checkpoint>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!("{CHECKPOINT_SELECT} WHERE c.id = ?1"),
                params![checkpoint_id],
                checkpoint_from_row,
            )
            .optional()?)
    }

    pub fn latest_checkpoint(&self, project_id: &str) -> NudeResult<Option<Checkpoint>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!(
                    "{CHECKPOINT_SELECT} WHERE c.project_id = ?1
                     ORDER BY c.created_at DESC, c.rowid DESC LIMIT 1"
                ),
                params![project_id],
                checkpoint_from_row,
            )
            .optional()?)
    }

    pub fn list_file_diffs(&self, checkpoint_id: &str) -> NudeResult<Vec<FileDiff>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT file_path, change_type, patch_text FROM file_diffs
             WHERE checkpoint_id = ?1 ORDER BY file_path",
        )?;
        let rows = stmt.query_map(params![checkpoint_id], |row| {
            let change: String = row.get("change_type")?;
            let change_type = match change.as_str() {
                "added" => ChangeType::Added,
                "deleted" => ChangeType::Deleted,
                _ => ChangeType::Modified,
            };
            Ok(FileDiff {
                file_path: row.get("file_path")?,
                change_type,
                patch_text: row.get("patch_text")?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sesión (no manual) del checkpoint de agente más reciente del proyecto.
    pub fn last_agent_session(&self, project_id: &str) -> NudeResult<Option<AgentSession>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                "SELECT s.id, s.project_id, s.agent_tool, s.model_name, s.external_id,
                        s.started_at, s.ended_at
                 FROM checkpoints c JOIN agent_sessions s ON s.id = c.session_id
                 WHERE c.project_id = ?1 AND s.agent_tool <> 'manual'
                 ORDER BY c.created_at DESC, c.rowid DESC LIMIT 1",
                params![project_id],
                session_from_row,
            )
            .optional()?)
    }

    /// Checkpoints de una sesión, del más antiguo al más reciente.
    pub fn session_checkpoints(&self, session_id: &str) -> NudeResult<Vec<Checkpoint>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "{CHECKPOINT_SELECT} WHERE c.session_id = ?1 ORDER BY c.created_at, c.rowid"
        ))?;
        let rows = stmt.query_map(params![session_id], checkpoint_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Checkpoint del proyecto inmediatamente anterior a `checkpoint_id`.
    pub fn checkpoint_before(&self, project_id: &str, checkpoint_id: &str) -> NudeResult<Option<Checkpoint>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!(
                    "{CHECKPOINT_SELECT}
                     WHERE c.project_id = ?1
                       AND (c.created_at, c.rowid) <
                           (SELECT created_at, rowid FROM checkpoints WHERE id = ?2)
                     ORDER BY c.created_at DESC, c.rowid DESC LIMIT 1"
                ),
                params![project_id, checkpoint_id],
                checkpoint_from_row,
            )
            .optional()?)
    }

    /// Para la sesión de `checkpoint_id`: sus checkpoints hasta él (incluido),
    /// en orden cronológico, con las rutas que tocó cada uno.
    pub fn session_file_history(&self, checkpoint_id: &str) -> NudeResult<Vec<(String, Vec<String>)>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT c.id, f.file_path
             FROM checkpoints c
             LEFT JOIN file_diffs f ON f.checkpoint_id = c.id
             WHERE c.session_id = (SELECT session_id FROM checkpoints WHERE id = ?1)
               AND (c.created_at, c.rowid) <=
                   (SELECT created_at, rowid FROM checkpoints WHERE id = ?1)
             ORDER BY c.created_at, c.rowid, f.file_path",
        )?;
        let rows = stmt.query_map(params![checkpoint_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?;
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for row in rows {
            let (id, path) = row?;
            if out.last().map(|(last, _)| last != &id).unwrap_or(true) {
                out.push((id, Vec::new()));
            }
            if let (Some(path), Some((_, paths))) = (path, out.last_mut()) {
                paths.push(path);
            }
        }
        Ok(out)
    }

    /// Registra un checkpoint y sus diffs en una sola transacción.
    /// `git_ref` debe venir de `git::create_snapshot`.
    pub fn insert_checkpoint(
        &self,
        session_id: &str,
        project_id: &str,
        git_ref: &str,
        prompt_summary: Option<&str>,
        diffs: &[FileDiff],
    ) -> NudeResult<Checkpoint> {
        let checkpoint_id = new_id();
        let files_changed_count = i64::try_from(diffs.len())
            .map_err(|_| NudeError::Task("demasiados ficheros en el diff".into()))?;

        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO checkpoints
                (id, session_id, project_id, git_ref, prompt_summary, files_changed_count, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                checkpoint_id,
                session_id,
                project_id,
                git_ref,
                prompt_summary,
                files_changed_count,
                now_rfc3339()
            ],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO file_diffs (checkpoint_id, file_path, change_type, patch_text)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for diff in diffs {
                stmt.execute(params![
                    checkpoint_id,
                    diff.file_path,
                    diff.change_type.as_str(),
                    diff.patch_text
                ])?;
            }
        }
        let checkpoint = tx.query_row(
            &format!("{CHECKPOINT_SELECT} WHERE c.id = ?1"),
            params![checkpoint_id],
            checkpoint_from_row,
        )?;
        tx.commit()?;
        Ok(checkpoint)
    }

    // -------------------------------------------------------------- guardrails

    pub fn list_guardrails(&self, project_id: &str) -> NudeResult<Vec<Guardrail>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, project_id, pattern, action, created_at FROM project_guardrails
             WHERE project_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![project_id], guardrail_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Crea la regla o, si el patrón ya existe en el proyecto, cambia su acción.
    pub fn upsert_guardrail(
        &self,
        project_id: &str,
        pattern: &str,
        action: GuardrailAction,
    ) -> NudeResult<Guardrail> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO project_guardrails (project_id, pattern, action, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (project_id, pattern) DO UPDATE SET action = excluded.action",
            params![project_id, pattern, action.as_str(), now_rfc3339()],
        )?;
        Ok(conn.query_row(
            "SELECT id, project_id, pattern, action, created_at FROM project_guardrails
             WHERE project_id = ?1 AND pattern = ?2",
            params![project_id, pattern],
            guardrail_from_row,
        )?)
    }

    /// Borra la regla y devuelve el proyecto al que pertenecía (`None` si no existía).
    pub fn delete_guardrail(&self, guardrail_id: i64) -> NudeResult<Option<String>> {
        let conn = self.lock()?;
        let project_id: Option<String> = conn
            .query_row(
                "SELECT project_id FROM project_guardrails WHERE id = ?1",
                params![guardrail_id],
                |r| r.get(0),
            )
            .optional()?;
        conn.execute("DELETE FROM project_guardrails WHERE id = ?1", params![guardrail_id])?;
        Ok(project_id)
    }
}

fn guardrail_from_row(row: &Row<'_>) -> rusqlite::Result<Guardrail> {
    let action: String = row.get("action")?;
    Ok(Guardrail {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        pattern: row.get("pattern")?,
        // El CHECK de la tabla garantiza uno de los dos valores.
        action: GuardrailAction::parse(&action).unwrap_or(GuardrailAction::Warn),
        created_at: row.get("created_at")?,
    })
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

        for table in ["projects", "agent_sessions", "checkpoints", "file_diffs", "project_guardrails"] {
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
            .get_or_create_session(&project.id, AgentTool::ClaudeCode, Some("abc"), Some("claude-opus-5-5"))
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
        assert_eq!(first.agent_tool, "claude-code");
        assert_eq!(first.model_name.as_deref(), Some("claude-opus-5-5"));
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
    fn sessions_are_reused_by_external_id_and_previous_ones_closed() {
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");

        let manual = db
            .get_or_create_session(&project.id, AgentTool::Manual, None, None)
            .expect("manual");
        let manual_again = db
            .get_or_create_session(&project.id, AgentTool::Manual, None, None)
            .expect("manual 2");
        assert_eq!(manual.id, manual_again.id);

        let claude = db
            .get_or_create_session(&project.id, AgentTool::ClaudeCode, Some("s1"), Some("opus"))
            .expect("claude");
        assert_ne!(claude.id, manual.id);
        let claude_again = db
            .get_or_create_session(&project.id, AgentTool::ClaudeCode, Some("s1"), Some("sonnet"))
            .expect("claude 2");
        assert_eq!(claude_again.id, claude.id);
        assert_eq!(claude_again.model_name.as_deref(), Some("sonnet"));

        // La sesión manual se cerró al llegar la del agente: la siguiente manual es otra.
        let manual_new = db
            .get_or_create_session(&project.id, AgentTool::Manual, None, None)
            .expect("manual 3");
        assert_ne!(manual_new.id, manual.id);
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

    #[test]
    fn guardrails_upsert_list_delete_and_cascade() {
        let db = Database::open_in_memory().expect("db");
        let (project, _) = db.upsert_project("demo", "/tmp/demo").expect("project");
        let env = db.upsert_guardrail(&project.id, ".env*", GuardrailAction::Warn).expect("env");
        db.upsert_guardrail(&project.id, "migrations/**", GuardrailAction::AutoRevert).expect("mig");
        // Mismo patrón: cambia la acción, no duplica.
        let env2 = db.upsert_guardrail(&project.id, ".env*", GuardrailAction::AutoRevert).expect("env2");
        assert_eq!(env2.id, env.id);
        assert_eq!(env2.action, GuardrailAction::AutoRevert);

        let rules = db.list_guardrails(&project.id).expect("list");
        assert_eq!(rules.iter().map(|g| g.pattern.as_str()).collect::<Vec<_>>(), [".env*", "migrations/**"]);

        assert_eq!(db.delete_guardrail(env.id).expect("del").as_deref(), Some(project.id.as_str()));
        assert_eq!(db.delete_guardrail(env.id).expect("del again"), None);

        let conn = db.lock().expect("lock");
        let bad = conn.execute(
            "INSERT INTO project_guardrails (project_id, pattern, action, created_at)
             VALUES (?1, 'x', 'block', '2026-01-01')",
            params![project.id],
        );
        assert!(bad.is_err(), "acción desconocida");
        drop(conn);

        db.delete_project(&project.id).expect("delete project");
        assert!(db.list_guardrails(&project.id).expect("list").is_empty());
    }
}
