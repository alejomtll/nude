//! Migraciones del esquema. Cada entrada de `MIGRATIONS` es una versión;
//! `PRAGMA user_version` guarda cuántas se han aplicado.
//!
//! Regla (AGENTS.md §5.2): nunca editar una migración ya publicada; añadir
//! una nueva al final del array.

use rusqlite::Connection;

use crate::error::NudeResult;

const MIGRATIONS: &[&str] = &[
    // v1 — esquema base (AGENTS.md §3)
    r#"
    CREATE TABLE projects (
        id          TEXT PRIMARY KEY NOT NULL,
        name        TEXT NOT NULL,
        root_path   TEXT NOT NULL UNIQUE,
        created_at  TEXT NOT NULL
    );

    CREATE TABLE agent_sessions (
        id          TEXT PRIMARY KEY NOT NULL,
        project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        agent_tool  TEXT NOT NULL
                    CHECK (agent_tool IN ('claude-code', 'codex', 'opencode', 'manual')),
        model_name  TEXT,
        started_at  TEXT NOT NULL,
        ended_at    TEXT
    );
    CREATE INDEX idx_agent_sessions_project ON agent_sessions(project_id, started_at);

    CREATE TABLE checkpoints (
        id                   TEXT PRIMARY KEY NOT NULL,
        session_id           TEXT NOT NULL REFERENCES agent_sessions(id) ON DELETE CASCADE,
        project_id           TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        git_ref              TEXT NOT NULL UNIQUE,
        prompt_summary       TEXT,
        files_changed_count  INTEGER NOT NULL DEFAULT 0,
        created_at           TEXT NOT NULL
    );
    CREATE INDEX idx_checkpoints_project ON checkpoints(project_id, created_at);
    CREATE INDEX idx_checkpoints_session ON checkpoints(session_id);

    CREATE TABLE file_diffs (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        checkpoint_id  TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
        file_path      TEXT NOT NULL,
        change_type    TEXT NOT NULL CHECK (change_type IN ('added', 'modified', 'deleted')),
        patch_text     TEXT
    );
    CREATE INDEX idx_file_diffs_checkpoint ON file_diffs(checkpoint_id);
    "#,
    // v2 — id externo de la sesión del agente (jsonl de Claude/Codex, `ses_*` de
    // OpenCode) para reutilizar la misma fila entre checkpoints.
    r#"
    ALTER TABLE agent_sessions ADD COLUMN external_id TEXT;
    CREATE UNIQUE INDEX idx_agent_sessions_external
        ON agent_sessions(project_id, agent_tool, external_id)
        WHERE external_id IS NOT NULL;
    "#,
];

pub fn run(conn: &mut Connection) -> NudeResult<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let current = usize::try_from(current).unwrap_or(0);

    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let version = index + 1;
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}
