//! Terminales reales (PTY) por carril paralelo.
//!
//! Cada carril puede tener una sesión: un shell de login interactivo del
//! usuario (`$SHELL -l -i`, para heredar el `PATH` de Homebrew, nvm, cargo…)
//! abierto en su worktree que, opcionalmente, lanza `claude`, `codex` u
//! `opencode` y al terminar deja al usuario en el shell.
//!
//! La salida se emite como eventos (`lane-pty-output`) y se guarda en un
//! historial acotado para que la UI pueda volver a engancharse a una sesión
//! viva (cambiar de vista no la mata). Cada evento lleva el id de sesión y el
//! offset final en bytes de ese trozo: el frontend descarta lo que ya pintó
//! con el historial.
//!
//! Un hilo de supervisión clasifica cada sesión (`activity.rs`) en
//! `working`, `needs_attention` o `exited` y emite `lane-pty-status` cuando
//! cambia.
//!
//! Solo procesos locales; nada de red (AGENTS.md §2).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};

use crate::error::{NudeError, NudeResult};

mod activity;

use activity::ActivityTracker;
pub use activity::LaneActivity;

/// Evento Tauri con un trozo de salida (`PtyOutput`).
pub const EVENT_PTY_OUTPUT: &str = "lane-pty-output";
/// Evento Tauri cuando termina el proceso de una sesión (`PtyExit`).
pub const EVENT_PTY_EXIT: &str = "lane-pty-exit";
/// Evento Tauri cuando cambia el estado en vivo de una sesión (`PtyActivityChange`).
pub const EVENT_PTY_STATUS: &str = "lane-pty-status";
/// Cada cuánto revisa el supervisor si alguna sesión espera respuesta.
const MONITOR_INTERVAL: Duration = Duration::from_millis(250);

/// Historial que se conserva por sesión para volver a engancharse.
const MAX_SCROLLBACK: usize = 512 * 1024;
/// Tras `SIGHUP`, cuánto se espera antes de forzar con `SIGKILL`.
const KILL_GRACE: Duration = Duration::from_millis(1500);

/// Qué se ejecuta en el terminal del carril.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum LaneProgram {
    #[serde(alias = "claude")]
    ClaudeCode,
    Codex,
    Opencode,
    /// Solo el shell de login.
    Shell,
}

impl LaneProgram {
    /// `agent_hint` de un carril (`claude-code`, `codex`, `opencode`).
    pub fn from_hint(hint: &str) -> Option<Self> {
        match hint {
            "claude-code" | "claude" => Some(Self::ClaudeCode),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::Opencode),
            _ => None,
        }
    }

    fn binary(self) -> Option<&'static str> {
        match self {
            Self::ClaudeCode => Some("claude"),
            Self::Codex => Some("codex"),
            Self::Opencode => Some("opencode"),
            Self::Shell => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::Opencode => "OpenCode",
            Self::Shell => "Shell",
        }
    }

    /// Líneas que se enseñan en el propio terminal si falta el binario.
    fn install_hint(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[
                "  Instálalo:   npm install -g @anthropic-ai/claude-code",
                "  O pulsa «Claude» en la cabecera del carril para abrir la app nativa.",
            ],
            Self::Codex => &[
                "  Instálalo:   npm install -g @openai/codex   (o: brew install --cask codex)",
                "  O pulsa «Codex» en la cabecera del carril para abrir la app nativa.",
            ],
            Self::Opencode => &[
                "  Instálalo:   npm install -g opencode-ai   (o: brew install sst/tap/opencode)",
                "  O abre la carpeta del carril en tu editor con los botones de la cabecera.",
            ],
            Self::Shell => &[],
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PtyOutput {
    pub lane_id: String,
    pub session_id: u64,
    pub data: String,
    /// Offset (bytes UTF-8 desde el inicio de la sesión) al final de `data`.
    pub end: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PtyExit {
    pub lane_id: String,
    pub session_id: u64,
    pub exit_code: Option<u32>,
}

/// Cambio de estado en vivo (`working` → `needs_attention`…).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PtyActivityChange {
    pub lane_id: String,
    pub session_id: u64,
    pub program: LaneProgram,
    pub activity: LaneActivity,
}

/// Lo que el gestor comunica a la capa Tauri.
pub enum PtyEvent<'a> {
    Output(&'a PtyOutput),
    Exit(&'a PtyExit),
    Status(&'a PtyActivityChange),
}

/// Estado resumido (para `list_parallel_lanes`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PtyStatus {
    pub session_id: u64,
    pub program: LaneProgram,
    pub running: bool,
    pub exit_code: Option<u32>,
    pub pid: Option<u32>,
    /// Estado en vivo (`working`, `needs_attention`, `exited`).
    pub activity: LaneActivity,
}

/// Sesión a la que se engancha la UI: estado + historial hasta `end`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PtyAttach {
    pub lane_id: String,
    #[serde(flatten)]
    pub status: PtyStatus,
    /// `false` si se creó en esta llamada.
    pub reused: bool,
    pub scrollback: String,
    pub end: u64,
}

#[derive(Default)]
struct Output {
    text: String,
    /// Bytes emitidos desde el inicio de la sesión.
    end: u64,
    /// Final de una secuencia UTF-8 partida entre dos lecturas.
    carry: Vec<u8>,
}

impl Output {
    /// Decodifica `bytes` (más lo pendiente) sin partir caracteres.
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let mut text = String::new();
        let mut rest: &[u8] = &self.carry;
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    text.push_str(valid);
                    rest = &[];
                    break;
                }
                Err(e) => {
                    let (valid, after) = rest.split_at(e.valid_up_to());
                    text.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        // Secuencia incompleta al final: se espera al resto.
                        None => {
                            rest = after;
                            break;
                        }
                        Some(len) => {
                            text.push('\u{FFFD}');
                            rest = &after[len..];
                        }
                    }
                }
            }
        }
        self.carry = rest.to_vec();
        text
    }

    fn append(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        self.end += chunk.len() as u64;
        if self.text.len() > MAX_SCROLLBACK {
            // Se recorta por el principio, desde un salto de línea si lo hay.
            let mut cut = self.text.len() - MAX_SCROLLBACK;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            let cut = self.text[cut..].find('\n').map_or(cut, |nl| cut + nl + 1);
            self.text.drain(..cut);
        }
    }
}

struct Session {
    id: u64,
    program: LaneProgram,
    pid: Option<u32>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    output: Mutex<Output>,
    /// `None` mientras el proceso sigue vivo.
    exit: Mutex<Option<Option<u32>>>,
    activity: Mutex<ActivityTracker>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl Session {
    fn status(&self) -> PtyStatus {
        let exit = *lock(&self.exit);
        let activity = if exit.is_some() { LaneActivity::Exited } else { lock(&self.activity).state() };
        PtyStatus {
            session_id: self.id,
            program: self.program,
            running: exit.is_none(),
            exit_code: exit.flatten(),
            pid: self.pid,
            activity,
        }
    }

    fn attach(&self, lane_id: &str, reused: bool) -> PtyAttach {
        let output = lock(&self.output);
        PtyAttach {
            lane_id: lane_id.to_owned(),
            status: self.status(),
            reused,
            scrollback: output.text.clone(),
            end: output.end,
        }
    }
}

type Sink = dyn Fn(PtyEvent<'_>) + Send + Sync;

struct Inner {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    next_id: AtomicU64,
    sink: Box<Sink>,
}

/// Gestor de terminales de los carriles. Barato de clonar.
#[derive(Clone)]
pub struct PtyManager {
    inner: Arc<Inner>,
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows: rows.clamp(2, 500), cols: cols.clamp(10, 1000), pixel_width: 0, pixel_height: 0 }
}

fn pty_error(error: impl std::fmt::Display) -> NudeError {
    NudeError::Pty(error.to_string())
}

impl PtyManager {
    /// `sink` recibe la salida y los finales de proceso (la capa Tauri los
    /// emite como eventos). Se llama desde hilos de lectura.
    pub fn new(sink: impl Fn(PtyEvent<'_>) + Send + Sync + 'static) -> Self {
        let manager = Self {
            inner: Arc::new(Inner {
                sessions: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                sink: Box::new(sink),
            }),
        };
        manager.spawn_monitor();
        manager
    }

    /// Supervisor: detecta las sesiones calladas que esperan respuesta. Se
    /// apaga solo cuando el gestor deja de existir.
    fn spawn_monitor(&self) {
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let spawned = std::thread::Builder::new().name("nude-pty-monitor".into()).spawn(move || {
            loop {
                std::thread::sleep(MONITOR_INTERVAL);
                let Some(inner) = weak.upgrade() else { return };
                let sessions: Vec<(String, Arc<Session>)> =
                    lock(&inner.sessions).iter().map(|(lane, s)| (lane.clone(), s.clone())).collect();
                let now = Instant::now();
                for (lane_id, session) in sessions {
                    let change = lock(&session.activity).tick(now);
                    if let Some(activity) = change {
                        emit_activity(&inner, &lane_id, &session, activity);
                    }
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("nude: no se pudo supervisar los terminales: {e}");
        }
    }

    /// Abre (o reutiliza, si sigue viva) la sesión del carril en `cwd`
    /// lanzando `program` desde el shell de login del usuario.
    pub fn spawn_program(&self, lane_id: &str, cwd: &Path, program: LaneProgram, cols: u16, rows: u16) -> NudeResult<PtyAttach> {
        if let Some(attached) = self.reuse(lane_id, cols, rows) {
            return Ok(attached);
        }
        if !cwd.is_dir() {
            return Err(NudeError::NotFound(format!("la carpeta del carril {} ya no existe", cwd.display())));
        }
        let shell = login_shell();
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.arg("-i");
        if program != LaneProgram::Shell {
            cmd.arg("-c");
            cmd.arg(agent_script(program, &shell));
        }
        cmd.cwd(cwd);
        apply_terminal_env(&mut cmd);
        self.spawn_command(lane_id, program, cmd, cols, rows)
    }

    /// Sesión viva del carril, redimensionada a la vista que se engancha.
    fn reuse(&self, lane_id: &str, cols: u16, rows: u16) -> Option<PtyAttach> {
        let session = lock(&self.inner.sessions).get(lane_id).cloned()?;
        if !session.status().running {
            return None;
        }
        let _ = self.resize(lane_id, cols, rows);
        Some(session.attach(lane_id, true))
    }

    /// Lanza `cmd` en un PTY nuevo para el carril. Una sesión terminada del
    /// mismo carril se sustituye.
    pub(crate) fn spawn_command(
        &self,
        lane_id: &str,
        program: LaneProgram,
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
    ) -> NudeResult<PtyAttach> {
        if let Some(attached) = self.reuse(lane_id, cols, rows) {
            return Ok(attached);
        }
        let pair = native_pty_system().openpty(size(cols, rows)).map_err(pty_error)?;
        let child = pair.slave.spawn_command(cmd).map_err(pty_error)?;
        // Sin esta copia del esclavo, el lector recibe EOF cuando muere el proceso.
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(pty_error)?;
        let writer = pair.master.take_writer().map_err(pty_error)?;

        let session = Arc::new(Session {
            id: self.inner.next_id.fetch_add(1, Ordering::Relaxed),
            program,
            pid: child.process_id(),
            master: Mutex::new(Some(pair.master)),
            writer: Mutex::new(Some(writer)),
            killer: Mutex::new(child.clone_killer()),
            output: Mutex::new(Output::default()),
            exit: Mutex::new(None),
            activity: Mutex::new(ActivityTracker::new()),
        });
        let previous = lock(&self.inner.sessions).insert(lane_id.to_owned(), session.clone());
        if let Some(previous) = previous {
            terminate(&previous);
        }

        self.spawn_reader(lane_id, session.clone(), reader);
        self.spawn_waiter(lane_id, session.clone(), child);
        Ok(session.attach(lane_id, false))
    }

    fn spawn_reader(&self, lane_id: &str, session: Arc<Session>, mut reader: Box<dyn Read + Send>) {
        let inner = self.inner.clone();
        let lane_id = lane_id.to_owned();
        let spawned = std::thread::Builder::new().name(format!("nude-pty-read-{}", session.id)).spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut output = lock(&session.output);
                        let data = output.decode(&buf[..n]);
                        if data.is_empty() {
                            continue;
                        }
                        output.append(&data);
                        let event = PtyOutput { lane_id: lane_id.clone(), session_id: session.id, data, end: output.end };
                        // Se emite con el historial bloqueado: un enganche
                        // simultáneo ve el trozo en el historial o en el evento.
                        (inner.sink)(PtyEvent::Output(&event));
                        drop(output);
                        let change = lock(&session.activity).on_output(&event.data, Instant::now());
                        if let Some(activity) = change {
                            emit_activity(&inner, &lane_id, &session, activity);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break, // EIO en macOS al cerrarse el esclavo
                }
            }
        });
        if let Err(e) = spawned {
            eprintln!("nude: no se pudo leer el terminal del carril: {e}");
        }
    }

    fn spawn_waiter(&self, lane_id: &str, session: Arc<Session>, mut child: Box<dyn portable_pty::Child + Send + Sync>) {
        let inner = self.inner.clone();
        let lane_id = lane_id.to_owned();
        let spawned = std::thread::Builder::new().name(format!("nude-pty-wait-{}", session.id)).spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            *lock(&session.exit) = Some(code);
            // Ya nadie escribe: se libera el PTY (y el lector termina).
            lock(&session.writer).take();
            lock(&session.master).take();
            let event = PtyExit { lane_id: lane_id.clone(), session_id: session.id, exit_code: code };
            (inner.sink)(PtyEvent::Exit(&event));
            let change = lock(&session.activity).on_exit();
            if let Some(activity) = change {
                emit_activity(&inner, &lane_id, &session, activity);
            }
        });
        if let Err(e) = spawned {
            eprintln!("nude: no se pudo vigilar el terminal del carril: {e}");
        }
    }

    /// Estado de la sesión del carril, sin historial.
    pub fn status(&self, lane_id: &str) -> Option<PtyStatus> {
        lock(&self.inner.sessions).get(lane_id).map(|s| s.status())
    }

    /// Engancharse a la sesión existente (viva o terminada) sin lanzar nada.
    pub fn attach(&self, lane_id: &str) -> Option<PtyAttach> {
        lock(&self.inner.sessions).get(lane_id).map(|s| s.attach(lane_id, true))
    }

    fn session(&self, lane_id: &str) -> NudeResult<Arc<Session>> {
        lock(&self.inner.sessions)
            .get(lane_id)
            .cloned()
            .ok_or_else(|| NudeError::NotFound("el carril no tiene terminal abierta".into()))
    }

    /// Lo que teclea el usuario (o pega) en el terminal.
    pub fn write(&self, lane_id: &str, data: &str) -> NudeResult<()> {
        let session = self.session(lane_id)?;
        lock(&session.activity).on_input(data, Instant::now());
        let mut writer = lock(&session.writer);
        let writer = writer
            .as_mut()
            .ok_or_else(|| NudeError::Pty("el proceso del terminal ya terminó".into()))?;
        writer.write_all(data.as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    pub fn resize(&self, lane_id: &str, cols: u16, rows: u16) -> NudeResult<()> {
        let session = self.session(lane_id)?;
        let master = lock(&session.master);
        match master.as_ref() {
            Some(master) => master.resize(size(cols, rows)).map_err(pty_error),
            None => Ok(()), // terminada: no hay nada que redimensionar
        }
    }

    /// Cierra la sesión del carril (si la hay) y la olvida. El proceso recibe
    /// `SIGHUP` y, si sigue vivo pasado `KILL_GRACE`, `SIGKILL`.
    pub fn kill(&self, lane_id: &str) -> bool {
        let removed = lock(&self.inner.sessions).remove(lane_id);
        match removed {
            Some(session) => {
                terminate(&session);
                true
            }
            None => false,
        }
    }

    /// Cierra todas las sesiones (al salir de la app).
    pub fn kill_all(&self) {
        let sessions: Vec<_> = lock(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        for session in sessions {
            terminate(&session);
        }
    }
}

fn emit_activity(inner: &Inner, lane_id: &str, session: &Session, activity: LaneActivity) {
    let event = PtyActivityChange { lane_id: lane_id.to_owned(), session_id: session.id, program: session.program, activity };
    (inner.sink)(PtyEvent::Status(&event));
}

/// `SIGHUP` al shell (que lo reenvía a sus trabajos) y cierre del PTY; si no
/// muere en `KILL_GRACE`, `SIGKILL` en segundo plano.
fn terminate(session: &Arc<Session>) {
    if !session.status().running {
        return;
    }
    let _ = lock(&session.killer).kill();
    lock(&session.writer).take();
    lock(&session.master).take();
    let session = session.clone();
    let _ = std::thread::Builder::new().name("nude-pty-kill".into()).spawn(move || {
        let deadline = Instant::now() + KILL_GRACE;
        while Instant::now() < deadline {
            if !session.status().running {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        #[cfg(unix)]
        if let Some(pid) = session.pid.and_then(|pid| i32::try_from(pid).ok()) {
            // SAFETY: `kill(2)` no toca memoria. El pid es nuestro hijo y sigue
            // marcado como vivo, así que no lo ha recogido el hilo `wait`.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    });
}

/// Shell de login del usuario (`$SHELL`), o `/bin/zsh` (el de macOS).
pub(crate) fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_file())
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

/// Variables de un terminal moderno. Las apps abiertas desde Finder no
/// suelen traer `LANG`, y sin él las CLI pintan mal los caracteres UTF-8.
fn apply_terminal_env(cmd: &mut CommandBuilder) {
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Nude");
    if std::env::var_os("LANG").is_none() {
        cmd.env("LANG", "en_US.UTF-8");
    }
}

/// `'texto'` seguro para sh/zsh/bash/fish.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Script que se pasa a `-c`: lanza el agente si existe o explica cómo
/// instalarlo, y en ambos casos deja al usuario en su shell (`exec`).
fn agent_script(program: LaneProgram, shell: &Path) -> String {
    let Some(binary) = program.binary() else {
        return format!("exec {} -l -i", quote(&shell.to_string_lossy()));
    };
    let title = format!("{} no está instalado (no se encontró `{binary}` en tu PATH).", program.label());
    let lines: Vec<String> = program.install_hint().iter().map(|line| quote(line)).collect();
    let message = format!(
        r"printf '\n\033[1m%s\033[0m\n' {}; printf '%s\n' {}; printf '\n'",
        quote(&title),
        lines.join(" ")
    );
    let exec = format!("exec {} -l -i", quote(&shell.to_string_lossy()));
    let fish = shell.file_name().is_some_and(|name| name == "fish");
    if fish {
        format!("if command -v {binary} >/dev/null 2>&1; {binary}; else; {message}; end; {exec}")
    } else {
        format!("if command -v {binary} >/dev/null 2>&1; then {binary}; else {message}; fi; {exec}")
    }
}

#[cfg(test)]
mod tests;
