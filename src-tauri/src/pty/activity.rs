//! Estado en vivo de la terminal de un carril (estilo cmux).
//!
//! - `working`: el proceso emite salida reciente.
//! - `needs_attention`: tras un bloque de salida lleva `QUIET_AFTER` callado y
//!   las últimas líneas (sin códigos ANSI) parecen una pregunta, una
//!   confirmación o un prompt listo para recibir instrucciones.
//! - `exited`: el proceso terminó.
//!
//! El detector es puro (recibe `Instant`s) para poder probarlo sin PTY. El eco
//! de lo que teclea el usuario no cuenta como trabajo: un carril que espera
//! respuesta sigue esperándola mientras el usuario escribe.

use std::time::{Duration, Instant};

use serde::Serialize;

/// Silencio necesario tras un bloque de salida para evaluar si espera input.
pub const QUIET_AFTER: Duration = Duration::from_secs(3);
/// Salida que llega justo después de una pulsación se considera eco.
const ECHO_WINDOW: Duration = Duration::from_millis(350);
/// Un trozo más grande que esto no es eco aunque llegue tras una pulsación.
const ECHO_MAX_BYTES: usize = 256;
/// Cola de la salida que se conserva para mirar las últimas líneas.
const TAIL_BYTES: usize = 4096;
/// Líneas no vacías (tras limpiar ANSI) que se examinan.
const TAIL_LINES: usize = 12;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LaneActivity {
    Working,
    NeedsAttention,
    Exited,
}

#[derive(Debug)]
pub struct ActivityTracker {
    state: LaneActivity,
    last_output: Option<Instant>,
    last_input: Option<Instant>,
    /// Hubo salida "real" (no eco) desde la última evaluación.
    unseen_output: bool,
    tail: String,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self { state: LaneActivity::Working, last_output: None, last_input: None, unseen_output: false, tail: String::new() }
    }

    pub fn state(&self) -> LaneActivity {
        self.state
    }

    fn set(&mut self, state: LaneActivity) -> Option<LaneActivity> {
        if self.state == state {
            return None;
        }
        self.state = state;
        Some(state)
    }

    /// El usuario tecleó o pegó algo en la terminal. Con Enter se envía la
    /// respuesta: lo que llegue después ya es trabajo, no eco.
    pub fn on_input(&mut self, data: &str, now: Instant) {
        self.last_input = if data.contains(['\r', '\n']) { None } else { Some(now) };
    }

    /// Llega un trozo de salida. Devuelve el estado nuevo si cambió.
    pub fn on_output(&mut self, chunk: &str, now: Instant) -> Option<LaneActivity> {
        if self.state == LaneActivity::Exited {
            return None;
        }
        push_tail(&mut self.tail, chunk);
        self.last_output = Some(now);
        let echo = chunk.len() <= ECHO_MAX_BYTES
            && self.last_input.is_some_and(|input| now.saturating_duration_since(input) <= ECHO_WINDOW);
        if echo {
            return None;
        }
        self.unseen_output = true;
        self.set(LaneActivity::Working)
    }

    /// Comprobación periódica: pasa a `needs_attention` si procede.
    pub fn tick(&mut self, now: Instant) -> Option<LaneActivity> {
        if self.state != LaneActivity::Working || !self.unseen_output {
            return None;
        }
        let Some(last) = self.last_output else { return None };
        if now.saturating_duration_since(last) < QUIET_AFTER {
            return None;
        }
        // Se evalúa una vez por bloque: si no parece una espera, sigue
        // `working` (p. ej. una compilación larga y callada).
        self.unseen_output = false;
        if looks_like_waiting(&self.tail) { self.set(LaneActivity::NeedsAttention) } else { None }
    }

    pub fn on_exit(&mut self) -> Option<LaneActivity> {
        self.set(LaneActivity::Exited)
    }
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn push_tail(tail: &mut String, chunk: &str) {
    tail.push_str(chunk);
    if tail.len() > TAIL_BYTES {
        let mut cut = tail.len() - TAIL_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
}

/// Texto visible de una salida de terminal: quita CSI/OSC/DCS y controles.
/// Los movimientos de cursor que suelen separar zonas de una TUI se vuelven
/// saltos de línea y los avances horizontales, espacios.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    // CSI: parámetros 0x30–0x3F, intermedios 0x20–0x2F, final 0x40–0x7E.
                    let mut last = None;
                    for next in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&next) {
                            last = Some(next);
                            break;
                        }
                    }
                    match last {
                        Some('H' | 'f' | 'A' | 'B' | 'E' | 'F' | 'd') => out.push('\n'),
                        Some('C' | 'G' | 'I') => out.push(' '),
                        _ => {}
                    }
                }
                Some(']' | 'P' | '_' | '^' | 'X') => {
                    // OSC/DCS/…: hasta BEL o ESC \.
                    while let Some(next) = chars.next() {
                        if next == '\u{07}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    chars.next(); // juego de caracteres: un byte más
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' | '\n' => out.push('\n'),
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Marcas de un agente que sigue trabajando (barras de estado de Claude Code y
/// Codex mientras piensan o ejecutan).
const WORKING_MARKERS: &[&str] = &[
    "esc to interrupt",
    "ctrl+c to interrupt",
    "esc para interrumpir",
    "press esc to stop",
];

/// Preguntas, confirmaciones y prompts de entrada de las CLI de agentes.
const WAITING_MARKERS: &[&str] = &[
    "(y/n)", "[y/n]", "(y/n)?", "[y/n]?", "(yes/no)", "[yes/no]", "(s/n)", "[s/n]",
    "do you want", "would you like", "are you sure", "do you trust",
    "proceed?", "continue?", "confirm?", "approve?", "allow?",
    "¿quieres", "¿deseas", "¿continuar", "¿confirmas", "¿permitir",
    "press enter", "enter to confirm", "enter to select", "esc to cancel", "to cancel",
    "❯ 1.", "› 1.", "> 1.", "1. yes",
    "for shortcuts", "⏎ send", "enter to send", "ctrl+j newline", "context left",
    "waiting for input", "awaiting input", "password:", "passphrase",
];

/// Último marcador (de una lista) en `text`: posición en bytes.
fn last_marker(text: &str, markers: &[&str]) -> Option<usize> {
    markers.iter().filter_map(|marker| text.rfind(marker)).max()
}

/// La última línea es un prompt de shell o de entrada (`$`, `%`, `❯`, `>`…).
fn is_prompt_line(line: &str) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    let first = line.chars().next();
    let last = line.chars().last();
    matches!(last, Some('$' | '%' | '#' | '>' | '❯' | '›' | '»' | 'λ' | '➜' | '?'))
        || matches!(first, Some('❯' | '›' | '➜' | '▌'))
        || line.starts_with("> ")
        || line.starts_with("│ >")
        || line.starts_with("│ ❯")
}

/// Heurística sobre la cola de salida: ¿espera respuesta o instrucciones?
pub fn looks_like_waiting(raw_tail: &str) -> bool {
    let text = strip_ansi(raw_tail);
    let lines: Vec<&str> = text.lines().map(str::trim_end).filter(|line| !line.trim().is_empty()).collect();
    let Some(last_line) = lines.last() else { return false };
    let recent = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n").to_lowercase();

    // Las TUI repintan: gana lo que se dibujó más tarde.
    let working = last_marker(&recent, WORKING_MARKERS);
    let waiting = last_marker(&recent, WAITING_MARKERS);
    match (working, waiting) {
        (Some(w), Some(q)) => return q > w,
        (Some(_), None) => return false,
        (None, Some(_)) => return true,
        (None, None) => {}
    }
    is_prompt_line(last_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn strip_ansi_removes_sequences_and_keeps_text() {
        let raw = "\u{1b}[1;32mok\u{1b}[0m \u{1b}]0;título\u{07}listo\u{1b}[2K\r\n\u{1b}[?25h$ ";
        assert_eq!(strip_ansi(raw), "ok listo\n$ ");
        // Movimiento de cursor = nueva zona; avance horizontal = espacio.
        assert_eq!(strip_ansi("a\u{1b}[5;1Hb\u{1b}[3Cc"), "a\nb c");
        assert_eq!(strip_ansi("\u{1b}(Bhola\u{1b}P1$r\u{1b}\\ ñ"), "hola ñ");
    }

    #[test]
    fn waiting_heuristics() {
        assert!(looks_like_waiting("Applying patch...\nDo you want to make this edit to main.rs?\n❯ 1. Yes\n  2. No\n"));
        assert!(looks_like_waiting("Overwrite file? (y/N) "));
        assert!(looks_like_waiting("\u{1b}[32muser@mac\u{1b}[0m nude % "));
        assert!(looks_like_waiting("Done.\n\u{1b}[1;35m❯\u{1b}[0m "));
        assert!(looks_like_waiting("╭────╮\n│ > Try \"fix lint errors\" │\n╰────╯\n  ? for shortcuts"));
        assert!(!looks_like_waiting("Compiling nude v0.1.0\n   Building [=====>    ] 120/300"));
        assert!(!looks_like_waiting("✻ Thinking… (12s · esc to interrupt)"));
        assert!(!looks_like_waiting(""));
    }

    #[test]
    fn later_redraw_wins_over_older_markers() {
        // Un repintado viejo de «trabajando» seguido del cuadro de entrada.
        let tail = "\u{1b}[2K✻ Working… (esc to interrupt)\u{1b}[3A\u{1b}[2K│ > \u{1b}[1B  ? for shortcuts";
        assert!(looks_like_waiting(tail));
        // Y al revés: pregunta respondida y el agente vuelve a trabajar.
        let tail = "Do you want to proceed? (y/n) y\n✻ Running tests… (8s · esc to interrupt)";
        assert!(!looks_like_waiting(tail));
    }

    #[test]
    fn tracker_needs_attention_after_quiet_prompt() {
        let t0 = Instant::now();
        let mut tracker = ActivityTracker::new();
        assert_eq!(tracker.state(), LaneActivity::Working);
        assert_eq!(tracker.on_output("Editing src/main.rs\n", t0), None);
        assert_eq!(tracker.on_output("Do you want to apply this change? (y/n) ", at(t0, 200)), None);
        assert_eq!(tracker.tick(at(t0, 2_000)), None, "aún no han pasado 3 s");
        assert_eq!(tracker.tick(at(t0, 3_300)), Some(LaneActivity::NeedsAttention));
        assert_eq!(tracker.tick(at(t0, 6_000)), None, "solo se avisa una vez");

        // El usuario responde: el eco no cuenta como trabajo…
        tracker.on_input("y", at(t0, 7_000));
        assert_eq!(tracker.on_output("y", at(t0, 7_020)), None);
        assert_eq!(tracker.state(), LaneActivity::NeedsAttention);
        // …pero tras Enter, la salida que sigue sí (aunque sea inmediata).
        tracker.on_input("\r", at(t0, 7_100));
        assert_eq!(tracker.on_output("\r\nApplying edit…\n", at(t0, 7_110)), Some(LaneActivity::Working));
        assert_eq!(tracker.on_exit(), Some(LaneActivity::Exited));
        assert_eq!(tracker.on_output("tarde", at(t0, 8_000)), None);
        assert_eq!(tracker.state(), LaneActivity::Exited);
    }

    #[test]
    fn tracker_stays_working_while_output_flows_or_without_prompt() {
        let t0 = Instant::now();
        let mut tracker = ActivityTracker::new();
        for i in 0..10 {
            tracker.on_output("✻ Thinking… (esc to interrupt)\r", at(t0, i * 1_000));
            assert_eq!(tracker.tick(at(t0, i * 1_000 + 900)), None);
        }
        // Silencio largo sin prompt: sigue «working».
        let mut quiet = ActivityTracker::new();
        quiet.on_output("Compiling 120 crates\n", t0);
        assert_eq!(quiet.tick(at(t0, 10_000)), None);
        assert_eq!(quiet.state(), LaneActivity::Working);
        // Nada de salida todavía: no se evalúa.
        let mut fresh = ActivityTracker::new();
        assert_eq!(fresh.tick(at(t0, 60_000)), None);
    }

    #[test]
    fn big_paste_output_is_not_echo() {
        let t0 = Instant::now();
        let mut tracker = ActivityTracker::new();
        tracker.on_output("$ ", t0);
        assert_eq!(tracker.tick(at(t0, 3_500)), Some(LaneActivity::NeedsAttention));
        tracker.on_input("x", at(t0, 4_000));
        let big = "x".repeat(ECHO_MAX_BYTES + 1);
        assert_eq!(tracker.on_output(&big, at(t0, 4_050)), Some(LaneActivity::Working));
    }
}
