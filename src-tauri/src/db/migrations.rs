//! Migraciones del esquema. Cada entrada de `MIGRATIONS` es una versión;
//! `PRAGMA user_version` guarda cuántas se han aplicado.
//!
//! Los cambios de esquema se añaden como nuevas versiones al final del array.

use rusqlite::{Connection, TransactionBehavior};

use crate::error::NudeResult;

const MIGRATIONS: &[&str] = &[
    // v1 — esquema base (AGENTS.md §3)
    r#"
    CREATE TABLE IF NOT EXISTS projects (
        id          TEXT PRIMARY KEY NOT NULL,
        name        TEXT NOT NULL,
        root_path   TEXT NOT NULL UNIQUE,
        created_at  TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS agent_sessions (
        id          TEXT PRIMARY KEY NOT NULL,
        project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        agent_tool  TEXT NOT NULL
                    CHECK (agent_tool IN ('claude-code', 'codex', 'opencode', 'manual')),
        model_name  TEXT,
        started_at  TEXT NOT NULL,
        ended_at    TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_agent_sessions_project ON agent_sessions(project_id, started_at);

    CREATE TABLE IF NOT EXISTS checkpoints (
        id                   TEXT PRIMARY KEY NOT NULL,
        session_id           TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
        project_id           TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        git_ref              TEXT NOT NULL UNIQUE,
        prompt_summary       TEXT,
        files_changed_count  INTEGER NOT NULL DEFAULT 0,
        created_at           TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_checkpoints_project ON checkpoints(project_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id);

    CREATE TABLE IF NOT EXISTS file_diffs (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        checkpoint_id  TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
        file_path      TEXT NOT NULL,
        change_type    TEXT NOT NULL CHECK (change_type IN ('added', 'modified', 'deleted')),
        patch_text     TEXT
    );
    CREATE INDEX IF NOT EXISTS idx_file_diffs_checkpoint ON file_diffs(checkpoint_id);
    "#,
    // v2 — id externo de la sesión del agente (jsonl de Claude/Codex, `ses_*` de
    // OpenCode) para reutilizar la misma fila entre checkpoints.
    r#"
    ALTER TABLE agent_sessions ADD COLUMN external_id TEXT;
    CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_sessions_external
        ON agent_sessions(project_id, agent_tool, external_id)
        WHERE external_id IS NOT NULL;
    "#,
    // v3 — firewall de archivos: rutas protegidas por proyecto (glob relativo a
    // la raíz) y qué hacer si un agente las toca.
    r#"
    CREATE TABLE IF NOT EXISTS project_guardrails (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        pattern     TEXT NOT NULL,
        action      TEXT NOT NULL CHECK (action IN ('warn', 'auto_revert')),
        created_at  TEXT NOT NULL,
        UNIQUE (project_id, pattern)
    );
    "#,
];

pub fn run(conn: &mut Connection) -> NudeResult<()> {
    // La versión se lee bajo el mismo bloqueo que se usa para migrar,
    // así dos instancias no pueden aplicar a la vez la misma migración.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let current: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let current = usize::try_from(current).unwrap_or(0);

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let version = index + 1;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as i64)?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DB_FILE_NAME, Database};
    use std::fs;
    use std::path::PathBuf;

    struct TestDirectory(PathBuf);

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn opening_application_data_preserves_history_after_upgrade_and_reopen() {
        for prior_version in [0, 1] {
            let root = TestDirectory(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
            let data_dir = root.0.join("Application Support/com.nude.app");
            fs::create_dir_all(&data_dir).expect("application data directory");
            let path = data_dir.join(DB_FILE_NAME);
            {
                let old = Connection::open(&path).expect("old database");
                old.execute_batch(MIGRATIONS[0]).expect("base schema");
                old.pragma_update(None, "user_version", prior_version)
                    .expect("old schema version");
                old.execute_batch(
                    "INSERT INTO projects VALUES ('p1', 'Demo', '/demo', '2026-01-01');
                     INSERT INTO agent_sessions VALUES
                        ('s1', 'p1', 'codex', 'gpt', '2026-01-01', NULL);
                     INSERT INTO checkpoints VALUES
                        ('c1', 's1', 'p1', 'refs/nude/checkpoints/s1/a', 'Resumen', 1, '2026-01-01');
                     INSERT INTO file_diffs (checkpoint_id, file_path, change_type, patch_text)
                        VALUES ('c1', 'demo.rs', 'added', '+contenido');",
                )
                .expect("existing project and history");
            }

            for _ in 0..2 {
                let db = Database::open_in_app_dir(&data_dir).expect("open persistent database");
                let projects = db.list_projects().expect("projects");
                assert_eq!(projects.len(), 1);
                assert_eq!(projects[0].root_path, "/demo");

                let checkpoints = db.list_checkpoints("p1").expect("checkpoints");
                assert_eq!(checkpoints.len(), 1);
                assert_eq!(checkpoints[0].session_id, "s1");
                assert_eq!(checkpoints[0].model_name.as_deref(), Some("gpt"));
                assert_eq!(checkpoints[0].prompt_summary.as_deref(), Some("Resumen"));
                let diffs = db.list_file_diffs("c1").expect("file diffs");
                assert_eq!(diffs.len(), 1);
                assert_eq!(diffs[0].patch_text.as_deref(), Some("+contenido"));

                let version: i64 = db
                    .lock()
                    .expect("database lock")
                    .pragma_query_value(None, "user_version", |row| row.get(0))
                    .expect("migrated schema version");
                assert_eq!(version, latest_version());
            }
        }
    }
}
