import { invoke } from "@tauri-apps/api/core";
import {
  CircleDot, Files, FolderGit2, FolderPlus, History, Plus, RotateCcw, Trash2,
} from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { DiffInspector } from "./DiffInspector";
import {
  errorMessage, fileCount, useCheckpoints,
  type AgentTool, type Checkpoint, type Project, type Resource,
} from "./workspace";
import "./App.css";

const agents: Record<AgentTool, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  manual: "Manual",
};

const relativeDate = new Intl.RelativeTimeFormat("es", { numeric: "auto" });
const fullDate = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "medium" });

function timeAgo(date: string, now: number) {
  const minutes = Math.max(0, Math.floor((now - Date.parse(date)) / 60_000));
  if (minutes < 1) return "Ahora";
  if (minutes < 60) return relativeDate.format(-minutes, "minute");
  if (minutes < 1440) return relativeDate.format(-Math.floor(minutes / 60), "hour");
  return relativeDate.format(-Math.floor(minutes / 1440), "day");
}

type Action =
  | { kind: "add" }
  | { kind: "manual"; project: Project }
  | { kind: "remove"; project: Project }
  | { kind: "rollback"; project: Project; checkpoint: Checkpoint };

const actions = {
  add: { title: "Agregar proyecto", submit: "Agregar proyecto" },
  manual: { title: "Checkpoint Manual", submit: "Guardar checkpoint" },
  remove: { title: "¿Quitar proyecto?", submit: "Quitar proyecto" },
  rollback: { title: "¿Aplicar rollback?", submit: "Aplicar rollback" },
};

function ActionDialog({ action, close, submit }: {
  action: Action;
  close: () => void;
  submit: (value: string) => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const submitting = useRef(false);
  const [value, setValue] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (submitting.current || (action.kind === "add" && !value.trim())) return;
    submitting.current = true;
    setPending(true);
    setError("");
    try {
      await submit(value);
      close();
    } catch (reason) {
      setError(errorMessage(reason));
      setPending(false);
      submitting.current = false;
    }
  }

  const destructive = action.kind === "remove" || action.kind === "rollback";
  return (
    <dialog
      ref={dialog}
      className="action-dialog"
      aria-labelledby="action-title"
      onCancel={(event) => { event.preventDefault(); if (!submitting.current) close(); }}
    >
      <form onSubmit={(event) => void onSubmit(event)}>
        <h2 id="action-title">{actions[action.kind].title}</h2>
        {action.kind === "add" && (
          <label className="dialog-field">
            Ruta absoluta del repositorio Git
            <input autoFocus required value={value} onChange={(event) => setValue(event.target.value)}
              placeholder="/Users/tu-nombre/Proyectos/repo" spellCheck={false} disabled={pending} />
          </label>
        )}
        {action.kind === "manual" && (
          <label className="dialog-field">
            Resumen del checkpoint
            <textarea autoFocus rows={3} value={value} onChange={(event) => setValue(event.target.value)}
              placeholder="Describe el estado que quieres guardar" disabled={pending} />
          </label>
        )}
        {action.kind === "remove" && (
          <p>Se detendrá la vigilancia de <strong>{action.project.name}</strong> y se borrará su historial de Nude. Los archivos del repositorio se conservarán.</p>
        )}
        {action.kind === "rollback" && (
          <p>Se restaurarán los archivos de <strong>{action.project.name}</strong> al checkpoint del <strong>{fullDate.format(Date.parse(action.checkpoint.createdAt))}</strong>. Nude guardará el estado actual si hay cambios sin checkpoint.</p>
        )}
        {error && <p className="error-text mt-3" role="alert">{error}</p>}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" className="button" autoFocus={destructive} onClick={close} disabled={pending}>Cancelar</button>
          <button type="submit" className={`button ${destructive ? "button-danger" : "button-primary"}`}
            disabled={pending || (action.kind === "add" && !value.trim())}>
            {pending ? "Procesando…" : actions[action.kind].submit}
          </button>
        </div>
      </form>
    </dialog>
  );
}

function ProjectWorkspace({ project, revision, onRefresh, onRollback }: {
  project: Project;
  revision: number;
  onRefresh: () => void;
  onRollback: (checkpoint: Checkpoint) => void;
}) {
  const { resource, live } = useCheckpoints(project.id, revision);
  const [selection, setSelection] = useState<string>();
  const [now, setNow] = useState(Date.now);

  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const checkpoints = resource.data ?? [];
  const selected = checkpoints.find((item) => item.id === selection) ?? checkpoints[0];
  return (
    <>
      <main className="timeline pane" aria-labelledby="timeline-heading">
        <header className="pane-header min-w-0 justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2.5">
            <History size={17} className="shrink-0 text-muted" aria-hidden="true" />
            <h1 id="timeline-heading" className="truncate" title={project.name}>Timeline <span className="text-muted">/ {project.name}</span></h1>
          </div>
          {live === "on" && <span className="live-label" role="status"><span />En vivo</span>}
          {live === "off" && <button className="button button-quiet" onClick={onRefresh}>Reconectar</button>}
        </header>

        <div className="timeline-scroll">
          {resource.error && (
            <div className="inline-error" role="alert">
              <span>{resource.error}</span>
              <button className="button" onClick={onRefresh}>Reintentar</button>
            </div>
          )}
          {resource.data ? (
            checkpoints.length ? (
              <ol className="checkpoint-list" aria-label="Checkpoints, del más reciente al más antiguo">
                {checkpoints.map((checkpoint) => (
                  <li key={checkpoint.id} className="checkpoint-entry" data-agent={checkpoint.agentTool}>
                    <div className="checkpoint-card" data-selected={checkpoint.id === selected?.id}>
                      <button className="checkpoint-select" aria-pressed={checkpoint.id === selected?.id}
                        onClick={() => setSelection(checkpoint.id)}>
                        <span className="checkpoint-top">
                          <span className="agent-badge">{agents[checkpoint.agentTool]}</span>
                          <span className="model-name" title={checkpoint.modelName ?? undefined}>
                            {checkpoint.modelName ?? (checkpoint.agentTool === "manual" ? "" : "Modelo desconocido")}
                          </span>
                          <time dateTime={checkpoint.createdAt} title={fullDate.format(Date.parse(checkpoint.createdAt))}>
                            {timeAgo(checkpoint.createdAt, now)}
                          </time>
                        </span>
                        <span className="checkpoint-summary" dir="auto">{checkpoint.promptSummary || "Sin resumen"}</span>
                      </button>
                      <div className="checkpoint-footer">
                        <span className="flex items-center gap-1.5"><Files size={13} aria-hidden="true" />{fileCount(checkpoint.filesChangedCount)}</span>
                        <button className="button button-quiet" onClick={() => onRollback(checkpoint)}
                          aria-label={`Rollback al checkpoint del ${fullDate.format(Date.parse(checkpoint.createdAt))}`}>
                          <RotateCcw size={13} aria-hidden="true" />Rollback
                        </button>
                      </div>
                    </div>
                  </li>
                ))}
              </ol>
            ) : (
              <div className="empty-state">
                <History size={28} aria-hidden="true" />
                <div><h2>Sin checkpoints</h2><p>Los cambios del repositorio aparecerán aquí.</p></div>
              </div>
            )
          ) : !resource.error && <div className="empty-state" role="status">Cargando checkpoints…</div>}
        </div>
      </main>
      <DiffInspector checkpointId={selected?.id} />
    </>
  );
}

function App() {
  const [projects, setProjects] = useState<Resource<Project[]>>({});
  const [projectId, setProjectId] = useState<string>();
  const [action, setAction] = useState<Action>();
  const [attempt, setAttempt] = useState(0);
  const [revision, setRevision] = useState(0);
  const [notice, setNotice] = useState("");
  const items = projects.data ?? [];
  const project = items.find((item) => item.id === projectId) ?? items[0];
  const refresh = () => setRevision((current) => current + 1);

  useEffect(() => {
    let active = true;
    invoke<Project[]>("list_projects").then(
      (data) => { if (active) setProjects({ data }); },
      (error: unknown) => { if (active) setProjects({ error: errorMessage(error) }); },
    );
    return () => { active = false; };
  }, [attempt]);

  async function perform(action: Action, value: string) {
    setNotice("");
    switch (action.kind) {
      case "add": {
        const { project: added, created } = await invoke<{ project: Project; created: boolean }>("add_project", { path: value.trim() });
        setProjects(({ data = [] }) => ({
          data: [...data.filter((item) => item.id !== added.id), added]
            .sort((a, b) => a.name.localeCompare(b.name, "es", { sensitivity: "base" })),
        }));
        setProjectId(added.id);
        setNotice(created ? "Proyecto agregado" : "Proyecto seleccionado");
        break;
      }
      case "remove": {
        await invoke("remove_project", { projectId: action.project.id });
        setProjects(({ data = [] }) => ({ data: data.filter((item) => item.id !== action.project.id) }));
        setProjectId(undefined);
        setNotice("Proyecto quitado");
        break;
      }
      case "manual": {
        await invoke<Checkpoint>("create_manual_checkpoint", { projectId: action.project.id, summary: value });
        refresh();
        setNotice("Checkpoint guardado");
        break;
      }
      case "rollback": {
        await invoke("rollback_checkpoint", { checkpointId: action.checkpoint.id });
        refresh();
        setNotice("Rollback aplicado");
        break;
      }
    }
  }

  return (
    <div className="workspace">
      <aside className="sidebar pane" aria-labelledby="projects-heading">
        <header className="pane-header justify-between gap-2">
          <h2 id="projects-heading">Proyectos</h2>
          <button className="icon-button" aria-label="Agregar proyecto" title="Agregar proyecto"
            disabled={!projects.data} onClick={() => setAction({ kind: "add" })}>
            <Plus size={17} aria-hidden="true" />
          </button>
        </header>
        <div className="project-list">
          {projects.error ? (
            <div className="px-2 pt-2 text-sm" role="alert">
              <p className="error-text">{projects.error}</p>
              <button className="button mt-2" onClick={() => { setProjects({}); setAttempt(attempt + 1); }}>Reintentar</button>
            </div>
          ) : projects.data ? (
            items.length ? (
              <ul>
                {items.map((item) => (
                  <li key={item.id}>
                    <button className="project-row" aria-pressed={project?.id === item.id}
                      title={item.rootPath} onClick={() => setProjectId(item.id)}>
                      <FolderGit2 size={17} aria-hidden="true" className="shrink-0" />
                      <span className="min-w-0">
                        <span className="block truncate font-medium" dir="auto">{item.name}</span>
                        <span className="project-path" dir="ltr">{item.rootPath}</span>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : <p className="px-2 pt-2 text-sm text-muted">Sin proyectos registrados.</p>
          ) : <p className="px-2 pt-2 text-sm text-muted" role="status">Cargando proyectos…</p>}
        </div>
        <div className="sidebar-bottom">
          <button className="button manual-button" disabled={!project}
            onClick={() => project && setAction({ kind: "manual", project })}>
            <CircleDot size={15} aria-hidden="true" />Checkpoint Manual
          </button>
          <button className="button button-quiet remove-button" disabled={!project}
            onClick={() => project && setAction({ kind: "remove", project })}>
            <Trash2 size={14} aria-hidden="true" />Quitar proyecto
          </button>
        </div>
      </aside>

      {project ? (
        <ProjectWorkspace key={project.id} project={project} revision={revision} onRefresh={refresh}
          onRollback={(checkpoint) => setAction({ kind: "rollback", project, checkpoint })} />
      ) : (
        <>
          <main className="pane timeline" aria-labelledby="timeline-heading">
            <header className="pane-header gap-2.5">
              <History size={17} className="text-muted" aria-hidden="true" />
              <h1 id="timeline-heading">Timeline</h1>
            </header>
            {projects.data && (
              <div className="empty-state">
                <FolderGit2 size={32} aria-hidden="true" />
                <div><h2>Agrega un repositorio Git</h2><p>Nude registrará sus cambios como checkpoints.</p></div>
                <button className="button button-primary" onClick={() => setAction({ kind: "add" })}>
                  <FolderPlus size={16} aria-hidden="true" />Agregar proyecto
                </button>
              </div>
            )}
          </main>
          <DiffInspector />
        </>
      )}
      <div className="notice" role="status">{notice && <span key={revision + notice}>{notice}</span>}</div>
      {action && (
        <ActionDialog action={action} close={() => setAction(undefined)}
          submit={(value) => perform(action, value)} />
      )}
    </div>
  );
}

export default App;
