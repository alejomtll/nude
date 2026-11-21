import { invoke } from "@tauri-apps/api/core";
import { RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  agentLabels, errorMessage, fileCount, useWatchStatus,
  type AgentSighting, type Checkpoint, type Project, type Resource, type WatcherDiagnostics, type WatchStatus,
} from "./workspace";

const dateTime = new Intl.DateTimeFormat("es", { dateStyle: "short", timeStyle: "medium" });
const when = (iso: string | null) => (iso ? dateTime.format(Date.parse(iso)) : "—");
const home = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");

function Sighting({ session, emptyText }: { session: AgentSighting | null; emptyText: string }) {
  if (!session) return <p className="diag-muted">{emptyText}</p>;
  return (
    <div className="diag-session" data-agent={session.agentTool}>
      <div>
        <span className="agent-dot" aria-hidden="true" />
        <strong>{agentLabels[session.agentTool]}</strong>
        {session.modelName && <code>{session.modelName}</code>}
        <span className="diag-time">{when(session.lastActivity)}</span>
      </div>
      {session.cwd && <code className="diag-path" title={session.cwd}>{home(session.cwd)}</code>}
      {!session.attributable && <p className="diag-muted">Inactiva: no se le atribuyen checkpoints nuevos.</p>}
    </div>
  );
}

/**
 * Indicador "en vivo" de la cabecera del timeline. Al hacer clic muestra el
 * diagnóstico del watcher: estado (o error exacto), última sesión de agente,
 * worktrees enlazados y "Forzar escaneo ahora".
 */
export function LiveIndicator({ project, live, onNotice, onWatchPath }: {
  project: Project;
  live: "connecting" | "on" | "off";
  onNotice: (text: string) => void;
  onWatchPath: (path: string) => Promise<void>;
}) {
  const { status, setStatus } = useWatchStatus(project.id);
  const [open, setOpen] = useState(false);
  const [diagnostics, setDiagnostics] = useState<Resource<WatcherDiagnostics>>({});
  const [scanning, setScanning] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const container = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    let active = true;
    invoke<WatcherDiagnostics>("get_watcher_diagnostics", { projectId: project.id }).then(
      (data) => { if (active) { setDiagnostics({ data }); setStatus(data.status); } },
      (error: unknown) => { if (active) setDiagnostics({ error: errorMessage(error) }); },
    );
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event instanceof MouseEvent && container.current?.contains(event.target as Node)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      active = false;
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open, attempt, project.id, setStatus]);

  async function scan() {
    setScanning(true);
    try {
      const result = await invoke<{ checkpoint: Checkpoint | null; status: WatchStatus }>("force_scan", { projectId: project.id });
      setStatus(result.status);
      onNotice(result.checkpoint
        ? `Checkpoint creado · ${fileCount(result.checkpoint.filesChangedCount)}`
        : "Sin cambios desde el último checkpoint");
    } catch (reason) {
      onNotice(errorMessage(reason));
    } finally {
      setScanning(false);
      setAttempt((value) => value + 1);
    }
  }

  const failing = status?.state === "error";
  const label = failing ? "sin vigilancia" : live === "on" ? "en vivo" : live === "off" ? "sin conexión" : "conectando";
  const data = diagnostics.data;
  const current = data?.status ?? status;
  const unwatched = data?.worktrees.filter((wt) => !wt.projectId) ?? [];

  return (
    <div ref={container} className="live-anchor">
      <button className="live-label" data-state={failing ? "error" : live} aria-haspopup="dialog" aria-expanded={open}
        title={failing ? status?.error ?? "Error del watcher" : "Estado del watcher"} onClick={() => setOpen((value) => !value)}>
        <span />{label}
      </button>
      {open && (
        <div className="popover diag-popover" role="dialog" aria-label="Diagnóstico del watcher">
          <section>
            <h3>Watcher</h3>
            {current ? (
              current.state === "error" ? (
                <p className="error-text" role="alert">{current.error ?? "Error desconocido"}</p>
              ) : (
                <p className="diag-state">
                  {current.state === "active" ? "Activo" : current.state === "starting" ? "Arrancando…" : "Detenido"}
                  {current.watchingSince && <span className="diag-muted"> · desde {when(current.watchingSince)}</span>}
                </p>
              )
            ) : <p className="diag-muted">Consultando…</p>}
            {current && (
              <dl className="diag-grid">
                <dt>Último evento</dt><dd>{when(current.lastEventAt)}</dd>
                <dt>Última revisión</dt><dd>{when(current.lastScanAt)}</dd>
                <dt>Último checkpoint</dt><dd>{when(current.lastCheckpointAt)}</dd>
              </dl>
            )}
          </section>

          <section>
            <h3>Última sesión de agente</h3>
            {diagnostics.error && <p className="error-text">{diagnostics.error}</p>}
            {data ? (
              <Sighting session={data.lastSession} emptyText="Ninguna en los últimos 7 días para esta carpeta." />
            ) : !diagnostics.error && <p className="diag-muted">Buscando…</p>}
            {data && (
              <p className="diag-muted">
                Se atribuyen checkpoints a sesiones activas en los últimos {data.attributionWindowMinutes} min; el firewall solo a las de los últimos {data.firewallWindowMinutes} min.
              </p>
            )}
          </section>

          {unwatched.length > 0 && (
            <section>
              <h3>Worktrees sin vigilar</h3>
              <p className="diag-muted">Los agentes que trabajan en un worktree no cambian esta carpeta, así que aquí no se crean checkpoints. Vigílalo como proyecto propio.</p>
              <ul className="diag-worktrees">
                {unwatched.map((worktree) => (
                  <li key={worktree.path}>
                    <div>
                      <code className="diag-path" title={worktree.path}>{home(worktree.path)}</code>
                      {worktree.branch && <span className="diag-muted">{worktree.branch}</span>}
                      {worktree.lastSession && (
                        <span className="diag-muted" data-agent={worktree.lastSession.agentTool}>
                          <span className="agent-dot" aria-hidden="true" /> {agentLabels[worktree.lastSession.agentTool]} · {when(worktree.lastSession.lastActivity)}
                        </span>
                      )}
                    </div>
                    <button className="button button-compact" onClick={() => {
                      onWatchPath(worktree.path).catch((reason: unknown) => onNotice(errorMessage(reason)));
                    }}>Vigilar</button>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <button className="button w-full" onClick={() => void scan()} disabled={scanning}>
            <RefreshCw size={13} aria-hidden="true" className={scanning ? "spin" : undefined} />
            {scanning ? "Escaneando…" : "Forzar escaneo ahora"}
          </button>
        </div>
      )}
    </div>
  );
}
