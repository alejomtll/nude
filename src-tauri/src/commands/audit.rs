//! Auditoría pre-merge local de un carril (estilo CodeRabbit, sin servidores).
//!
//! `audit_parallel_lane` toma el diff completo del carril contra la rama del
//! proyecto (commits + cambios sin confirmar + archivos nuevos sin trackear),
//! se lo pasa al CLI de agente instalado en el equipo (`claude -p` o
//! `codex exec`) en modo no interactivo y de solo lectura, con el shell de
//! login del usuario (`$SHELL -l -i`, sus credenciales y su `PATH`), y
//! devuelve un reporte estructurado. Nada de API keys ni red propia: lo que
//! haga el CLI es cosa de su propia configuración (AGENTS.md §2).
//!
//! El CLI se ejecuta en una carpeta temporal vacía, no en el worktree: así su
//! transcripción no se confunde con la sesión del agente del carril (el
//! detector de sesiones busca por `cwd`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use super::blocking;
use super::lanes::active_lane;
use crate::db::Database;
use crate::db::models::{ChangeType, new_id};
use crate::error::{NudeError, NudeResult};
use crate::git;
use crate::pty::login_shell;
use crate::safety;

/// Límite total de la auditoría (arranque del shell incluido).
pub const AUDIT_TIMEOUT: Duration = Duration::from_secs(45);
/// Caracteres de diff que se mandan como máximo al CLI.
const MAX_PROMPT_DIFF: usize = 120_000;
/// Caracteres de parche por archivo.
const MAX_FILE_PATCH: usize = 24_000;
/// Hallazgos que se conservan.
const MAX_FINDINGS: usize = 5;
/// Salida que se lee del CLI como máximo.
const MAX_CLI_OUTPUT: usize = 2 * 1024 * 1024;

/// CLI que hace la auditoría.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AuditorCli {
    #[serde(alias = "claude-code")]
    Claude,
    Codex,
    /// El del agente del carril si es uno de los dos; si no está instalado, el otro.
    Auto,
}

impl AuditorCli {
    fn binary(self) -> &'static str {
        match self {
            Self::Claude | Self::Auto => "claude",
            Self::Codex => "codex",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Claude | Self::Auto => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AuditVerdict {
    Ready,
    Review,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum FindingSeverity {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuditFinding {
    pub severity: FindingSeverity,
    pub file: Option<String>,
    pub message: String,
}

/// Lo que se extrae de la respuesta del CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAudit {
    pub summary: String,
    pub verdict: AuditVerdict,
    pub findings: Vec<AuditFinding>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditReport {
    pub lane_id: String,
    /// `claude` o `codex`.
    pub auditor: AuditorCli,
    pub summary: String,
    pub verdict: AuditVerdict,
    pub findings: Vec<AuditFinding>,
    pub files_reviewed: usize,
    /// El diff se recortó para caber en el prompt.
    pub truncated: bool,
    pub duration_ms: u64,
    /// Milisegundos Unix.
    pub created_at: i64,
}

// -------------------------------------------------------------------- prompt

struct Prompt {
    text: String,
    files: usize,
    truncated: bool,
}

fn change_marker(change: ChangeType) -> &'static str {
    match change {
        ChangeType::Added => "A",
        ChangeType::Modified => "M",
        ChangeType::Deleted => "D",
    }
}

fn cut_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((at, _)) => (&text[..at], true),
        None => (text, false),
    }
}

fn build_prompt(lane_name: &str, lane_branch: &str, diff: &git::LaneDiff) -> Prompt {
    let base = diff.base_branch.as_deref().unwrap_or("la rama actual");
    let mut text = format!(
        "Eres un revisor de código senior. Audita los cambios del carril «{lane_name}» (rama {lane_branch}) \
         que se van a fusionar sobre {base}. Trabaja en modo solo lectura: no modifiques archivos ni ejecutes comandos.\n\n\
         Responde ÚNICAMENTE con un objeto JSON válido, sin markdown ni texto antes o después, con esta forma exacta:\n\
         {{\"summary\": \"…\", \"verdict\": \"ready\" | \"review\", \"findings\": [{{\"severity\": \"high\" | \"medium\" | \"low\", \"file\": \"ruta/relativa\", \"message\": \"…\"}}]}}\n\n\
         Reglas:\n\
         - summary: resumen conciso de 2 o 3 líneas en español de qué cambia y su riesgo.\n\
         - verdict: \"ready\" si se puede aplicar tal cual; \"review\" si algo necesita revisión humana antes.\n\
         - findings: como máximo {MAX_FINDINGS}, de mayor a menor severidad, sobre posibles bugs, regresiones, \
           archivos sensibles (credenciales, dependencias, migraciones, despliegue) o falta de pruebas. \
           Lista vacía si no hay nada relevante. message en español, una o dos frases concretas; file es la ruta \
           del archivo afectado o null.\n\n"
    );

    let sensitive: Vec<String> = diff
        .files
        .iter()
        .filter_map(|f| safety::sensitive_reason(&f.file_path).map(|(_, why)| format!("- {}: {why}", f.file_path)))
        .collect();
    if !sensitive.is_empty() {
        text.push_str("Archivos sensibles detectados por Nude:\n");
        text.push_str(&sensitive.join("\n"));
        text.push_str("\n\n");
    }
    if !diff.conflicts.is_empty() {
        text.push_str(&format!("Archivos que también cambiaron en {base} (conflicto al fusionar):\n"));
        for path in &diff.conflicts {
            text.push_str(&format!("- {path}\n"));
        }
        text.push('\n');
    }

    text.push_str(&format!("Archivos cambiados ({}):\n", diff.files.len()));
    for file in &diff.files {
        text.push_str(&format!("{} {}\n", change_marker(file.change_type), file.file_path));
    }
    text.push_str("\nDiff unificado:\n");

    let mut budget = MAX_PROMPT_DIFF;
    let mut truncated = false;
    let mut omitted = Vec::new();
    for file in &diff.files {
        let Some(patch) = file.patch_text.as_deref() else {
            text.push_str(&format!("\n--- {} (binario o demasiado grande; sin parche)\n", file.file_path));
            continue;
        };
        if budget == 0 {
            omitted.push(file.file_path.as_str());
            continue;
        }
        let (patch, cut_file) = cut_chars(patch, MAX_FILE_PATCH.min(budget));
        budget -= patch.chars().count();
        text.push('\n');
        text.push_str(patch);
        if !patch.ends_with('\n') {
            text.push('\n');
        }
        if cut_file {
            truncated = true;
            text.push_str(&format!("[… parche de {} recortado por tamaño]\n", file.file_path));
        }
    }
    if !omitted.is_empty() {
        truncated = true;
        text.push_str(&format!("\n[Sin parche por tamaño del diff: {}]\n", omitted.join(", ")));
    }
    Prompt { text, files: diff.files.len(), truncated }
}

// -------------------------------------------------------------------- parser

/// Objeto JSON balanceado que empieza en `start` (una `{`), respetando cadenas.
fn object_at(text: &str, start: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let (mut depth, mut in_string, mut escaped) = (0usize, false, false);
    for (offset, &b) in bytes[start..].iter().enumerate() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Bloques ```…``` (sin la etiqueta de lenguaje).
fn fenced_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let body_start = after.find('\n').map_or(after.len(), |nl| nl + 1);
        let body = &after[body_start..];
        let Some(close) = body.find("```") else { break };
        blocks.push(&body[..close]);
        rest = &body[close + 3..];
    }
    blocks
}

/// Quita comas finales (`[1, 2,]`, `{"a": 1,}`) fuera de cadenas.
fn strip_trailing_commas(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn parse_object(candidate: &str) -> Option<serde_json::Map<String, Value>> {
    let candidate = candidate.trim();
    let value = serde_json::from_str::<Value>(candidate)
        .ok()
        .or_else(|| serde_json::from_str::<Value>(&strip_trailing_commas(candidate)).ok())?;
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn severity_of(value: Option<&Value>) -> FindingSeverity {
    let text = value.and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
    match text.as_str() {
        "high" | "critical" | "blocker" | "major" | "error" | "alta" | "alto" | "crítica" | "critica" => {
            FindingSeverity::High
        }
        "low" | "minor" | "info" | "nit" | "trivial" | "baja" | "bajo" | "menor" => FindingSeverity::Low,
        _ => FindingSeverity::Medium,
    }
}

fn first_str<'a>(map: &'a serde_json::Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| map.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
}

fn finding_of(value: &Value) -> Option<AuditFinding> {
    match value {
        Value::String(message) if !message.trim().is_empty() => Some(AuditFinding {
            severity: FindingSeverity::Medium,
            file: None,
            message: message.trim().to_owned(),
        }),
        Value::Object(map) => {
            let message = first_str(map, &["message", "description", "detail", "text", "issue", "mensaje"])?;
            let file = first_str(map, &["file", "path", "filename", "archivo"])
                .filter(|f| !f.eq_ignore_ascii_case("null"))
                .map(str::to_owned);
            let severity = severity_of(map.get("severity").or_else(|| map.get("level")).or_else(|| map.get("severidad")));
            Some(AuditFinding { severity, file, message: message.to_owned() })
        }
        _ => None,
    }
}

/// Valida y normaliza un objeto candidato. `None` si no es un reporte.
fn normalize(map: &serde_json::Map<String, Value>) -> Option<ParsedAudit> {
    // Envoltorio de `claude -p --output-format json`: {"type":"result","result":"…"}.
    if !map.contains_key("verdict") && !map.contains_key("summary") {
        let inner = map.get("result").and_then(Value::as_str)?;
        return parse_audit_response(inner).ok();
    }
    let mut findings: Vec<AuditFinding> = map
        .get("findings")
        .or_else(|| map.get("issues"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(finding_of).collect())
        .unwrap_or_default();
    findings.sort_by_key(|f| f.severity);
    findings.truncate(MAX_FINDINGS);

    let verdict_text = map.get("verdict").and_then(Value::as_str).unwrap_or_default().trim().to_lowercase();
    let mut verdict = match verdict_text.as_str() {
        "ready" | "approve" | "approved" | "lgtm" | "ok" | "pass" | "listo" | "aprobado" => AuditVerdict::Ready,
        // Desconocido o ausente: lo prudente es revisar.
        _ => AuditVerdict::Review,
    };
    // Un hallazgo grave nunca se aplica "tal cual".
    if findings.iter().any(|f| f.severity == FindingSeverity::High) {
        verdict = AuditVerdict::Review;
    }

    let summary = first_str(map, &["summary", "resumen"]).unwrap_or_default();
    let summary = match summary {
        "" if verdict == AuditVerdict::Ready => "Sin observaciones.".to_owned(),
        "" => "El auditor no dio resumen.".to_owned(),
        text => {
            let (text, cut) = cut_chars(text, 600);
            if cut { format!("{text}…") } else { text.to_owned() }
        }
    };
    Some(ParsedAudit { summary, verdict, findings })
}

/// Extrae el reporte de la respuesta del CLI, aunque venga envuelta en
/// markdown, con texto alrededor o con comas finales.
pub fn parse_audit_response(text: &str) -> Result<ParsedAudit, String> {
    let text = text.trim().trim_start_matches('\u{feff}');
    let mut candidates: Vec<&str> = vec![text];
    candidates.extend(fenced_blocks(text).into_iter().rev());
    // Objetos sueltos en el texto: el último completo suele ser la respuesta.
    let starts: Vec<usize> = text.match_indices('{').map(|(i, _)| i).take(500).collect();
    candidates.extend(starts.into_iter().rev().filter_map(|start| object_at(text, start)));

    for candidate in candidates {
        if let Some(parsed) = parse_object(candidate).as_ref().and_then(normalize) {
            return Ok(parsed);
        }
    }
    let (snippet, cut) = cut_chars(text, 160);
    Err(format!("la respuesta no contenía el JSON pedido: «{}{}»", snippet.replace('\n', " "), if cut { "…" } else { "" }))
}

// ------------------------------------------------------------- ejecución CLI

#[derive(Debug, PartialEq, Eq)]
enum RunError {
    NotInstalled,
    Timeout,
    Failed(String),
}

/// Script para `-c`: comprueba que el binario existe (127 si no) y lo
/// sustituye (`exec`) con el prompt por stdin.
fn auditor_script(cli: AuditorCli) -> String {
    let binary = cli.binary();
    let run = match cli {
        AuditorCli::Codex => {
            "exec codex exec --sandbox read-only --skip-git-repo-check --color never --output-last-message last.txt - < prompt.txt"
        }
        AuditorCli::Claude | AuditorCli::Auto => {
            "exec claude -p --permission-mode plan --output-format text \
             --disallowedTools 'Bash,Edit,Write,MultiEdit,NotebookEdit,WebFetch,WebSearch' < prompt.txt"
        }
    };
    format!("command -v {binary} >/dev/null 2>&1 || exit 127; {run}")
}

/// Lee un pipe en segundo plano (hasta `MAX_CLI_OUTPUT`).
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) if buf.len() < MAX_CLI_OUTPUT => buf.extend_from_slice(&chunk[..n]),
                    Ok(_) => {} // se sigue leyendo para no bloquear al proceso
                }
            }
            let _ = tx.send(buf);
        });
    }
    rx
}

fn collect(rx: &mpsc::Receiver<Vec<u8>>) -> String {
    String::from_utf8_lossy(&rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default()).into_owned()
}

#[cfg(unix)]
fn kill_group(child: &std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `kill(2)` no toca memoria; `-pid` es el grupo que creamos
        // para este proceso (`process_group(0)`).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

/// Ejecuta el auditor en `workdir` (que ya contiene `prompt.txt`).
/// `command` construye el proceso a partir del script (shell de login en
/// producción, `/bin/sh` en los tests).
fn run_cli(cli: AuditorCli, workdir: &Path, timeout: Duration, command: &dyn Fn(&str) -> Command) -> Result<String, RunError> {
    let mut cmd = command(&auditor_script(cli));
    cmd.current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("NUDE_AUDIT", "1");
    if std::env::var_os("LANG").is_none() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| RunError::Failed(format!("no se pudo lanzar el shell: {e}")))?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                #[cfg(unix)]
                kill_group(&child);
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(RunError::Failed(e.to_string())),
        }
    };
    // Hijos que el CLI dejara colgados en su grupo.
    #[cfg(unix)]
    kill_group(&child);

    let out = collect(&stdout);
    let err = collect(&stderr);
    match status.code() {
        Some(0) => {
            let last = std::fs::read_to_string(workdir.join("last.txt")).unwrap_or_default();
            Ok(if last.trim().is_empty() { out } else { last })
        }
        Some(127) => Err(RunError::NotInstalled),
        code => {
            let detail = err
                .lines()
                .chain(out.lines())
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .unwrap_or("sin detalle");
            let (detail, _) = cut_chars(detail, 300);
            let code = code.map_or("una señal".to_owned(), |c| format!("el código {c}"));
            Err(RunError::Failed(format!("terminó con {code}: {detail}")))
        }
    }
}

/// Carpeta temporal propia de una auditoría; se borra al terminar.
struct Workdir(PathBuf);

impl Workdir {
    fn new(prompt: &str) -> NudeResult<Self> {
        let path = std::env::temp_dir().join(format!("nude-audit-{}", new_id()));
        std::fs::create_dir_all(&path)?;
        let dir = Self(path);
        std::fs::write(dir.0.join("prompt.txt"), prompt)?;
        Ok(dir)
    }
}

impl Drop for Workdir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn login_command(script: &str) -> Command {
    let mut cmd = Command::new(login_shell());
    cmd.args(["-l", "-i", "-c", script]);
    cmd
}

/// Orden en que se prueban los CLI.
fn candidates(requested: AuditorCli, agent_hint: Option<&str>) -> Vec<AuditorCli> {
    match requested {
        AuditorCli::Auto if agent_hint == Some("codex") => vec![AuditorCli::Codex, AuditorCli::Claude],
        AuditorCli::Auto => vec![AuditorCli::Claude, AuditorCli::Codex],
        cli => vec![cli],
    }
}

/// Ejecuta la auditoría con el primer CLI disponible de `order`.
fn audit_with(
    order: &[AuditorCli],
    prompt: &str,
    timeout: Duration,
    command: &dyn Fn(&str) -> Command,
) -> NudeResult<(AuditorCli, ParsedAudit)> {
    let started = Instant::now();
    let mut missing = Vec::new();
    for &cli in order {
        let left = timeout.saturating_sub(started.elapsed());
        let workdir = Workdir::new(prompt)?;
        match run_cli(cli, &workdir.0, left, command) {
            Ok(output) => {
                let parsed = parse_audit_response(&output)
                    .map_err(|e| NudeError::Audit(format!("{}: {e}", cli.label())))?;
                return Ok((cli, parsed));
            }
            Err(RunError::NotInstalled) => missing.push(cli),
            Err(RunError::Timeout) => {
                return Err(NudeError::Audit(format!(
                    "{} tardó más de {} s y se canceló la auditoría.",
                    cli.label(),
                    timeout.as_secs()
                )));
            }
            Err(RunError::Failed(detail)) => {
                return Err(NudeError::Audit(format!("{} no pudo auditar: {detail}", cli.label())));
            }
        }
    }
    let names: Vec<String> = missing.iter().map(|cli| format!("`{}`", cli.binary())).collect();
    Err(NudeError::Audit(format!(
        "No se encontró {} en tu PATH. Instala Claude Code (npm install -g @anthropic-ai/claude-code) \
         o Codex (npm install -g @openai/codex) para auditar.",
        names.join(" ni ")
    )))
}

pub(crate) fn audit_lane(db: &Database, lane_id: &str, requested: AuditorCli, timeout: Duration) -> NudeResult<AuditReport> {
    let lane = active_lane(db, lane_id)?;
    let project = super::project_or_404(db, &lane.project_id)?;
    let diff = git::lane_diff(Path::new(&project.root_path), Path::new(&lane.worktree_path))?;
    if diff.files.is_empty() {
        return Err(NudeError::Audit("El carril no tiene cambios que auditar.".into()));
    }
    let prompt = build_prompt(&lane.name, &lane.branch_name, &diff);
    let started = Instant::now();
    let order = candidates(requested, lane.agent_hint.as_deref());
    let (auditor, parsed) = audit_with(&order, &prompt.text, timeout, &login_command)?;
    Ok(AuditReport {
        lane_id: lane.id,
        auditor,
        summary: parsed.summary,
        verdict: parsed.verdict,
        findings: parsed.findings,
        files_reviewed: prompt.files,
        truncated: prompt.truncated,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        created_at: chrono::Utc::now().timestamp_millis(),
    })
}

/// Audita el carril con `claude -p` o `codex exec` (solo lectura, ≤ 45 s).
#[tauri::command]
pub async fn audit_parallel_lane(
    db: State<'_, Database>,
    lane_id: String,
    auditor_cli: Option<AuditorCli>,
) -> NudeResult<AuditReport> {
    let db = db.inner().clone();
    blocking(move || audit_lane(&db, &lane_id, auditor_cli.unwrap_or(AuditorCli::Auto), AUDIT_TIMEOUT)).await
}

#[cfg(test)]
mod tests;
