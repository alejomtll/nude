import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  BookmarkPlus, ClipboardPaste, Ellipsis, FolderOpen, PanelLeftClose, PanelLeftOpen, Plus, RotateCcw, Trash2, Undo2,
} from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { DiffInspector } from "./DiffInspector";
import { type GraphAction, GitGraphView } from "./GitGraphView";
import { Wordmark } from "./Wordmark";
import {
  agentLabels, cleanPrompt, errorMessage, fileCount, suggestedBranchName, useCheckpoints, useUndoPlan,
  type Checkpoint, type Project, type Resource, type UndoAgentPlan,
} from "./workspace";
import "./App.css";

const relativeDate = new Intl.RelativeTimeFormat("es", { numeric: "auto" });
const fullDate = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "medium" });
const shortTime = new Intl.DateTimeFormat("es", { dateStyle: "short", timeStyle: "short" });

function timeAgo(date: string, now: number) {
  const minutes = Math.max(0, Math.floor((now - Date.parse(date)) / 60_000));
  if (minutes < 1) return "ahora";
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
  manual: { title: "Checkpoint manual", submit: "Guardar checkpoint" },
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

function TimelineView({ project, revision, focus, onRefresh, onRollback, onManual }: {
  project: Project;
  revision: number;
  focus?: string;
  onRefresh: () => void;
  onRollback: (checkpoint: Checkpoint) => void;
  onManual: () => void;
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
        <header className="pane-header">
          <h1 id="timeline-heading" className="pane-title" title={project.rootPath}>
            {project.name}
            {live === "on" && <span className="live-label" role="status" title="Recibiendo checkpoints en vivo"><span />en vivo</span>}
          </h1>
          <div className="pane-actions">
            {live === "off" && <button className="button button-quiet" onClick={onRefresh}>Reconectar</button>}
            <button className="button button-compact" onClick={onManual} title="Guardar el estado actual como checkpoint">
              <BookmarkPlus size={13} aria-hidden="true" />Checkpoint
            </button>
          </div>
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
                {checkpoints.map((checkpoint) => {
                  const prompt = cleanPrompt(checkpoint.promptSummary);
                  const isSelected = checkpoint.id === selected?.id;
                  const date = fullDate.format(Date.parse(checkpoint.createdAt));
                  return (
                    <li key={checkpoint.id} id={`checkpoint-${checkpoint.id}`} className="checkpoint-row"
                      data-agent={checkpoint.agentTool} data-selected={isSelected}>
                      <button className="checkpoint-select" aria-pressed={isSelected}
                        onClick={() => setSelection(checkpoint.id)}>
                        <span className="checkpoint-meta">
                          <span className="agent-dot" aria-hidden="true" />
                          <span className="agent-name">{agentLabels[checkpoint.agentTool]}</span>
                          {checkpoint.modelName && (
                            <span className="model-name" title={checkpoint.modelName}>{checkpoint.modelName}</span>
                          )}
                          <span className="checkpoint-files">{fileCount(checkpoint.filesChangedCount)}</span>
                          <time dateTime={checkpoint.createdAt} title={date}>{timeAgo(checkpoint.createdAt, now)}</time>
                        </span>
                        <span className="checkpoint-summary" dir="auto" title={prompt || undefined}>
                          {prompt || <span className="text-faint">Sin resumen</span>}
                        </span>
                      </button>
                      <button className="row-action-button" onClick={() => onRollback(checkpoint)}
                        title="Restaurar los archivos a este checkpoint"
                        aria-label={`Rollback al checkpoint del ${date}`}>
                        <RotateCcw size={12} aria-hidden="true" />Rollback
                      </button>
                    </li>
                  );
                })}
              </ol>
            ) : (
              <div className="empty-state watermark">
                <Wordmark height={44} title={null} className="wordmark-faint" />
                <p>Sin checkpoints todavía. Los cambios del repositorio aparecerán aquí.</p>
              </div>
            )
          ) : !resource.error && <div className="empty-state" role="status">Cargando checkpoints…</div>}
        </div>
      </main>
      <DiffInspector checkpointId={selected?.id} />
    </>
  );
}

function TopBar({ view, setView, project, revision, onUndo }: {
  project: Project;
  view: View;
  setView: (view: View) => void;
  revision: number;
  onUndo: (plan: UndoAgentPlan) => void;
}) {
  return (
    <header className="topbar">
      <div className="view-switch" role="tablist" aria-label="Vista">
        <button role="tab" aria-selected={view === "timeline"} onClick={() => setView("timeline")}>
          Timeline de agentes
        </button>
        <button role="tab" aria-selected={view === "graph"} onClick={() => setView("graph")}>
          Grafo Git
        </button>
      </div>
      <UndoButton key={project.id} project={project} revision={revision} onUndo={onUndo} />
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
    <button className="button button-compact" disabled={!plan || plan.alreadyUndone} title={title}
      onClick={() => plan && onUndo(plan)}>
      <Undo2 size={13} aria-hidden="true" />Deshacer último agente
      {plan && !plan.alreadyUndone && (
        <span className="undo-agent" data-agent={plan.agentTool}>
          <span className="agent-dot" aria-hidden="true" />{agentLabels[plan.agentTool]}
        </span>
      )}
    </button>
  );
}

function ProjectRow({ project, active, onSelect, onRemove }: {
  project: Project;
  active: boolean;
  onSelect: () => void;
  onRemove: () => void;
}) {
  const [menu, setMenu] = useState(false);
  const container = useRef<HTMLLIElement>(null);

  useEffect(() => {
    if (!menu) return;
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event instanceof MouseEvent && container.current?.contains(event.target as Node)) return;
      setMenu(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [menu]);

  return (
    <li ref={container} className="project-item" data-active={active} data-menu={menu}>
      <button className="project-row" aria-pressed={active} title={project.rootPath} onClick={onSelect}>
        <span className="project-name" dir="auto">{project.name}</span>
        <span className="project-path" dir="ltr">{project.rootPath.replace(/^\/Users\/[^/]+/, "~")}</span>
      </button>
      <button className="project-more" aria-label={`Opciones de ${project.name}`} aria-haspopup="menu"
        aria-expanded={menu} onClick={() => setMenu((open) => !open)}>
        <Ellipsis size={14} aria-hidden="true" />
      </button>
      {menu && (
        <div className="menu" role="menu">
          <button role="menuitem" autoFocus onClick={() => { setMenu(false); onRemove(); }}>
            <Trash2 size={13} aria-hidden="true" />Quitar proyecto
          </button>
        </div>
      )}
    </li>
  );
}

const SIDEBAR_KEY = "nude.sidebarCollapsed";

function readCollapsed() {
  try {
    return window.localStorage.getItem(SIDEBAR_KEY) === "1";
  } catch {
    return false;
  }
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
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const items = projects.data ?? [];
  const project = items.find((item) => item.id === projectId) ?? items[0];
  const refresh = () => setRevision((current) => current + 1);
  const announce = (text: string) => { setNotice(text); refresh(); };

  useEffect(() => {
    try {
      window.localStorage.setItem(SIDEBAR_KEY, collapsed ? "1" : "0");
    } catch {
      // Preferencia solo visual: si no se puede guardar, no pasa nada.
    }
  }, [collapsed]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "b") {
        event.preventDefault();
        setCollapsed((value) => !value);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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

  /** "Abrir carpeta": abre Finder directamente; si falla, el diálogo con ruta. */
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
    <div className="workspace" data-sidebar={collapsed ? "collapsed" : "open"}>
      <aside className="sidebar pane" aria-label="Proyectos">
        <header className="sidebar-header">
          {!collapsed && <Wordmark height={11} className="sidebar-wordmark" />}
          <button className="icon-button" onClick={() => setCollapsed((value) => !value)}
            aria-label={collapsed ? "Mostrar barra lateral" : "Ocultar barra lateral"}
            title={`${collapsed ? "Mostrar" : "Ocultar"} barra lateral (⌘B)`} aria-expanded={!collapsed}>
            {collapsed ? <PanelLeftOpen size={15} aria-hidden="true" /> : <PanelLeftClose size={15} aria-hidden="true" />}
          </button>
        </header>
        {!collapsed && (
          <>
            <div className="sidebar-section">
              <h2 id="projects-heading">Proyectos</h2>
              <span className="flex">
                <button className="icon-button" aria-label="Pegar ruta de un proyecto" title="Pegar ruta"
                  disabled={!projects.data} onClick={() => setAction({ kind: "add" })}>
                  <ClipboardPaste size={13} aria-hidden="true" />
                </button>
                <button className="icon-button" aria-label="Abrir carpeta en Finder" title="Abrir carpeta (Finder)"
                  disabled={!projects.data} onClick={() => void addFromFinder()}>
                  <Plus size={15} aria-hidden="true" />
                </button>
              </span>
            </div>
            <div className="project-list">
              {projects.error ? (
                <div className="px-3 pt-1" role="alert">
                  <p className="error-text">{projects.error}</p>
                  <button className="button mt-2" onClick={() => { setProjects({}); setAttempt(attempt + 1); }}>Reintentar</button>
                </div>
              ) : projects.data ? (
                items.length ? (
                  <ul aria-labelledby="projects-heading">
                    {items.map((item) => (
                      <ProjectRow key={item.id} project={item} active={project?.id === item.id}
                        onSelect={() => { setProjectId(item.id); setFocus(undefined); }}
                        onRemove={() => setAction({ kind: "remove", project: item })} />
                    ))}
                  </ul>
                ) : <p className="px-3 text-faint">Sin proyectos.</p>
              ) : <p className="px-3 text-faint" role="status">Cargando…</p>}
            </div>
          </>
        )}
      </aside>

      {project ? (
        <>
          <TopBar project={project} view={view} setView={setView} revision={revision}
            onUndo={(plan) => setAction({ kind: "undo", project, plan })} />
          {view === "graph" ? (
            <GitGraphView key={project.id} project={project} revision={revision} onAction={onGraphAction}
              onNotice={setNotice}
              onOpenCheckpoint={(id) => { setFocus(id); setView("timeline"); }} />
          ) : (
            <TimelineView key={project.id} project={project} revision={revision} focus={focus} onRefresh={refresh}
              onRollback={(checkpoint) => setAction({ kind: "rollback", project, checkpoint })}
              onManual={() => setAction({ kind: "manual", project })} />
          )}
        </>
      ) : (
        <main className="welcome" aria-labelledby="welcome-title">
          <h1 id="welcome-title" className="sr-only">Nude</h1>
          <Wordmark height={64} title={null} className="wordmark-faint" />
          {projects.data && (
            <button className="button button-compact welcome-button" onClick={() => void addFromFinder()}>
              <FolderOpen size={14} aria-hidden="true" />Abrir carpeta
            </button>
          )}
        </main>
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
