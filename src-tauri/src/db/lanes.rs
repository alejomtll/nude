//! Carriles paralelos (`agent_lanes`, migración v5).
//!
//! Cada carril activo tiene además una fila propia en `projects` cuya
//! `root_path` es el worktree: así el watcher, los checkpoints, la atribución
//! de sesiones y el firewall funcionan en el carril igual que en un proyecto,
//! sin mezclarse con la historia del repositorio principal. Esas filas no
//! aparecen en `list_projects` (la UI las muestra dentro de su proyecto).

use rusqlite::{OptionalExtension, Row, params};

use super::Database;
use super::models::{AgentLane, LaneStatus, Project};
use crate::error::NudeResult;

const LANE_COLUMNS: &str =
    "id, project_id, name, branch_name, worktree_path, agent_hint, status, created_at";

fn lane_from_row(row: &Row<'_>) -> rusqlite::Result<AgentLane> {
    let status: String = row.get("status")?;
    Ok(AgentLane {
        id: row.get("id")?,
        project_id: row.get("project_id")?,
        name: row.get("name")?,
        branch_name: row.get("branch_name")?,
        worktree_path: row.get("worktree_path")?,
        agent_hint: row.get("agent_hint")?,
        // El CHECK de la tabla garantiza uno de los tres valores.
        status: LaneStatus::parse(&status).unwrap_or(LaneStatus::Discarded),
        created_at: row.get("created_at")?,
    })
}

impl Database {
    pub fn insert_lane(&self, lane: &AgentLane) -> NudeResult<()> {
        let conn = self.lock()?;
        conn.execute(
            &format!("INSERT INTO agent_lanes ({LANE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"),
            params![
                lane.id,
                lane.project_id,
                lane.name,
                lane.branch_name,
                lane.worktree_path,
                lane.agent_hint,
                lane.status.as_str(),
                lane.created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_lane(&self, lane_id: &str) -> NudeResult<Option<AgentLane>> {
        let conn = self.lock()?;
        Ok(conn
            .query_row(
                &format!("SELECT {LANE_COLUMNS} FROM agent_lanes WHERE id = ?1"),
                params![lane_id],
                lane_from_row,
            )
            .optional()?)
    }

    /// Carriles activos del proyecto, del más antiguo al más reciente.
    pub fn list_active_lanes(&self, project_id: &str) -> NudeResult<Vec<AgentLane>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {LANE_COLUMNS} FROM agent_lanes
             WHERE project_id = ?1 AND status = 'active'
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![project_id], lane_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_lane_status(&self, lane_id: &str, status: LaneStatus) -> NudeResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "UPDATE agent_lanes SET status = ?2 WHERE id = ?1",
            params![lane_id, status.as_str()],
        )?;
        Ok(())
    }

    /// Fila de `projects` que vigila el worktree del carril (si existe).
    pub fn lane_project(&self, lane: &AgentLane) -> NudeResult<Option<Project>> {
        let id: Option<String> = {
            let conn = self.lock()?;
            conn.query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                params![lane.worktree_path],
                |row| row.get(0),
            )
            .optional()?
        };
        match id {
            Some(id) => self.get_project(&id),
            None => Ok(None),
        }
    }

    /// Todo lo que el watcher debe vigilar al arrancar: los proyectos y los
    /// worktrees de sus carriles activos.
    pub fn list_watch_targets(&self) -> NudeResult<Vec<Project>> {
        let mut projects = self.list_projects()?;
        let lane_ids: Vec<String> = {
            let conn = self.lock()?;
            let mut stmt = conn.prepare(
                "SELECT p.id FROM projects p
                 JOIN agent_lanes l ON l.worktree_path = p.root_path
                 WHERE l.status = 'active'",
            )?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in lane_ids {
            if let Some(project) = self.get_project(&id)? {
                projects.push(project);
            }
        }
        Ok(projects)
    }

    /// Proyectos internos de los carriles (activos o no) de `project_id`.
    pub fn lane_projects_of(&self, project_id: &str) -> NudeResult<Vec<String>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT p.id FROM projects p
             JOIN agent_lanes l ON l.worktree_path = p.root_path
             WHERE l.project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// El carril hereda las reglas del firewall de su proyecto.
    pub fn copy_guardrails(&self, from_project: &str, to_project: &str) -> NudeResult<()> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT OR IGNORE INTO project_guardrails (project_id, pattern, action, created_at)
             SELECT ?2, pattern, action, created_at FROM project_guardrails WHERE project_id = ?1",
            params![from_project, to_project],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{GuardrailAction, new_id};

    #[test]
    fn lanes_are_listed_per_project_and_hidden_from_the_project_list() {
        let db = Database::open_in_memory().expect("db");
        let (main, _) = db.upsert_project("demo", "/demo").expect("project");
        db.upsert_guardrail(&main.id, ".env*", GuardrailAction::AutoRevert).expect("rule");
        let lane = AgentLane {
            id: new_id(),
            project_id: main.id.clone(),
            name: "Refactor".into(),
            branch_name: "nude/lane-refactor".into(),
            worktree_path: "/data/worktrees/p/refactor".into(),
            agent_hint: Some("codex".into()),
            status: LaneStatus::Active,
            created_at: 1,
        };
        db.insert_lane(&lane).expect("insert");
        let (inner, _) = db.upsert_project("demo · Refactor", &lane.worktree_path).expect("lane project");
        db.copy_guardrails(&main.id, &inner.id).expect("copy rules");

        assert_eq!(db.list_projects().expect("projects"), vec![main.clone()]);
        assert_eq!(db.list_watch_targets().expect("targets").len(), 2);
        assert_eq!(db.lane_project(&lane).expect("lane project").map(|p| p.id), Some(inner.id.clone()));
        assert_eq!(db.list_guardrails(&inner.id).expect("rules").len(), 1);
        assert_eq!(db.list_active_lanes(&main.id).expect("lanes"), vec![lane.clone()]);

        db.set_lane_status(&lane.id, LaneStatus::Merged).expect("status");
        assert!(db.list_active_lanes(&main.id).expect("lanes").is_empty());
        assert_eq!(db.get_lane(&lane.id).expect("get").map(|l| l.status), Some(LaneStatus::Merged));
        assert_eq!(db.list_watch_targets().expect("targets").len(), 1, "solo se vigilan carriles activos");
        assert_eq!(db.lane_projects_of(&main.id).expect("inner"), vec![inner.id]);

        db.delete_project(&main.id).expect("delete");
        assert!(db.get_lane(&lane.id).expect("get").is_none(), "ON DELETE CASCADE");
    }
}
