//! Tipos de dominio que viajan por IPC (camelCase para TypeScript).
//! Las marcas de tiempo son RFC 3339 en UTC con milisegundos.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTool {
    ClaudeCode,
    Codex,
    Opencode,
    Manual,
}

impl AgentTool {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentTool::ClaudeCode => "claude-code",
            AgentTool::Codex => "codex",
            AgentTool::Opencode => "opencode",
            AgentTool::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub id: String,
    pub project_id: String,
    pub agent_tool: String,
    pub model_name: Option<String>,
    pub external_id: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: String,
    pub session_id: String,
    pub project_id: String,
    pub agent_tool: String,
    pub model_name: Option<String>,
    pub git_ref: String,
    pub prompt_summary: Option<String>,
    pub files_changed_count: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

impl ChangeType {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeType::Added => "added",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "deleted",
        }
    }
}

/// Cambio de un fichero, tal y como lo produce `git::` y lo guarda `file_diffs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    pub file_path: String,
    pub change_type: ChangeType,
    /// Parche unificado. `None` si el fichero es binario o supera el límite de tamaño.
    pub patch_text: Option<String>,
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
