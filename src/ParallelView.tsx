import { invoke } from "@tauri-apps/api/core";
import {
  ChevronDown, Code, Copy, FolderOpen, GitBranch, GitMerge, Plus, RefreshCw, Trash2, TriangleAlert,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { UnifiedPatch, type Editors } from "./DiffInspector";
import {
  agentLabels, cdCommand, copyText, errorMessage, fileCount, openInEditor, tildePath,
  type AgentTool, type Lane, type LaneDiff, type Project, type Resource,
} from "./workspace";

const relative = new Intl.RelativeTimeFormat("es", { numeric: "auto" });
const markers = { added: "A", modified: "M", deleted: "D" } as const;
const agentChoices: AgentTool[] = ["claude-code", "codex", "opencode"];

function ago(time: number) {
  const minutes = Math.max(0, Math.floor((Date.now() - time) / 60_000));
  if (minutes < 1) return "ahora";
  if (minutes < 60) return relative.format(-minutes, "minute");
  if (minutes < 1440) return relative.format(-Math.floor(minutes / 60), "hour");
  return relative.format(-Math.floor(minutes / 1440), "day");
}

/**
 * Vista "Paralelo": un carril por columna (2–3 visibles), cada uno en su
 * propio worktree. Nada de lo que pasa aquí toca la rama del proyecto hasta
 * que el usuario pulsa "Aplicar".
 */
export function ParallelView({ project, lanes, onReload, editors, freshLaneId, onCreate, onMerge, onDiscard, onNotice }: {
  project: Project;
  lanes: Resource<Lane[]>;
  onReload: () => void;
  editors: Editors;
  /** Carril recién creado: se resalta y se desplaza a la vista. */
  freshLaneId: string | null;
  onCreate: (agentHint: AgentTool | null) => Promise<void>;
  onMerge: (lane: Lane) => void;
  onDiscard: (lane: Lane) => void;
  onNotice: (text: string) => void;
}) {
  const items = lanes.data ?? [];
  const [creating, setCreating] = useState(false);

  async function create(agentHint: AgentTool | null) {
    if (creating) return;
    setCreating(true);
    try {
      await onCreate(agentHint);
    } finally {
      setCreating(false);
    }
  }

  return (
    <main className="pane parallel-pane" aria-labelledby="parallel-heading">
      <header className="pane-header">
        <h1 id="parallel-heading" className="pane-title" title={project.rootPath}>
          {project.name}<span className="text-faint">Carriles paralelos</span>
        </h1>
        <div className="pane-actions">
          <NewLaneButton creating={creating} onCreate={(hint) => void create(hint)} />
          <button className="icon-button" aria-label="Actualizar carriles" title="Actualizar" onClick={onReload}>
            <RefreshCw size={13} aria-hidden="true" />
          </button>
        </div>
      </header>
      {lanes.error && (
        <div className="inline-error" role="alert">
          <span>{lanes.error}</span>
          <button className="button" onClick={onReload}>Reintentar</button>
        </div>
      )}
      {items.length > 0 ? (
        <div className="lane-grid" data-count={Math.min(items.length, 3)}>
          {items.map((lane) => (
            <LaneColumn key={lane.id} lane={lane} fresh={lane.id === freshLaneId} editors={editors}
              onMerge={() => onMerge(lane)} onDiscard={() => onDiscard(lane)} onNotice={onNotice} />
          ))}
        </div>
      ) : lanes.data ? (
        <div className="empty-state watermark">
          <GitBranch size={20} aria-hidden="true" />
          <div>
            <h2>Sin carriles activos</h2>
            <p>Cada carril es un worktree aislado sobre una rama <span className="mono">nude/lane-…</span> creada
              desde tu HEAD. Lanza un agente en cada uno, compáralos lado a lado y aplica solo el ganador.</p>
          </div>
          <button className="button button-primary" disabled={creating} onClick={() => void create(null)}>
            <Plus size={14} aria-hidden="true" />Nuevo carril aislado
          </button>
        </div>
      ) : !lanes.error && <div className="empty-state" role="status">Cargando carriles…</div>}
    </main>
  );
}

function NewLaneButton({ creating, onCreate }: { creating: boolean; onCreate: (hint: AgentTool | null) => void }) {
  const [menu, setMenu] = useState(false);
  const container = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const close = (event: MouseEvent) => {
      if (!container.current?.contains(event.target as Node)) setMenu(false);
    };
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") setMenu(false); };
    window.addEventListener("mousedown", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu]);

  return (
    <div className="split-button" ref={container}>
      <button className="button button-compact" disabled={creating} onClick={() => onCreate(null)}
        title="Crea un worktree aislado desde el HEAD actual">
        <Plus size={13} aria-hidden="true" />{creating ? "Creando…" : "Nuevo carril aislado"}
      </button>
      <button className="button button-compact" disabled={creating} aria-label="Elegir agente del carril"
        aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu((open) => !open)}>
        <ChevronDown size={12} aria-hidden="true" />
      </button>
      {menu && (
        <div className="menu" role="menu">
          {agentChoices.map((agent) => (
            <button key={agent} role="menuitem" data-agent={agent} onClick={() => { setMenu(false); onCreate(agent); }}>
              <span className="agent-dot" aria-hidden="true" />Carril para {agentLabels[agent]}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** Diff completo del carril; se vuelve a pedir cuando cambian sus archivos. */
function useLaneDiff(lane: Lane) {
  const fingerprint = `${lane.id}|${lane.checkpointCount}|${lane.lastCheckpointAt}|${lane.files.map((f) => f.changeType + f.filePath).join(",")}`;
  const [state, setState] = useState<{ key: string; diff?: LaneDiff; error?: string }>({ key: "" });
  useEffect(() => {
    let active = true;
    invoke<LaneDiff>("get_lane_diff", { laneId: lane.id }).then(
      (diff) => { if (active) setState({ key: fingerprint, diff }); },
      (error: unknown) => { if (active) setState({ key: fingerprint, error: errorMessage(error) }); },
    );
    return () => { active = false; };
    // `fingerprint` resume todo lo que puede cambiar el diff.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fingerprint]);
  return { ...state, stale: state.key !== fingerprint };
}

function LaneColumn({ lane, fresh, editors, onMerge, onDiscard, onNotice }: {
  lane: Lane;
  fresh: boolean;
  editors: Editors;
  onMerge: () => void;
  onDiscard: () => void;
  onNotice: (text: string) => void;
}) {
  const column = useRef<HTMLElement>(null);
  const { diff, error } = useLaneDiff(lane);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const conflicts = useMemo(() => new Set(lane.conflicts), [lane.conflicts]);
  const files = diff?.files ?? [];
  const selected = files.find((file) => file.filePath === selectedPath) ?? files[0];

  useEffect(() => {
    if (fresh) column.current?.scrollIntoView({ behavior: "smooth", inline: "nearest", block: "nearest" });
  }, [fresh]);

  const activity = lane.lastActivity;
  const agent = activity?.agentTool ?? lane.agentHint ?? "manual";
  const agentText = activity
    ? `${agentLabels[activity.agentTool]}${activity.modelName ? ` · ${activity.modelName}` : ""}`
    : lane.agentHint ? `${agentLabels[lane.agentHint]} · sin actividad todavía` : "Sin agente todavía";
  const target = lane.baseBranch ?? "la rama actual";

  async function copyPath() {
    try {
      await copyText(cdCommand(lane.worktreePath));
      onNotice("Comando cd copiado");
    } catch (reason) {
      onNotice(errorMessage(reason));
    }
  }

  return (
    <section ref={column} className="lane-column" data-fresh={fresh || undefined} aria-label={`Carril ${lane.name}`}>
      <header className="lane-header">
        <div className="lane-title">
          <h2 title={lane.name}>{lane.name}</h2>
          <span className="mono text-faint" title={lane.branchName}>{lane.branchName}</span>
        </div>
        <div className="lane-agent" data-agent={agent} data-live={activity?.attributable || undefined}>
          <span className="agent-dot" aria-hidden="true" />
          <span className="truncate" title={activity?.cwd ?? undefined}>{agentText}</span>
          <span className="text-faint">
            {activity ? ago(Date.parse(activity.lastActivity)) : `creado ${ago(lane.createdAt)}`}
          </span>
        </div>
        <div className="lane-open">
          <button className="lane-path mono" onClick={() => void copyPath()} title={`Copiar ${cdCommand(lane.worktreePath)}`}>
            <Copy size={11} aria-hidden="true" />
            <span className="truncate">cd {tildePath(lane.worktreePath)}</span>
          </button>
          {editors.editors.map((editor) => (
            <button key={editor.id} className="icon-button" title={editor.id === "finder" ? "Mostrar en Finder" : `Abrir en ${editor.name}`}
              aria-label={editor.id === "finder" ? "Mostrar carril en Finder" : `Abrir carril en ${editor.name}`}
              onClick={() => openInEditor(editor.id, lane.worktreePath).catch((reason: unknown) => onNotice(errorMessage(reason)))}>
              {editor.id === "finder" ? <FolderOpen size={13} aria-hidden="true" /> : <Code size={13} aria-hidden="true" />}
              <span className="lane-editor-name">{editor.id === "finder" ? "Finder" : editor.name}</span>
            </button>
          ))}
        </div>
      </header>

      {lane.error ? (
        <div className="inline-error" role="alert"><span>{lane.error}</span></div>
      ) : (
        <>
          <div className="lane-summary">
            <span>{lane.files.length ? fileCount(lane.files.length) : "Sin cambios"}</span>
            <span className="text-faint">· {lane.checkpointCount} checkpoints</span>
            {lane.conflicts.length > 0 && (
              <span className="lane-conflict" title={lane.conflicts.join("\n")}>
                <TriangleAlert size={11} aria-hidden="true" />{lane.conflicts.length} en conflicto con {target}
              </span>
            )}
          </div>
          {files.length > 0 && (
            <ul className="file-list lane-files">
              {files.map((file) => (
                <li key={file.filePath}>
                  <button className="file-row" aria-pressed={file === selected} onClick={() => setSelectedPath(file.filePath)}
                    title={file.filePath}>
                    <span className={`file-status ${file.changeType}`} aria-hidden="true">{markers[file.changeType]}</span>
                    <span className="min-w-0 flex-1 truncate" dir="auto">{file.filePath}</span>
                    {conflicts.has(file.filePath) && <TriangleAlert size={11} className="lane-conflict-icon" aria-label="en conflicto" />}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="lane-diff">
            {error ? <div className="inline-error" role="alert"><span>{error}</span></div>
              : selected ? (selected.patchText
                ? <UnifiedPatch key={selected.filePath} patch={selected.patchText} />
                : <div className="empty-state">Archivo binario o demasiado grande para mostrar.</div>)
              : !diff && lane.files.length ? <div className="empty-state" role="status">Cargando diff…</div>
              : <div className="empty-state">Abre el carril con tu agente; sus cambios aparecerán aquí.</div>}
          </div>
        </>
      )}

      <footer className="lane-footer">
        <button className="button button-primary" disabled={!lane.files.length || Boolean(lane.error)} onClick={onMerge}
          title={lane.conflicts.length ? `Hay conflictos con ${target}; se pedirá confirmación` : `Merge --no-ff en ${target}`}>
          <GitMerge size={13} aria-hidden="true" />Aplicar a {lane.baseBranch ?? "main"}
        </button>
        <button className="button button-danger" onClick={onDiscard}>
          <Trash2 size={13} aria-hidden="true" />Descartar carril
        </button>
      </footer>
    </section>
  );
}
