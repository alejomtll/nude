import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  BookmarkPlus, ClipboardPaste, Code, Command, Ellipsis, FileCode, FolderOpen, GitBranch, House, ListTree,
  PanelLeft, PanelLeftClose, PanelLeftOpen, Plus, RotateCcw, Shield, Trash2, Undo2,
} from "lucide-react";
import { memo, type FormEvent, useCallback, useDeferredValue, useEffect, useRef, useState } from "react";
import { useCheckpointDetailsCache, type CheckpointDetails } from "./checkpointDetails";
import { CommandPalette, type PaletteCommand } from "./CommandPalette";
import { DiffInspector, type Editors } from "./DiffInspector";
import { type GraphAction, GitGraphView } from "./GitGraphView";
import { GuardrailToasts, GuardrailsButton } from "./Guardrails";
import { PreviewButton } from "./Preview";
import { TimeScrubber } from "./TimeScrubber";
import { ProjectLauncher } from "./ProjectLauncher";
import { LiveIndicator } from "./WatcherStatus";
import { Wordmark } from "./Wordmark";
import {
  agentLabels, cleanPrompt, errorMessage, fileCount, openInEditor, suggestedBranchName, useCheckpoints,
  useEditors, useGuardrailAlerts, useUndoPlan,
  type Checkpoint, type OpenTarget, type Project, type Resource, type UndoAgentPlan,
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

const CheckpointRow = memo(function CheckpointRow({ checkpoint, now, isSelected, onSelect, onRollback }: {
  checkpoint: Checkpoint;
  now: number;
  isSelected: boolean;
  onSelect: (id: string) => void;
  onRollback: (checkpoint: Checkpoint) => void;
}) {
  const prompt = cleanPrompt(checkpoint.promptSummary);
  const date = fullDate.format(Date.parse(checkpoint.createdAt));
  return (
    <li id={`checkpoint-${checkpoint.id}`} className="checkpoint-row"
      data-agent={checkpoint.agentTool} data-selected={isSelected}>
      <button className="checkpoint-select" aria-pressed={isSelected}
        onClick={() => onSelect(checkpoint.id)}>
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
});

function TimelineView({
  project, revision, focus, cache, onRefresh, onRollback, onManual, editors, onTarget, onNotice, onWatchPath,
}: {
  project: Project;
  revision: number;
  focus?: string;
  cache: CheckpointDetails;
  onRefresh: () => void;
  onRollback: (checkpoint: Checkpoint) => void;
  onManual: () => void;
  editors: Editors;
  onTarget: (target: OpenTarget | null) => void;
  onNotice: (text: string) => void;
  onWatchPath: (path: string) => Promise<void>;
}) {
  const { resource, live } = useCheckpoints(project.id, revision);
  const [selection, setSelection] = useState<string | undefined>(focus);
  const [scrubbed, setScrubbed] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [settled, setSettled] = useState<string>();
  const [now, setNow] = useState(Date.now);

  const selectRow = useCallback((id: string) => {
    setScrubbed(false);
    setSelection(id);
  }, []);
  useEffect(() => { if (focus) selectRow(focus); }, [focus, selectRow]);
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);
  useEffect(() => {
    if (focus) document.getElementById(`checkpoint-${focus}`)?.scrollIntoView({ block: "nearest" });
  }, [focus, resource.data]);

  const checkpoints = resource.data ?? [];
  const selected = checkpoints.find((item) => item.id === selection) ?? checkpoints[0];
  const selectedId = selected?.id;
  const inspectorId = useDeferredValue(selectedId);
  useEffect(() => {
    if (!selectedId || !scrubbed || dragging) return;
    const timer = window.setTimeout(() => {
      setSettled(selectedId);
      document.getElementById(`checkpoint-${selectedId}`)?.scrollIntoView({ block: "nearest" });
    }, 150);
    return () => window.clearTimeout(timer);
  }, [selectedId, scrubbed, dragging]);

  return (
    <>
      <main className="timeline pane" aria-labelledby="timeline-heading">
        <header className="pane-header">
          <h1 id="timeline-heading" className="pane-title" title={project.rootPath}>
            {project.name}
          </h1>
          <LiveIndicator project={project} live={live} onNotice={onNotice} onWatchPath={onWatchPath} />
          <div className="pane-actions">
            {live === "off" && <button className="button button-quiet" onClick={onRefresh}>Reconectar</button>}
            <button className="button button-compact" onClick={onManual} title="Guardar el estado actual como checkpoint">
              <BookmarkPlus size={13} aria-hidden="true" />Checkpoint
            </button>
          </div>
        </header>

        {selected && (
          <TimeScrubber checkpoints={checkpoints} selected={selected} onRestore={onRollback}
            onDraggingChange={setDragging}
            onSelect={(checkpoint) => {
              setScrubbed(true);
              setSettled(undefined);
              setSelection(checkpoint.id);
            }} />
        )}

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
                  <CheckpointRow key={checkpoint.id} checkpoint={checkpoint} now={now}
                    isSelected={checkpoint.id === selectedId} onSelect={selectRow} onRollback={onRollback} />
                ))}
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
      <DiffInspector projectId={project.id} checkpointId={inspectorId} cache={cache}
        ready={inspectorId === selectedId && !dragging && (!scrubbed || settled === selectedId)}
        projectRoot={project.rootPath} editors={editors} onTarget={onTarget} />
    </>
  );
}

function TopBar({ view, setView, project, revision, onUndo, guardOpen, setGuardOpen, onPalette, onPreviewUrl }: {
  project: Project;
  view: View;
  setView: (view: View) => void;
  revision: number;
  onUndo: (plan: UndoAgentPlan) => void;
  guardOpen: boolean;
  setGuardOpen: (open: boolean) => void;
  onPalette: () => void;
  onPreviewUrl: (url: string | null) => void;
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
      <div className="topbar-actions">
        <button className="palette-trigger" onClick={onPalette} title="Paleta de comandos (⌘K)" aria-label="Paleta de comandos">
          <Command size={11} aria-hidden="true" />K
        </button>
        <GuardrailsButton project={project} open={guardOpen} setOpen={setGuardOpen} />
        <PreviewButton project={project} onUrl={onPreviewUrl} />
        <UndoButton project={project} revision={revision} onUndo={onUndo} />
      </div>
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
  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(null);
  const [action, setAction] = useState<Action>();
  const [attempt, setAttempt] = useState(0);
  const [revision, setRevision] = useState(0);
  const [notice, setNotice] = useState("");
  const [view, setView] = useState<View>("timeline");
  const [focus, setFocus] = useState<string>();
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const [palette, setPalette] = useState(false);
  const [guardOpen, setGuardOpen] = useState(false);
  const [target, setTarget] = useState<OpenTarget | null>(null);
  const editors = useEditors();
  const guardAlerts = useGuardrailAlerts();
  const checkpointCache = useCheckpointDetailsCache();
  const items = projects.data ?? [];
  const project = items.find((item) => item.id === selectedProjectId);
  const confirmRollback = useCallback((checkpoint: Checkpoint) => {
    if (project) setAction({ kind: "rollback", project, checkpoint });
  }, [project]);
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
    if (selectedProjectId === null) return;
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "b") {
        event.preventDefault();
        setCollapsed((value) => !value);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [selectedProjectId]);

  // ⌘K abre o cierra la paleta en cualquier pantalla (salvo con un diálogo
  // abierto). Se escucha en fase de captura para que ningún input ni componente
  // se lo quede, y también llega por el acelerador del menú nativo
  // (`open-command-palette`); si llegan los dos, el segundo se descarta.
  const actionOpen = useRef(false);
  actionOpen.current = action !== undefined;
  const lastToggle = useRef(0);
  useEffect(() => {
    const toggle = () => {
      if (actionOpen.current) return;
      const now = Date.now();
      if (now - lastToggle.current < 400) return;
      lastToggle.current = now;
      setGuardOpen(false);
      setPalette((open) => !open);
    };
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        event.stopImmediatePropagation();
        toggle();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    let active = true;
    let unlisten: (() => void) | undefined;
    void listen("open-command-palette", toggle).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }, () => undefined);
    return () => {
      active = false;
      unlisten?.();
      window.removeEventListener("keydown", onKey, { capture: true });
    };
  }, []);

  useEffect(() => {
    let active = true;
    invoke<Project[]>("list_projects").then(
      (data) => { if (active) setProjects({ data }); },
      (error: unknown) => { if (active) setProjects({ error: errorMessage(error) }); },
    );
    return () => { active = false; };
  }, [attempt]);

  function selectProject(id: string | null) {
    setFocus(undefined);
    setGuardOpen(false);
    setSelectedProjectId(id);
  }

  function openWith(editorId: Parameters<typeof openInEditor>[0], path: string, line: number | null = null) {
    openInEditor(editorId, path, line).catch((reason: unknown) => setNotice(errorMessage(reason)));
  }

  async function startUndo(current: Project) {
    try {
      const plan = await invoke<UndoAgentPlan | null>("preview_undo_last_agent", { projectId: current.id });
      if (!plan) setNotice("Todavía no hay sesiones de agentes en este proyecto");
      else setAction({ kind: "undo", project: current, plan });
    } catch (reason) {
      setNotice(errorMessage(reason));
    }
  }

  function paletteCommands(): PaletteCommand[] {
    const commands: PaletteCommand[] = [];
    const installed = editors.editors;
    // ----------------------------------------------------------- Acciones
    if (project) {
      commands.push(
        { id: "manual", group: "Acciones", title: "Crear checkpoint manual", icon: <BookmarkPlus size={14} />,
          keywords: "guardar snapshot", run: () => setAction({ kind: "manual", project }) },
        { id: "undo", group: "Acciones", title: "Deshacer último agente", icon: <Undo2 size={14} />,
          keywords: "revertir sesión rollback", run: () => startUndo(project) },
        { id: "guardrails", group: "Acciones", title: "Firewall de archivos…", icon: <Shield size={14} />,
          keywords: "guardrails proteger bloquear env", run: () => setGuardOpen(true) },
      );
      if (target) {
        const name = target.path.split("/").pop() ?? target.path;
        for (const editor of installed) {
          commands.push({
            id: `file-${editor.id}`, group: "Acciones",
            title: editor.id === "finder" ? `Mostrar ${name} en Finder` : `Abrir ${name} en ${editor.name}`,
            hint: target.line ? `línea ${target.line}` : undefined, icon: <FileCode size={14} />,
            keywords: "editor archivo actual", run: () => openWith(editor.id, target.path, target.line),
          });
        }
      }
      for (const editor of installed) {
        commands.push({
          id: `project-${editor.id}`, group: "Acciones",
          title: editor.id === "finder" ? "Mostrar proyecto en Finder" : `Abrir proyecto en ${editor.name}`,
          icon: editor.id === "finder" ? <FolderOpen size={14} /> : <Code size={14} />,
          keywords: "editor carpeta", run: () => openWith(editor.id, project.rootPath),
        });
      }
    }
    commands.push({ id: "add", group: "Acciones", title: "Agregar proyecto…", icon: <Plus size={14} />,
      keywords: "abrir carpeta finder repositorio", run: () => addFromFinder() });

    // --------------------------------------------------------- Navegación
    if (project) {
      commands.push(
        { id: "view-timeline", group: "Navegación", title: "Timeline de agentes", icon: <ListTree size={14} />,
          hint: view === "timeline" ? "actual" : undefined, keywords: "vista checkpoints historial", run: () => setView("timeline") },
        { id: "view-graph", group: "Navegación", title: "Grafo Git", icon: <GitBranch size={14} />,
          hint: view === "graph" ? "actual" : undefined, keywords: "vista ramas commits", run: () => setView("graph") },
        { id: "sidebar", group: "Navegación", title: collapsed ? "Mostrar barra lateral" : "Ocultar barra lateral",
          shortcut: "⌘B", icon: <PanelLeft size={14} />, run: () => setCollapsed((value) => !value) },
        { id: "home", group: "Navegación", title: "Volver al inicio", icon: <House size={14} />, run: () => selectProject(null) },
      );
    }

    // ---------------------------------------------------------- Proyectos
    for (const item of items) {
      if (item.id === project?.id) continue;
      commands.push({
        id: `switch-${item.id}`, group: "Proyectos", title: item.name, icon: <FolderOpen size={14} />,
        hint: item.rootPath.replace(/^\/Users\/[^/]+/, "~"), keywords: "proyecto ir cambiar", run: () => selectProject(item.id),
      });
    }
    return commands;
  }

  async function addProject(path: string) {
    const { project: added, created } = await invoke<{ project: Project; created: boolean }>("add_project", { path: path.trim() });
    setProjects(({ data = [] }) => ({
      data: [...data.filter((item) => item.id !== added.id), added]
        .sort((a, b) => a.name.localeCompare(b.name, "es", { sensitivity: "base" })),
    }));
    selectProject(added.id);
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
        setSelectedProjectId((current) => current === action.project.id ? null : current);
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

  const homeButton = (
    <button className="sidebar-wordmark" aria-label="Nude · Volver al inicio"
      title="Volver al inicio" onClick={() => selectProject(null)}>
      <Wordmark height={collapsed ? 7 : 11} title={null} />
    </button>
  );

  return (
    <>
      {project ? (
        <div className="workspace" data-sidebar={collapsed ? "collapsed" : "open"}>
          <aside className="sidebar pane" aria-label="Proyectos">
            <header className="sidebar-header">
              {!collapsed && homeButton}
              <button className="icon-button" onClick={() => setCollapsed((value) => !value)}
                aria-label={collapsed ? "Mostrar barra lateral" : "Ocultar barra lateral"}
                title={`${collapsed ? "Mostrar" : "Ocultar"} barra lateral (⌘B)`} aria-expanded={!collapsed}>
                {collapsed ? <PanelLeftOpen size={15} aria-hidden="true" /> : <PanelLeftClose size={15} aria-hidden="true" />}
              </button>
            </header>
            {collapsed && homeButton}
            {!collapsed && (
              <>
                <div className="sidebar-section">
                  <h2 id="projects-heading">Proyectos</h2>
                  <span className="flex">
                    <button className="icon-button" aria-label="Pegar ruta de un proyecto" title="Pegar ruta"
                      onClick={() => setAction({ kind: "add" })}>
                      <ClipboardPaste size={13} aria-hidden="true" />
                    </button>
                    <button className="icon-button" aria-label="Abrir carpeta en Finder" title="Abrir carpeta (Finder)"
                      onClick={() => void addFromFinder()}>
                      <Plus size={15} aria-hidden="true" />
                    </button>
                  </span>
                </div>
                <div className="project-list">
                  <ul aria-labelledby="projects-heading">
                    {items.map((item) => (
                      <ProjectRow key={item.id} project={item} active={project.id === item.id}
                        onSelect={() => selectProject(item.id)}
                        onRemove={() => setAction({ kind: "remove", project: item })} />
                    ))}
                  </ul>
                </div>
              </>
            )}
          </aside>
          <TopBar key={`topbar:${project.id}`} project={project} view={view} setView={setView} revision={revision}
            onUndo={(plan) => setAction({ kind: "undo", project, plan })}
            guardOpen={guardOpen} setGuardOpen={setGuardOpen} onPalette={() => setPalette(true)}
            onPreviewUrl={(previewUrl) => setProjects((previous) => ({
              ...previous, data: previous.data?.map((item) => item.id === project.id ? { ...item, previewUrl } : item),
            }))} />
          {view === "graph" ? (
            <GitGraphView key={project.id} project={project} revision={revision} onAction={onGraphAction}
              onNotice={setNotice}
              onOpenCheckpoint={(id) => { setFocus(id); setView("timeline"); }} />
          ) : (
            <TimelineView key={project.id} project={project} cache={checkpointCache} revision={revision} focus={focus} onRefresh={refresh}
              onRollback={confirmRollback}
              onManual={() => setAction({ kind: "manual", project })}
              editors={editors} onTarget={setTarget} onNotice={setNotice}
              onWatchPath={(path) => addProject(path)} />
          )}
        </div>
      ) : (
        <ProjectLauncher projects={projects}
          onOpen={selectProject} onFinder={() => void addFromFinder()} onPaste={() => setAction({ kind: "add" })}
          onRetry={() => { setProjects({}); setAttempt((current) => current + 1); }} />
      )}
      <GuardrailToasts alerts={guardAlerts.alerts} dismiss={guardAlerts.dismiss}
        onOpenProject={(id) => { selectProject(id); setView("timeline"); }} />
      {palette && <CommandPalette commands={paletteCommands()} close={() => setPalette(false)} />}
      <div className="notice" role="status">{notice && <span key={revision + notice}>{notice}</span>}</div>
      {action && (
        <ActionDialog action={action} close={() => setAction(undefined)}
          submit={(value, flag) => perform(action, value, flag)} />
      )}
    </>
  );
}

export default App;
