import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  CircleDot, ClipboardPaste, Files, FolderGit2, FolderOpen, GitGraph, History, Plus, RotateCcw, Trash2, Undo2,
} from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { DiffInspector } from "./DiffInspector";
import { type GraphAction, GitGraphView } from "./GitGraphView";
import {
  agentLabels, errorMessage, fileCount, suggestedBranchName, useCheckpoints, useUndoPlan,
  type Checkpoint, type Project, type Resource, type UndoAgentPlan,
} from "./workspace";
import "./App.css";

const relativeDate = new Intl.RelativeTimeFormat("es", { numeric: "auto" });
const fullDate = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "medium" });
const shortTime = new Intl.DateTimeFormat("es", { dateStyle: "short", timeStyle: "short" });

function timeAgo(date: string, now: number) {
  const minutes = Math.max(0, Math.floor((now - Date.parse(date)) / 60_000));
  if (minutes < 1) return "Ahora";
  if (minutes < 60) return relativeDate.format(-minutes, "minute");
  if (minutes < 1440) return relativeDate.format(-Math.floor(minutes / 60), "hour");
  return relativeDate.format(-Math.floor(minutes / 1440), "day");
}

type View = "timeline" | "graph";

type Action =
  | { kind: "add" }
  | { kind: "manual"; project: Project }
  | { kind: "remove"; project: Project }
  | { kind: "rollback"; project: Project; checkpoint: Checkpoint }
  | { kind: "undo"; project: Project; plan: UndoAgentPlan }
  | { kind: "branch"; project: Project; fromCommit: string | null; label: string }
  | { kind: "checkout"; project: Project; branch: string };

const actions: Record<Action["kind"], { title: string; submit: string }> = {
  add: { title: "Agregar proyecto", submit: "Agregar proyecto" },
  manual: { title: "Checkpoint Manual", submit: "Guardar checkpoint" },
  remove: { title: "¿Quitar proyecto?", submit: "Quitar proyecto" },
  rollback: { title: "¿Aplicar rollback?", submit: "Aplicar rollback" },
  undo: { title: "¿Deshacer el último agente?", submit: "Deshacer sesión" },
  branch: { title: "Nueva rama de prueba", submit: "Crear rama" },
  checkout: { title: "¿Cambiar de rama?", submit: "Cambiar de rama" },
};

async function pickFolder(): Promise<string | null> {
  const selected = await openDialog({
    directory: true,
    multiple: false,
    title: "Elige la carpeta de un repositorio Git",
  });
  return typeof selected === "string" ? selected : null;
}

function undoDescription(plan: UndoAgentPlan) {
  const agent = agentLabels[plan.agentTool];
  const model = plan.modelName ? ` (${plan.modelName})` : "";
  const target = plan.targetCheckpoint
    ? `al checkpoint del ${fullDate.format(Date.parse(plan.targetCheckpoint.createdAt))}`
    : `al commit ${plan.baseCommit?.slice(0, 7) ?? "base"} sobre el que empezó`;
  return { agent: `${agent}${model}`, target };
}

function ActionDialog({ action, close, submit }: {
  action: Action;
  close: () => void;
  submit: (value: string, flag: boolean) => Promise<void>;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const submitting = useRef(false);
  const [value, setValue] = useState(() => (action.kind === "branch" ? suggestedBranchName() : ""));
  const [flag, setFlag] = useState(true);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  const needsValue = action.kind === "add" || action.kind === "branch";
  const blocked = action.kind === "undo" && action.plan.alreadyUndone;

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (submitting.current || blocked || (needsValue && !value.trim())) return;
    submitting.current = true;
    setPending(true);
    setError("");
    try {
      await submit(value, flag);
      close();
    } catch (reason) {
      setError(errorMessage(reason));
      setPending(false);
      submitting.current = false;
    }
  }

  async function browse() {
    try {
      const folder = await pickFolder();
      if (folder) setValue(folder);
    } catch (reason) {
      setError(errorMessage(reason));
    }
  }

  const destructive = action.kind === "remove" || action.kind === "rollback" || action.kind === "undo";
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
          <>
            <button type="button" className="button button-primary w-full" onClick={() => void browse()} disabled={pending}>
              <FolderOpen size={15} aria-hidden="true" />Elegir carpeta en Finder…
            </button>
            <label className="dialog-field mt-4">
              <span className="flex items-center gap-1.5"><ClipboardPaste size={13} aria-hidden="true" />O pega la ruta absoluta</span>
              <input required value={value} onChange={(event) => setValue(event.target.value)}
                placeholder="/Users/tu-nombre/Proyectos/repo" spellCheck={false} disabled={pending} />
            </label>
          </>
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
        {action.kind === "undo" && (() => {
          const { agent, target } = undoDescription(action.plan);
          const discarded = action.plan.sessionCheckpoints + action.plan.laterCheckpoints;
          return action.plan.alreadyUndone ? (
            <p>Los archivos ya están como antes de la sesión de <strong>{agent}</strong>. No hay nada que deshacer.</p>
          ) : (
            <>
              <p>Se descartarán los cambios de la sesión de <strong>{agent}</strong> que empezó el {shortTime.format(Date.parse(action.plan.startedAt))} ({discarded} {discarded === 1 ? "checkpoint" : "checkpoints"}) y los archivos volverán <strong>{target}</strong>.</p>
              <p className="mt-2">Antes se guarda un checkpoint de seguridad, así que puedes volver atrás desde el timeline. Tus commits y ramas no cambian.</p>
            </>
          );
        })()}
        {action.kind === "branch" && (
          <>
            <p className="mb-3">Se creará desde <strong>{action.label}</strong>. Úsala para que un agente trabaje sin tocar tu rama; si no te convence, vuelve a la tuya.</p>
            <label className="dialog-field">
              Nombre de la rama
              <input autoFocus required value={value} onChange={(event) => setValue(event.target.value)}
                spellCheck={false} disabled={pending} />
            </label>
            <label className="mt-3 flex items-center gap-2 text-muted">
              <input type="checkbox" checked={flag} onChange={(event) => setFlag(event.target.checked)} disabled={pending} />
              Cambiar a la nueva rama
            </label>
          </>
        )}
        {action.kind === "checkout" && (
          <p>Se cambiará a <strong>{action.branch}</strong>. Los cambios sin commit que no choquen se mantienen; si alguno chocara, no se toca nada. Nude guarda antes un checkpoint.</p>
        )}
        {error && <p className="error-text mt-3" role="alert">{error}</p>}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" className="button" autoFocus={destructive} onClick={close} disabled={pending}>
            {blocked ? "Cerrar" : "Cancelar"}
          </button>
          {!blocked && (
            <button type="submit" className={`button ${destructive ? "button-danger" : "button-primary"}`}
              disabled={pending || (needsValue && !value.trim())}>
              {pending ? "Procesando…" : actions[action.kind].submit}
            </button>
          )}
        </div>
      </form>
    </dialog>
  );
}

function TimelineView({ project, revision, focus, onRefresh, onRollback }: {
  project: Project;
  revision: number;
  focus?: string;
  onRefresh: () => void;
  onRollback: (checkpoint: Checkpoint) => void;
}) {
  const { resource, live } = useCheckpoints(project.id, revision);
  const [selection, setSelection] = useState<string | undefined>(focus);
  const [now, setNow] = useState(Date.now);

  useEffect(() => { if (focus) setSelection(focus); }, [focus]);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (focus) document.getElementById(`checkpoint-${focus}`)?.scrollIntoView({ block: "nearest" });
  }, [focus, resource.data]);

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
                  <li key={checkpoint.id} id={`checkpoint-${checkpoint.id}`} className="checkpoint-entry" data-agent={checkpoint.agentTool}>
                    <div className="checkpoint-card" data-selected={checkpoint.id === selected?.id}>
                      <button className="checkpoint-select" aria-pressed={checkpoint.id === selected?.id}
                        onClick={() => setSelection(checkpoint.id)}>
                        <span className="checkpoint-top">
                          <span className="agent-badge">{agentLabels[checkpoint.agentTool]}</span>
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

function TopBar({ project, view, setView, revision, onUndo }: {
  project?: Project;
  view: View;
  setView: (view: View) => void;
  revision: number;
  onUndo: (plan: UndoAgentPlan) => void;
}) {
  return (
    <header className="topbar">
      <div className="view-switch" role="tablist" aria-label="Vista">
        <button role="tab" aria-selected={view === "timeline"} disabled={!project} onClick={() => setView("timeline")}>
          <History size={14} aria-hidden="true" />Timeline de Agentes
        </button>
        <button role="tab" aria-selected={view === "graph"} disabled={!project} onClick={() => setView("graph")}>
          <GitGraph size={14} aria-hidden="true" />Grafo Git
        </button>
      </div>
      {project && <UndoButton key={project.id} project={project} revision={revision} onUndo={onUndo} />}
    </header>
  );
}

function UndoButton({ project, revision, onUndo }: {
  project: Project;
  revision: number;
  onUndo: (plan: UndoAgentPlan) => void;
}) {
  const { resource } = useUndoPlan(project.id, revision);
  const plan = resource.data;
  const title = plan
    ? plan.alreadyUndone
      ? `La última sesión de ${agentLabels[plan.agentTool]} ya está deshecha`
      : `Vuelve al estado anterior a la última sesión de ${agentLabels[plan.agentTool]}`
    : resource.error ?? "Todavía no hay sesiones de agentes en este proyecto";
  return (
    <button className="button undo-button" disabled={!plan || plan.alreadyUndone} title={title}
      onClick={() => plan && onUndo(plan)}>
      <Undo2 size={14} aria-hidden="true" />Deshacer último agente
      {plan && !plan.alreadyUndone && <span className="undo-agent" data-agent={plan.agentTool}>{agentLabels[plan.agentTool]}</span>}
    </button>
  );
}

function App() {
  const [projects, setProjects] = useState<Resource<Project[]>>({});
  const [projectId, setProjectId] = useState<string>();
  const [action, setAction] = useState<Action>();
  const [attempt, setAttempt] = useState(0);
  const [revision, setRevision] = useState(0);
  const [notice, setNotice] = useState("");
  const [view, setView] = useState<View>("timeline");
  const [focus, setFocus] = useState<string>();
  const items = projects.data ?? [];
  const project = items.find((item) => item.id === projectId) ?? items[0];
  const refresh = () => setRevision((current) => current + 1);
  const announce = (text: string) => { setNotice(text); refresh(); };

  useEffect(() => {
    let active = true;
    invoke<Project[]>("list_projects").then(
      (data) => { if (active) setProjects({ data }); },
      (error: unknown) => { if (active) setProjects({ error: errorMessage(error) }); },
    );
    return () => { active = false; };
  }, [attempt]);

  async function addProject(path: string) {
    const { project: added, created } = await invoke<{ project: Project; created: boolean }>("add_project", { path: path.trim() });
    setProjects(({ data = [] }) => ({
      data: [...data.filter((item) => item.id !== added.id), added]
        .sort((a, b) => a.name.localeCompare(b.name, "es", { sensitivity: "base" })),
    }));
    setProjectId(added.id);
    setNotice(created ? "Proyecto agregado" : "Proyecto seleccionado");
  }

  /** Botón "Agregar proyecto": abre Finder directamente; si falla, el diálogo con ruta. */
  async function addFromFinder() {
    setNotice("");
    try {
      const folder = await pickFolder();
      if (folder) await addProject(folder);
    } catch (reason) {
      setNotice(errorMessage(reason));
      setAction({ kind: "add" });
    }
  }

  function onGraphAction(graphAction: GraphAction) {
    if (!project) return;
    if (graphAction.kind === "checkout") setAction({ kind: "checkout", project, branch: graphAction.branch });
    else setAction({ kind: "branch", project, fromCommit: graphAction.fromCommit, label: graphAction.label });
  }

  async function perform(action: Action, value: string, flag: boolean) {
    setNotice("");
    switch (action.kind) {
      case "add":
        await addProject(value);
        break;
      case "remove":
        await invoke("remove_project", { projectId: action.project.id });
        setProjects(({ data = [] }) => ({ data: data.filter((item) => item.id !== action.project.id) }));
        setProjectId(undefined);
        setNotice("Proyecto quitado");
        break;
      case "manual":
        await invoke<Checkpoint>("create_manual_checkpoint", { projectId: action.project.id, summary: value });
        announce("Checkpoint guardado");
        break;
      case "rollback":
        await invoke("rollback_checkpoint", { checkpointId: action.checkpoint.id });
        announce("Rollback aplicado");
        break;
      case "undo": {
        const result = await invoke<{ rollback: { filesRestored: number } }>("undo_last_agent", { projectId: action.project.id });
        announce(`Sesión de ${agentLabels[action.plan.agentTool]} deshecha · ${fileCount(result.rollback.filesRestored)}`);
        break;
      }
      case "branch": {
        const result = await invoke<{ branch: string }>("git_create_branch", {
          projectId: action.project.id, name: value.trim(), fromCommit: action.fromCommit, checkout: flag,
        });
        announce(flag ? `Ahora estás en ${result.branch}` : `Rama ${result.branch} creada`);
        break;
      }
      case "checkout":
        await invoke("git_checkout_branch", { projectId: action.project.id, name: action.branch });
        announce(`Ahora estás en ${action.branch}`);
        break;
    }
  }

  return (
    <div className="workspace">
      <aside className="sidebar pane" aria-labelledby="projects-heading">
        <header className="pane-header justify-between gap-2">
          <h2 id="projects-heading">Proyectos</h2>
          <span className="flex gap-1">
            <button className="icon-button" aria-label="Pegar ruta de un proyecto" title="Pegar ruta"
              disabled={!projects.data} onClick={() => setAction({ kind: "add" })}>
              <ClipboardPaste size={15} aria-hidden="true" />
            </button>
            <button className="icon-button" aria-label="Agregar proyecto desde Finder" title="Agregar proyecto (Finder)"
              disabled={!projects.data} onClick={() => void addFromFinder()}>
              <Plus size={17} aria-hidden="true" />
            </button>
          </span>
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
                      title={item.rootPath} onClick={() => { setProjectId(item.id); setFocus(undefined); }}>
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

      <TopBar project={project} view={view} setView={setView} revision={revision}
        onUndo={(plan) => project && setAction({ kind: "undo", project, plan })} />

      {project ? (
        view === "graph" ? (
          <GitGraphView key={project.id} project={project} revision={revision} onAction={onGraphAction}
            onNotice={setNotice}
            onOpenCheckpoint={(id) => { setFocus(id); setView("timeline"); }} />
        ) : (
          <TimelineView key={project.id} project={project} revision={revision} focus={focus} onRefresh={refresh}
            onRollback={(checkpoint) => setAction({ kind: "rollback", project, checkpoint })} />
        )
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
                <div className="flex flex-wrap justify-center gap-2">
                  <button className="button button-primary" onClick={() => void addFromFinder()}>
                    <FolderOpen size={16} aria-hidden="true" />Elegir carpeta…
                  </button>
                  <button className="button" onClick={() => setAction({ kind: "add" })}>
                    <ClipboardPaste size={15} aria-hidden="true" />Pegar ruta
                  </button>
                </div>
              </div>
            )}
          </main>
          <DiffInspector />
        </>
      )}
      <div className="notice" role="status">{notice && <span key={revision + notice}>{notice}</span>}</div>
      {action && (
        <ActionDialog action={action} close={() => setAction(undefined)}
          submit={(value, flag) => perform(action, value, flag)} />
      )}
    </div>
  );
}

export default App;
