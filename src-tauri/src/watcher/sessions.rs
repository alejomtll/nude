//! Detector local de sesiones activas de agentes de IA.
//!
//! Lee (solo lectura) los historiales que dejan en disco:
//! - Claude Code: `~/.claude/projects/<cwd codificado>/<uuid>.jsonl`
//! - Codex CLI:   `~/.codex/sessions/AAAA/MM/DD/rollout-*.jsonl`
//! - OpenCode:    `~/.local/share/opencode/opencode.db` (SQLite) o, en versiones
//!   antiguas, `storage/{session,message,part}/…` en JSON.
//!
//! Una sesión está "activa" si se escribió en ella durante los últimos
//! `ACTIVE_WINDOW`. Si ninguna lo está, el resultado es `manual`.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, OpenFlags, params};
use serde_json::Value;

use crate::db::models::AgentTool;

/// Donde Claude Code crea los worktrees de cada tarea, relativo al proyecto.
pub const CLAUDE_WORKTREES_DIR: &str = ".claude/worktrees";

pub const ACTIVE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Cuánto del final de un `.jsonl` se lee para buscar modelo y último prompt.
const TAIL_BYTES: u64 = 1024 * 1024;
const MAX_PROMPT_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedSession {
    pub tool: AgentTool,
    pub external_id: Option<String>,
    pub model_name: Option<String>,
    pub last_prompt: Option<String>,
    /// Carpeta de trabajo del agente según su historial (normalizada).
    pub cwd: Option<String>,
}

impl DetectedSession {
    pub fn manual() -> Self {
        Self {
            tool: AgentTool::Manual,
            external_id: None,
            model_name: None,
            last_prompt: None,
            cwd: None,
        }
    }
}

/// Ventana de atribución de checkpoints: una sesión escrita en los últimos
/// 15 min se considera activa. (El firewall usa una propia, mucho más corta.)
pub fn detect_active_session(home: &Path, project_root: &Path, now: SystemTime) -> DetectedSession {
    detect_session_within(home, project_root, now, ACTIVE_WINDOW)
}

/// Sesión más reciente para `project_root` escrita en los últimos `window`, o `manual`.
pub fn detect_session_within(home: &Path, project_root: &Path, now: SystemTime, window: Duration) -> DetectedSession {
    latest_session(home, project_root, now, window)
        .map(|(_, s)| s)
        .unwrap_or_else(DetectedSession::manual)
}

/// Igual, pero con la hora de la última actividad (para el diagnóstico).
pub fn latest_session(
    home: &Path,
    project_root: &Path,
    now: SystemTime,
    window: Duration,
) -> Option<(SystemTime, DetectedSession)> {
    let roots = root_variants(project_root);
    let mut candidates: Vec<(SystemTime, DetectedSession)> = roots
        .iter()
        .flat_map(|root| {
            [
                detect_claude(home, root, now, window),
                detect_codex(home, root, now, window),
                detect_opencode(home, root, now, window),
            ]
        })
        .flatten()
        .collect();
    candidates.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    candidates.into_iter().next()
}

/// Ruta comparable: canónica si existe y sin `/` final.
pub fn normalize_path(path: &Path) -> PathBuf {
    let resolved = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed })
}

/// La raíz tal cual y su forma canónica (p. ej. `/tmp/x` y `/private/tmp/x`):
/// el agente guarda el `cwd` desde el que lo lanzaste, que puede ser cualquiera.
fn root_variants(root: &Path) -> Vec<PathBuf> {
    let text = root.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    let given = PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed });
    let canonical = normalize_path(root);
    if canonical == given { vec![given] } else { vec![given, canonical] }
}

// ------------------------------------------------------------------- Claude

/// Claude Code nombra la carpeta con el cwd, cambiando todo lo que no sea
/// alfanumérico por `-`.
pub fn claude_project_dir_name(root: &Path) -> String {
    root.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn detect_claude(home: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(SystemTime, DetectedSession)> {
    let base = home.join(".claude/projects");
    let dir = base.join(claude_project_dir_name(root));
    // La sesión más reciente entre la del propio proyecto y las de sus
    // worktrees de Claude Code (`<proyecto>/.claude/worktrees/<tarea>`).
    let (path, modified) = newest_active_file(&dir, "jsonl", now, window)
        .into_iter()
        .chain(claude_worktree_sessions(&base, root, now, window))
        .max_by_key(|(_, modified)| *modified)
        .or_else(|| claude_by_cwd(&base, root, now, window))?;
    let external_id = path.file_stem()?.to_str()?.to_owned();
    let lines = tail_lines(&path)?;

    let mut model = None;
    let mut prompt = None;
    let mut cwd = None;
    for line in lines.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(str::to_owned);
        }
        match v.get("type").and_then(Value::as_str) {
            Some("assistant") if model.is_none() => {
                model = v
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .filter(|m| !m.starts_with('<')) // "<synthetic>"
                    .map(str::to_owned);
            }
            Some("user") if prompt.is_none() => {
                if v.get("isMeta").and_then(Value::as_bool) != Some(true) {
                    prompt = user_text(v.pointer("/message/content"));
                }
            }
            _ => {}
        }
        if model.is_some() && prompt.is_some() {
            break;
        }
    }
    Some((
        modified,
        DetectedSession {
            tool: AgentTool::ClaudeCode,
            external_id: Some(external_id),
            model_name: model,
            last_prompt: prompt.map(|p| truncate(&p)),
            cwd: Some(cwd.map_or_else(|| root.display().to_string(), |c| normalize_path(Path::new(&c)).display().to_string())),
        },
    ))
}

/// `cwd` declarado en las últimas líneas de un `.jsonl` de Claude Code.
fn claude_session_cwd(path: &Path) -> Option<String> {
    tail_lines(path)?.iter().rev().find_map(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| v.get("cwd").and_then(Value::as_str).map(str::to_owned))
    })
}

/// Sesiones activas de los worktrees que Claude Code crea dentro del proyecto
/// (`<proyecto>/.claude/worktrees/<tarea>`). Su carpeta en `~/.claude/projects`
/// es la del proyecto seguida de `--claude-worktrees-<tarea>`; si el historial
/// trae `cwd`, se confirma que de verdad cae en ese worktree.
fn claude_worktree_sessions(
    base: &Path,
    root: &Path,
    now: SystemTime,
    window: Duration,
) -> Vec<(PathBuf, SystemTime)> {
    let prefix = format!("{}--claude-worktrees-", claude_project_dir_name(root));
    sorted_subdirs(base)
        .iter()
        .filter(|dir| dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&prefix)))
        .filter_map(|dir| newest_active_file(dir, "jsonl", now, window))
        .filter(|(path, _)| claude_session_cwd(path).is_none_or(|cwd| path_matches(&cwd, root)))
        .collect()
}

/// Plan B: si la carpeta codificada no existe (Claude se lanzó desde un
/// symlink, una subcarpeta o con `/` final), busca entre las sesiones activas
/// la que declare un `cwd` que, normalizado, caiga dentro del proyecto.
fn claude_by_cwd(base: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(PathBuf, SystemTime)> {
    sorted_subdirs(base)
        .iter()
        .filter_map(|dir| newest_active_file(dir, "jsonl", now, window))
        .filter(|(path, _)| claude_session_cwd(path).is_some_and(|cwd| path_matches(&cwd, root)))
        .max_by_key(|(_, modified)| *modified)
}

// -------------------------------------------------------------------- Codex

fn detect_codex(home: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(SystemTime, DetectedSession)> {
    let base = home.join(".codex/sessions");
    // Solo se miran los últimos días (la estructura es AAAA/MM/DD).
    let mut day_dirs = Vec::new();
    for year in sorted_subdirs(&base) {
        for month in sorted_subdirs(&year) {
            day_dirs.extend(sorted_subdirs(&month));
        }
    }
    day_dirs.sort();

    let mut best: Option<(SystemTime, PathBuf, String)> = None;
    for dir in day_dirs.iter().rev().take(3) {
        let Ok(entries) = fs::read_dir(dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(modified) = active_mtime(&path, now, window) else { continue };
            if best.as_ref().is_some_and(|(m, _, _)| *m >= modified) {
                continue;
            }
            if let Some(cwd) = codex_cwd(&path).filter(|cwd| path_matches(cwd, root)) {
                best = Some((modified, path, cwd));
            }
        }
    }
    let (modified, path, cwd) = best?;

    let first = read_first_line(&path)?;
    let meta: Value = serde_json::from_str(&first).ok()?;
    let external_id = meta.pointer("/payload/id").and_then(Value::as_str).map(str::to_owned);

    let mut model = None;
    let mut prompt = None;
    for line in tail_lines(&path)?.iter().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let payload = v.get("payload");
        match v.get("type").and_then(Value::as_str) {
            Some("turn_context") if model.is_none() => {
                model = payload
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
            }
            Some("event_msg") if prompt.is_none() => {
                if payload.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("user_message") {
                    prompt = payload
                        .and_then(|p| p.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
            Some("response_item") if prompt.is_none() => {
                if payload.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("message")
                    && payload.and_then(|p| p.get("role")).and_then(Value::as_str) == Some("user")
                {
                    prompt = user_text(payload.and_then(|p| p.get("content")));
                }
            }
            _ => {}
        }
        if model.is_some() && prompt.is_some() {
            break;
        }
    }
    Some((
        modified,
        DetectedSession {
            tool: AgentTool::Codex,
            external_id,
            model_name: model,
            last_prompt: prompt.map(|p| truncate(&p)),
            cwd: Some(normalize_path(Path::new(&cwd)).display().to_string()),
        },
    ))
}

fn codex_cwd(path: &Path) -> Option<String> {
    let v: Value = serde_json::from_str(&read_first_line(path)?).ok()?;
    v.pointer("/payload/cwd").and_then(Value::as_str).map(str::to_owned)
}

// ----------------------------------------------------------------- OpenCode

fn detect_opencode(home: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(SystemTime, DetectedSession)> {
    let base = home.join(".local/share/opencode");
    detect_opencode_db(&base.join("opencode.db"), root, now, window)
        .or_else(|| detect_opencode_storage(&base.join("storage"), root, now, window))
}

fn detect_opencode_db(db_path: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(SystemTime, DetectedSession)> {
    if !db_path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(Duration::from_millis(500)).ok()?;

    let now_ms = now.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis() as i64;
    let min_ms = now_ms - window.as_millis() as i64;
    let root_str = root.to_string_lossy().into_owned();
    let like = format!("{}/%", root_str.trim_end_matches('/').replace('%', "\\%").replace('_', "\\_"));

    let (id, updated_ms, directory): (String, i64, String) = conn
        .query_row(
            "SELECT id, time_updated, directory FROM session
             WHERE (directory = ?1 OR directory LIKE ?2 ESCAPE '\\')
               AND time_updated >= ?3 AND time_archived IS NULL
             ORDER BY time_updated DESC LIMIT 1",
            params![root_str, like, min_ms],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()?;

    let mut model = None;
    let mut prompt = None;
    let mut stmt = conn
        .prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created DESC LIMIT 40")
        .ok()?;
    let rows = stmt
        .query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .ok()?;
    for row in rows.flatten() {
        let (message_id, data) = row;
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        if model.is_none() {
            model = opencode_model(&v);
        }
        if prompt.is_none() && v.get("role").and_then(Value::as_str) == Some("user") {
            prompt = conn
                .prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created, id")
                .ok()
                .and_then(|mut s| {
                    s.query_map(params![message_id], |r| r.get::<_, String>(0))
                        .ok()
                        .map(|rows| rows.flatten().collect::<Vec<_>>())
                })
                .and_then(|parts| join_text_parts(parts.iter().filter_map(|p| serde_json::from_str::<Value>(p).ok())));
        }
        if model.is_some() && prompt.is_some() {
            break;
        }
    }

    let modified = SystemTime::UNIX_EPOCH + Duration::from_millis(updated_ms.max(0) as u64);
    Some((
        modified,
        DetectedSession {
            tool: AgentTool::Opencode,
            external_id: Some(id),
            model_name: model,
            last_prompt: prompt.map(|p| truncate(&p)),
            cwd: Some(normalize_path(Path::new(&directory)).display().to_string()),
        },
    ))
}

/// Formato anterior de OpenCode: ficheros JSON bajo `storage/`.
fn detect_opencode_storage(storage: &Path, root: &Path, now: SystemTime, window: Duration) -> Option<(SystemTime, DetectedSession)> {
    let mut best: Option<(SystemTime, String, String)> = None;
    for project_dir in sorted_subdirs(&storage.join("session")) {
        let Ok(entries) = fs::read_dir(&project_dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(modified) = active_mtime(&path, now, window) else { continue };
            if best.as_ref().is_some_and(|(m, _, _)| *m >= modified) {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else { continue };
            let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
            let directory = v.get("directory").and_then(Value::as_str).filter(|d| path_matches(d, root));
            if let (Some(directory), Some(id)) = (directory, v.get("id").and_then(Value::as_str)) {
                best = Some((modified, id.to_owned(), directory.to_owned()));
            }
        }
    }
    let (modified, id, directory) = best?;

    let mut model = None;
    let mut prompt = None;
    let mut messages: Vec<PathBuf> = fs::read_dir(storage.join("message").join(&id))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    messages.sort();
    for path in messages.iter().rev().take(40) {
        let Ok(text) = fs::read_to_string(path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        if model.is_none() {
            model = opencode_model(&v);
        }
        if prompt.is_none() && v.get("role").and_then(Value::as_str) == Some("user") {
            let message_id = path.file_stem()?.to_str()?;
            let mut parts: Vec<PathBuf> = fs::read_dir(storage.join("part").join(message_id))
                .map(|d| d.flatten().map(|e| e.path()).collect())
                .unwrap_or_default();
            parts.sort();
            prompt = join_text_parts(parts.iter().filter_map(|p| {
                fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            }));
        }
        if model.is_some() && prompt.is_some() {
            break;
        }
    }
    Some((
        modified,
        DetectedSession {
            tool: AgentTool::Opencode,
            external_id: Some(id),
            model_name: model,
            last_prompt: prompt.map(|p| truncate(&p)),
            cwd: Some(normalize_path(Path::new(&directory)).display().to_string()),
        },
    ))
}

fn opencode_model(message: &Value) -> Option<String> {
    message
        .get("modelID")
        .or_else(|| message.pointer("/model/modelID"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn join_text_parts(parts: impl Iterator<Item = Value>) -> Option<String> {
    let text: Vec<String> = parts
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        .filter(|p| p.get("synthetic").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect();
    let joined = text.join("\n");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

// ------------------------------------------------------------------ helpers

/// Texto de un mensaje de usuario (string o lista de bloques), descartando el
/// ruido que inyectan los propios agentes (contexto, avisos de comandos locales…).
fn user_text(content: Option<&Value>) -> Option<String> {
    let raw = match content? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| matches!(b.get("type").and_then(Value::as_str), Some("text" | "input_text")))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = raw.trim();
    let is_injected = text.is_empty()
        || text.starts_with('<')
        || text.starts_with("# AGENTS.md")
        || text.starts_with("Caveat:");
    (!is_injected).then(|| text.to_owned())
}

fn truncate(text: &str) -> String {
    let single_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= MAX_PROMPT_CHARS {
        single_line
    } else {
        let cut: String = single_line.chars().take(MAX_PROMPT_CHARS).collect();
        format!("{}…", cut.trim_end())
    }
}

fn active_mtime(path: &Path, now: SystemTime, window: Duration) -> Option<SystemTime> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let age = now.duration_since(modified).unwrap_or_default();
    (age <= window).then_some(modified)
}

/// Fichero `*.<ext>` más reciente de `dir` modificado dentro de la ventana activa.
fn newest_active_file(dir: &Path, ext: &str, now: SystemTime, window: Duration) -> Option<(PathBuf, SystemTime)> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
        .filter_map(|p| active_mtime(&p, now, window).map(|m| (p, m)))
        .max_by_key(|(_, m)| *m)
}

fn sorted_subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// `cwd` coincide con la raíz del proyecto o está dentro de ella. Se comparan
/// tal cual y normalizadas (canónicas, sin `/` final) para que un symlink o una
/// barra de más no rompan la coincidencia.
///
/// Los worktrees que Claude Code crea en `<proyecto>/.claude/worktrees/`
/// cuentan como del proyecto: el agente trabaja ahí y luego fusiona en la
/// rama del proyecto, y esos cambios deben atribuirse a su sesión. Cualquier
/// otra subcarpeta que sea un repositorio o worktree aparte (con su propio
/// `.git`) **no** cuenta: sus cambios no llegan a este proyecto.
fn path_matches(cwd: &str, root: &Path) -> bool {
    let trimmed = cwd.trim_end_matches('/');
    let raw = Path::new(if trimmed.is_empty() { "/" } else { trimmed });
    let (cwd, root) = if raw.starts_with(root) {
        (raw.to_path_buf(), root.to_path_buf())
    } else {
        (normalize_path(raw), normalize_path(root))
    };
    if !cwd.starts_with(&root) {
        return false;
    }
    if cwd.starts_with(root.join(CLAUDE_WORKTREES_DIR)) {
        return true;
    }
    !cwd.ancestors()
        .take_while(|dir| *dir != root.as_path())
        .any(|dir| dir.join(".git").exists())
}

fn read_first_line(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    File::open(path).ok()?.take(256 * 1024).read_to_end(&mut buf).ok()?;
    let end = buf.iter().position(|b| *b == b'\n').unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Últimas líneas completas del fichero (máx. `TAIL_BYTES`).
fn tail_lines(path: &Path) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.lines();
    if start > 0 {
        lines.next(); // primera línea probablemente cortada
    }
    Some(lines.filter(|l| !l.trim().is_empty()).map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("nude-sess-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&p).expect("mkdir");
            Self(p)
        }
        fn put(&self, rel: &str, body: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            fs::write(&p, body).expect("write");
            p
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root() -> PathBuf {
        PathBuf::from("/Users/dev/My Projects/app.v2")
    }

    #[test]
    fn claude_dir_name_replaces_non_alphanumerics() {
        assert_eq!(
            claude_project_dir_name(&root()),
            "-Users-dev-My-Projects-app-v2"
        );
    }

    #[test]
    fn no_history_means_manual() {
        let home = Tmp::new();
        assert_eq!(
            detect_active_session(&home.0, &root(), SystemTime::now()),
            DetectedSession::manual()
        );
    }

    #[test]
    fn detects_claude_session_model_and_last_real_prompt() {
        let home = Tmp::new();
        let dir = format!(".claude/projects/{}", claude_project_dir_name(&root()));
        let jsonl = [
            r#"{"type":"user","message":{"role":"user","content":"primer prompt"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"ok"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"  arregla   el\nbug de login  "}}"#,
            r#"{"type":"assistant","message":{"model":"<synthetic>","content":[]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            r#"{"type":"user","message":{"role":"user","content":"<local-command-caveat>ruido</local-command-caveat>"}}"#,
            "no es json",
        ]
        .join("\n");
        home.put(&format!("{dir}/aaaa-bbbb.jsonl"), &jsonl);

        let s = detect_active_session(&home.0, &root(), SystemTime::now());
        assert_eq!(s.tool, AgentTool::ClaudeCode);
        assert_eq!(s.external_id.as_deref(), Some("aaaa-bbbb"));
        assert_eq!(s.model_name.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(s.last_prompt.as_deref(), Some("arregla el bug de login"));
    }

    #[test]
    fn stale_sessions_are_ignored() {
        let home = Tmp::new();
        let dir = format!(".claude/projects/{}", claude_project_dir_name(&root()));
        home.put(&format!("{dir}/old.jsonl"), r#"{"type":"user","message":{"content":"hola"}}"#);
        let later = SystemTime::now() + ACTIVE_WINDOW + Duration::from_secs(60);
        assert_eq!(detect_active_session(&home.0, &root(), later).tool, AgentTool::Manual);
    }

    #[test]
    fn attribution_uses_15_minutes_and_the_firewall_only_2() {
        let home = Tmp::new();
        let dir = format!(".claude/projects/{}", claude_project_dir_name(&root()));
        home.put(&format!("{dir}/s.jsonl"), r#"{"type":"assistant","message":{"model":"m","content":[]}}"#);
        let ten_minutes_later = SystemTime::now() + Duration::from_secs(10 * 60);
        assert_eq!(detect_active_session(&home.0, &root(), ten_minutes_later).tool, AgentTool::ClaudeCode);
        let firewall = detect_session_within(&home.0, &root(), ten_minutes_later, Duration::from_secs(120));
        assert_eq!(firewall.tool, AgentTool::Manual);
        let (_, seen) = latest_session(&home.0, &root(), ten_minutes_later, Duration::from_secs(3600)).expect("diag");
        assert_eq!(seen.cwd.as_deref(), Some(root().to_str().expect("utf8")));
    }

    #[test]
    fn cwd_matching_survives_symlinks_trailing_slashes_and_subfolders() {
        let fs_root = Tmp::new();
        let real = fs::canonicalize(&fs_root.0).expect("canon").join("real");
        fs::create_dir_all(real.join("src")).expect("mkdir");
        let link = fs_root.0.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert!(path_matches(&format!("{}/", real.display()), &real));
        assert!(path_matches(&link.display().to_string(), &real));
        assert!(path_matches(&format!("{}/src/", link.display()), &real));
        assert!(!path_matches("/otro", &real));

        // Claude lanzado desde el symlink: su carpeta codificada no coincide con
        // la raíz canónica, pero el `cwd` del historial sí.
        let home = Tmp::new();
        let dir = format!(".claude/projects/{}", claude_project_dir_name(&link.join("src")));
        let line = format!(r#"{{"type":"assistant","cwd":"{}/src","message":{{"model":"m","content":[]}}}}"#, link.display());
        home.put(&format!("{dir}/s.jsonl"), &line);
        let s = detect_active_session(&home.0, &real, SystemTime::now());
        assert_eq!(s.tool, AgentTool::ClaudeCode);
        assert_eq!(s.cwd.as_deref(), Some(real.join("src").to_str().expect("utf8")));
    }

    #[test]
    fn claude_worktree_sessions_count_for_the_project_but_other_nested_repos_do_not() {
        let fs_root = Tmp::new();
        let root = fs::canonicalize(&fs_root.0).expect("canon");
        let worktree = root.join(".claude/worktrees/tarea");
        let vendor = root.join("vendor/lib");
        for repo in [&worktree, &vendor] {
            fs::create_dir_all(repo).expect("mkdir");
            fs::write(repo.join(".git"), "gitdir: elsewhere\n").expect("git file");
        }
        assert!(path_matches(&worktree.display().to_string(), &root));
        assert!(path_matches(&format!("{}/src/", worktree.display()), &root));
        assert!(path_matches(&worktree.display().to_string(), &worktree));
        assert!(!path_matches(&vendor.display().to_string(), &root), "otro repo anidado");

        // Codex: lo mismo por `cwd`.
        let home = Tmp::new();
        let meta = |id: &str, cwd: &Path| format!(r#"{{"type":"session_meta","payload":{{"id":"{id}","cwd":"{}"}}}}"#, cwd.display());
        home.put(".codex/sessions/2026/11/21/rollout-v.jsonl", &meta("cx-v", &vendor));
        assert_eq!(detect_active_session(&home.0, &root, SystemTime::now()).tool, AgentTool::Manual);
        home.put(".codex/sessions/2026/11/21/rollout-w.jsonl", &meta("cx-w", &worktree));
        let s = detect_active_session(&home.0, &root, SystemTime::now());
        assert_eq!((s.tool, s.external_id.as_deref()), (AgentTool::Codex, Some("cx-w")));
    }

    #[test]
    fn claude_worktree_session_is_used_unless_the_project_has_a_newer_one() {
        let fs_root = Tmp::new();
        let root = fs::canonicalize(&fs_root.0).expect("canon");
        let worktree = root.join(".claude/worktrees/opus-tarea");
        fs::create_dir_all(&worktree).expect("mkdir");
        let home = Tmp::new();
        let session = |cwd: &Path, model: &str, prompt: &str| {
            [
                format!(r#"{{"type":"user","cwd":"{}","message":{{"role":"user","content":"{prompt}"}}}}"#, cwd.display()),
                format!(r#"{{"type":"assistant","cwd":"{}","message":{{"model":"{model}","content":[]}}}}"#, cwd.display()),
            ]
            .join("\n")
        };
        let direct = home.put(
            &format!(".claude/projects/{}/directa.jsonl", claude_project_dir_name(&root)),
            &session(&root, "claude-sonnet-5-5", "arreglo directo"),
        );
        let in_worktree = home.put(
            &format!(".claude/projects/{}/wt.jsonl", claude_project_dir_name(&worktree)),
            &session(&worktree, "claude-opus-5-5", "fusiona la rama de la tarea"),
        );
        let now = SystemTime::now();
        let set_age = |path: &Path, secs: u64| {
            fs::File::options().write(true).open(path).expect("open")
                .set_modified(now - Duration::from_secs(secs)).expect("mtime");
        };

        // El worktree es lo más reciente: se atribuye a Claude Code con su modelo y prompt.
        set_age(&direct, 300);
        set_age(&in_worktree, 10);
        let s = detect_active_session(&home.0, &root, now);
        assert_eq!(s.tool, AgentTool::ClaudeCode);
        assert_eq!(s.external_id.as_deref(), Some("wt"));
        assert_eq!(s.model_name.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(s.last_prompt.as_deref(), Some("fusiona la rama de la tarea"));
        assert_eq!(s.cwd.as_deref(), Some(worktree.to_str().expect("utf8")));

        // Una sesión más reciente en el propio proyecto tiene prioridad.
        set_age(&direct, 5);
        let s = detect_active_session(&home.0, &root, now);
        assert_eq!(s.external_id.as_deref(), Some("directa"));
        assert_eq!(s.model_name.as_deref(), Some("claude-sonnet-5-5"));

        // Solo el worktree, sin sesión directa: también cuenta.
        fs::remove_file(&direct).expect("rm");
        assert_eq!(detect_active_session(&home.0, &root, now).external_id.as_deref(), Some("wt"));
    }

    #[test]
    fn detects_codex_session_by_cwd() {
        let home = Tmp::new();
        let meta = format!(r#"{{"type":"session_meta","payload":{{"id":"cx-1","cwd":"{}"}}}}"#, root().display());
        let body = [
            meta.as_str(),
            r#"{"type":"turn_context","payload":{"model":"gpt-5.3-codex"}}"#,
            r##"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"# AGENTS.md instructions"}]}}"##,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"añade tests"}]}}"#,
        ]
        .join("\n");
        home.put(".codex/sessions/2026/11/20/rollout-1.jsonl", &body);
        let other = r#"{"type":"session_meta","payload":{"id":"cx-2","cwd":"/otro/proyecto"}}"#;
        home.put(".codex/sessions/2026/11/20/rollout-2.jsonl", other);

        let s = detect_active_session(&home.0, &root(), SystemTime::now());
        assert_eq!(s.tool, AgentTool::Codex);
        assert_eq!(s.external_id.as_deref(), Some("cx-1"));
        assert_eq!(s.model_name.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(s.last_prompt.as_deref(), Some("añade tests"));
    }

    #[test]
    fn detects_opencode_session_from_sqlite() {
        let home = Tmp::new();
        let db_path = home.0.join(".local/share/opencode/opencode.db");
        fs::create_dir_all(db_path.parent().expect("parent")).expect("mkdir");
        let conn = Connection::open(&db_path).expect("db");
        conn.execute_batch(
            "CREATE TABLE session (id TEXT, directory TEXT, time_updated INTEGER, time_archived INTEGER);
             CREATE TABLE message (id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT, message_id TEXT, time_created INTEGER, data TEXT);",
        )
        .expect("schema");
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).expect("t").as_millis() as i64;
        conn.execute(
            "INSERT INTO session VALUES ('ses_1', ?1, ?2, NULL)",
            params![root().to_string_lossy(), now_ms],
        )
        .expect("session");
        conn.execute(
            "INSERT INTO message VALUES ('msg_u', 'ses_1', 1, ?1), ('msg_a', 'ses_1', 2, ?2)",
            params![
                r#"{"role":"user","model":{"providerID":"openai","modelID":"gpt-5.3-codex"}}"#,
                r#"{"role":"assistant","modelID":"gpt-5.3-codex"}"#
            ],
        )
        .expect("messages");
        conn.execute(
            "INSERT INTO part VALUES ('prt_1', 'msg_u', 1, ?1), ('prt_2', 'msg_u', 2, ?2)",
            params![r#"{"type":"text","text":"refactoriza el módulo db"}"#, r#"{"type":"reasoning","text":"x"}"#],
        )
        .expect("parts");
        drop(conn);

        let s = detect_active_session(&home.0, &root(), SystemTime::now());
        assert_eq!(s.tool, AgentTool::Opencode);
        assert_eq!(s.external_id.as_deref(), Some("ses_1"));
        assert_eq!(s.model_name.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(s.last_prompt.as_deref(), Some("refactoriza el módulo db"));
    }

    #[test]
    fn long_prompts_are_truncated() {
        let long = "a ".repeat(500);
        let t = truncate(&long);
        assert!(t.chars().count() <= MAX_PROMPT_CHARS + 1);
        assert!(t.ends_with('…'));
    }
}
