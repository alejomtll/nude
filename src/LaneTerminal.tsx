import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { Play, RotateCcw, Square, SquareTerminal } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  activityLabels, errorMessage, programLabels,
  type AgentTool, type LaneProgram, type PtyActivityChange, type PtyAttach, type PtyExit, type PtyOutput, type PtyStatus,
} from "./workspace";

/** Paleta neutra (la de la app): fondo #121212, texto #E5E5E5, cursor #A3A3A3. */
const theme = {
  background: "#121212",
  foreground: "#E5E5E5",
  cursor: "#A3A3A3",
  cursorAccent: "#121212",
  selectionBackground: "#3A3A3A",
  black: "#1C1C1C", brightBlack: "#5C5C5C",
  red: "#F85149", brightRed: "#FF7B72",
  green: "#3FB950", brightGreen: "#56D364",
  yellow: "#D29922", brightYellow: "#E3B341",
  blue: "#6C9EF8", brightBlue: "#8DB4FA",
  magenta: "#BC8CFF", brightMagenta: "#D2A8FF",
  cyan: "#39C5CF", brightCyan: "#56D4DD",
  white: "#C9C9C9", brightWhite: "#EDEDED",
};

const programs: LaneProgram[] = ["claude-code", "codex", "opencode"];

/**
 * Terminal real del carril (PTY en Rust + xterm.js). Desmontarla no mata el
 * proceso: al volver se reengancha con el historial y solo pinta la salida
 * posterior (`end` de cada trozo).
 */
export function LaneTerminal({ laneId, preset, visible, onNotice }: {
  laneId: string;
  /** Agente previsto del carril: su botón va primero. */
  preset: AgentTool | null;
  visible: boolean;
  onNotice: (text: string) => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fit = useRef<FitAddon | null>(null);
  /** Sesión pintada y hasta qué byte; `null` mientras se conecta. */
  const session = useRef<{ id: number; end: number } | null>(null);
  const pending = useRef<PtyOutput[]>([]);
  const [status, setStatus] = useState<PtyStatus | null>(null);
  const [phase, setPhase] = useState<"connecting" | "ready">("connecting");
  const running = status?.running ?? false;
  const runningRef = useRef(false);
  runningRef.current = running;
  /** Ajusta xterm al contenedor y avisa al PTY si cambió el tamaño. */
  const syncSize = useRef<(force?: boolean) => void>(() => undefined);

  function adopt(attach: PtyAttach) {
    const terminal = term.current;
    if (!terminal) return;
    terminal.reset();
    terminal.write(attach.scrollback);
    let end = attach.end;
    for (const chunk of pending.current) {
      if (chunk.sessionId === attach.sessionId && chunk.end > end) {
        terminal.write(chunk.data);
        end = chunk.end;
      }
    }
    pending.current = [];
    session.current = { id: attach.sessionId, end };
    runningRef.current = attach.running;
    setStatus(attach);
    setPhase("ready");
    if (attach.running) {
      // La sesión pudo nacer con otro tamaño (p. ej. al crear el carril).
      window.requestAnimationFrame(() => syncSize.current(true));
      terminal.focus();
    }
  }

  async function start(program: LaneProgram) {
    const terminal = term.current;
    if (!terminal) return;
    session.current = null;
    pending.current = [];
    setPhase("connecting");
    try {
      adopt(await invoke<PtyAttach>("spawn_lane_pty", {
        laneId, agentCmd: program, cols: terminal.cols, rows: terminal.rows,
      }));
    } catch (reason) {
      setPhase("ready");
      onNotice(errorMessage(reason));
    }
  }

  async function stop() {
    try {
      await invoke("kill_lane_pty", { laneId });
      setStatus((current) => current && { ...current, running: false, activity: "exited" });
    } catch (reason) {
      onNotice(errorMessage(reason));
    }
  }

  useEffect(() => {
    const element = host.current;
    if (!element) return;
    const terminal = new Terminal({
      theme,
      fontFamily: '"JetBrains Mono", "SF Mono", SFMono-Regular, ui-monospace, Menlo, monospace',
      fontSize: 12,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 5000,
      macOptionIsMeta: true,
      allowTransparency: false,
    });
    const fitter = new FitAddon();
    terminal.loadAddon(fitter);
    terminal.open(element);
    term.current = terminal;
    fit.current = fitter;
    if (element.clientWidth > 0) fitter.fit();

    let active = true;
    const stops: (() => void)[] = [];
    const subscribe = <T,>(event: string, handler: (payload: T) => void) => listen<T>(event, ({ payload }) => handler(payload))
      .then((stop) => { if (active) stops.push(stop); else stop(); });

    const input = terminal.onData((data) => {
      if (!runningRef.current) return;
      invoke("write_lane_pty", { laneId, data }).catch(() => undefined);
    });

    // Tamaño: se ajusta al contenedor y se avisa al PTY (con un poco de calma).
    let timer: number | undefined;
    let sent = "";
    syncSize.current = (force = false) => {
      if (!element.clientWidth || !element.clientHeight) return; // pestaña oculta
      fitter.fit();
      const size = `${terminal.cols}x${terminal.rows}`;
      if (!runningRef.current || (size === sent && !force)) return;
      sent = size;
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        invoke("resize_lane_pty", { laneId, cols: terminal.cols, rows: terminal.rows }).catch(() => undefined);
      }, 80);
    };
    const observer = new ResizeObserver(() => syncSize.current());
    observer.observe(element);

    void Promise.all([
      subscribe<PtyOutput>("lane-pty-output", (chunk) => {
        if (chunk.laneId !== laneId) return;
        const current = session.current;
        if (!current) {
          pending.current.push(chunk);
        } else if (chunk.sessionId === current.id && chunk.end > current.end) {
          terminal.write(chunk.data);
          current.end = chunk.end;
        }
      }),
      subscribe<PtyExit>("lane-pty-exit", (exit) => {
        if (exit.laneId !== laneId || exit.sessionId !== session.current?.id) return;
        setStatus((current) => current && { ...current, running: false, exitCode: exit.exitCode, activity: "exited" });
      }),
      subscribe<PtyActivityChange>("lane-pty-status", (change) => {
        if (change.laneId !== laneId || change.sessionId !== session.current?.id) return;
        setStatus((current) => current && current.sessionId === change.sessionId
          ? { ...current, activity: change.activity } : current);
      }),
    ]).then(async () => {
      if (!active) return;
      // Con los oyentes ya puestos: se engancha a la sesión que haya.
      const attach = await invoke<PtyAttach | null>("attach_lane_pty", { laneId }).catch(() => null);
      if (!active) return;
      if (attach) adopt(attach);
      else setPhase("ready");
    });

    return () => {
      active = false;
      window.clearTimeout(timer);
      observer.disconnect();
      input.dispose();
      stops.forEach((stop) => stop());
      terminal.dispose();
      term.current = null;
      fit.current = null;
      syncSize.current = () => undefined;
      session.current = null;
    };
    // Una terminal por carril; `onNotice` no cambia lo que se pinta.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [laneId]);

  useEffect(() => {
    if (!visible) return;
    const id = window.requestAnimationFrame(() => {
      syncSize.current();
      if (runningRef.current) term.current?.focus();
    });
    return () => window.cancelAnimationFrame(id);
  }, [visible]);

  const first = programs.find((program) => program === preset);
  const order = first ? [first, ...programs.filter((program) => program !== first)] : programs;
  const idle = phase === "ready" && !status;

  return (
    <div className="lane-terminal">
      <div className="lane-terminal-bar">
        {status ? (
          <>
            <span className="lane-terminal-state" data-activity={running ? status.activity : "exited"}>
              <span className="state-dot" aria-hidden="true" />
              {programLabels[status.program]}
              <span className="text-faint">
                {running
                  ? activityLabels[status.activity].toLowerCase()
                  : `terminado${status.exitCode !== null ? ` (código ${status.exitCode})` : ""}`}
              </span>
            </span>
            {running ? (
              <button className="button button-compact" onClick={() => void stop()} title="Cierra el proceso (SIGHUP)">
                <Square size={10} aria-hidden="true" />Detener
              </button>
            ) : (
              <span className="flex gap-1">
                <button className="button button-compact" onClick={() => void start(status.program)}>
                  <RotateCcw size={11} aria-hidden="true" />Reiniciar
                </button>
                {status.program !== "shell" && (
                  <button className="button button-compact" onClick={() => void start("shell")}>
                    <SquareTerminal size={11} aria-hidden="true" />Shell
                  </button>
                )}
              </span>
            )}
          </>
        ) : (
          <span className="text-faint">{phase === "connecting" ? "Conectando…" : "Sin proceso"}</span>
        )}
      </div>
      <div className="lane-terminal-host" ref={host} hidden={idle} />
      {idle && (
        <div className="lane-launcher">
          <p>Ejecuta un agente en este carril sin salir de Nude. Usa tu shell de login, con tu <span className="mono">PATH</span> de Homebrew, nvm y cargo.</p>
          <div className="lane-launcher-actions">
            {order.map((program, index) => (
              <button key={program} className={`button ${index === 0 ? "button-primary" : ""}`} data-agent={program}
                onClick={() => void start(program)}>
                <Play size={12} aria-hidden="true" />Iniciar {programLabels[program]}
              </button>
            ))}
            <button className="button button-quiet" onClick={() => void start("shell")}>
              <SquareTerminal size={13} aria-hidden="true" />Solo shell
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
