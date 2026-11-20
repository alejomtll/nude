import { invoke } from "@tauri-apps/api/core";
import {
  ArrowRightLeft, Check, Diamond, GitBranch, GitBranchPlus, GitCommitHorizontal, GitGraph as GitGraphIcon,
  RefreshCw, TriangleExclamationPoint,
} from "lucide-react";
import { type CSSProperties, type FormEvent, useMemo, useState } from "react";
import {
  agentLabels, errorMessage, fileCount, useGitGraph,
  type CheckpointAnchor, type CommitInfo, type CommitResult, type GitGraph, type Project, type StatusKind,
} from "./workspace";

// --------------------------------------------------------------- layout

const ROW = 38;
const LANE = 16;
const PAD = 14;
const WORKTREE_ID = "__worktree__";
const PALETTE = ["#b9a9f4", "#80dbb6", "#f4b38b", "#a4c9ff", "#f2d479", "#f59fc0", "#7fd4e6", "#c9e58a"];

interface Segment { from: number; to: number; color: string }
interface GraphRow {
  commit: CommitInfo;
  lane: number;
  color: string;
  /** Tramos de esta fila a la siguiente. */
  segments: Segment[];
  virtual: boolean;
}

/**
 * Asigna un carril a cada commit (orden topológico, más reciente primero).
 * Cada carril "espera" un commit concreto; nunca hay dos carriles esperando el
 * mismo, así que las uniones se dibujan directamente hacia el carril existente.
 */
export function layoutGraph(commits: CommitInfo[], virtualIds: Set<string> = new Set()) {
  const lanes: (string | null)[] = [];
  const laneColor: string[] = [];
  let nextColor = 0;
  const allocate = () => {
    let index = lanes.indexOf(null);
    if (index === -1) { index = lanes.length; lanes.push(null); }
    laneColor[index] = PALETTE[nextColor++ % PALETTE.length];
    return index;
  };

  let width = 1;
  const rows: GraphRow[] = commits.map((commit) => {
    let lane = lanes.indexOf(commit.id);
    if (lane === -1) lane = allocate();
    const color = laneColor[lane];
    lanes[lane] = null;

    const segments: Segment[] = [];
    const assigned = new Set<number>();
    commit.parentIds.forEach((parent, n) => {
      let target = lanes.indexOf(parent);
      if (target === -1) {
        if (n === 0 && lanes[lane] === null) {
          target = lane;
        } else {
          target = allocate();
        }
        lanes[target] = parent;
        assigned.add(target);
      }
      segments.push({ from: lane, to: target, color: laneColor[target] });
    });
    lanes.forEach((id, j) => {
      if (id !== null && !assigned.has(j)) segments.push({ from: j, to: j, color: laneColor[j] });
    });
    while (lanes.length && lanes[lanes.length - 1] === null) lanes.pop();
    width = Math.max(width, lanes.length, lane + 1);
    return { commit, lane, color, segments, virtual: virtualIds.has(commit.id) };
  });
  return { rows, width };
}

const laneX = (lane: number) => PAD + lane * LANE;
const rowY = (row: number) => row * ROW + ROW / 2;

function segmentPath(row: number, { from, to }: Segment) {
  const x1 = laneX(from);
  const x2 = laneX(to);
  const y1 = rowY(row);
  const y2 = rowY(row + 1);
  if (x1 === x2) return `M${x1} ${y1}V${y2}`;
  // Estilo metro: vertical, diagonal a 45° aprox. y vertical.
  return `M${x1} ${y1}V${y1 + ROW * 0.22}L${x2} ${y2 - ROW * 0.22}V${y2}`;
}

// ---------------------------------------------------------------- view

const relative = new Intl.RelativeTimeFormat("es", { numeric: "auto" });
function ago(date: string) {
  const minutes = Math.round((Date.now() - Date.parse(date)) / 60_000);
  if (minutes < 60) return minutes < 1 ? "ahora" : relative.format(-minutes, "minute");
  if (minutes < 1440) return relative.format(-Math.round(minutes / 60), "hour");
  if (minutes < 60 * 24 * 60) return relative.format(-Math.round(minutes / 1440), "day");
  return relative.format(-Math.round(minutes / 43200), "month");
}

export type GraphAction =
  | { kind: "create-branch"; fromCommit: string | null; label: string }
  | { kind: "checkout"; branch: string };

export function GitGraphView({ project, revision, onAction, onOpenCheckpoint, onNotice }: {
  project: Project;
  revision: number;
  onAction: (action: GraphAction) => void;
  onOpenCheckpoint: (checkpointId: string) => void;
  onNotice: (text: string) => void;
}) {
  const { resource, reload } = useGitGraph(project.id, revision);
  const graph = resource.data;

  return (
    <>
      <main className="pane graph-pane" aria-labelledby="graph-heading">
        <header className="pane-header justify-between gap-2">
          <div className="flex min-w-0 items-center gap-2.5">
            <GitGraphIcon size={17} className="shrink-0 text-muted" aria-hidden="true" />
            <h1 id="graph-heading" className="truncate">Grafo Git <span className="text-muted">/ {project.name}</span></h1>
          </div>
          <div className="flex items-center gap-1.5">
            <button className="button" disabled={!graph || graph.head.unborn}
              onClick={() => onAction({ kind: "create-branch", fromCommit: null, label: "HEAD" })}
              title="Crea una rama desde HEAD para probar un agente sin tocar tu rama">
              <GitBranchPlus size={14} aria-hidden="true" />Rama de prueba
            </button>
            <button className="icon-button" aria-label="Actualizar grafo" title="Actualizar" onClick={reload}>
              <RefreshCw size={14} aria-hidden="true" />
            </button>
          </div>
        </header>
        <div className="graph-scroll">
          {resource.error && (
            <div className="inline-error" role="alert">
              <span>{resource.error}</span>
              <button className="button" onClick={reload}>Reintentar</button>
            </div>
          )}
          {graph ? <CommitGraph graph={graph} onAction={onAction} onOpenCheckpoint={onOpenCheckpoint} />
            : !resource.error && <div className="empty-state" role="status">Leyendo el repositorio…</div>}
        </div>
      </main>
      <GitPanel project={project} graph={graph} onAction={onAction} onNotice={onNotice} onCommitted={reload} />
    </>
  );
}

function CommitGraph({ graph, onAction, onOpenCheckpoint }: {
  graph: GitGraph;
  onAction: (action: GraphAction) => void;
  onOpenCheckpoint: (checkpointId: string) => void;
}) {
  const changed = changedFiles(graph).length;
  const { rows, width, branchesByCommit, checkpointsByCommit } = useMemo(() => {
    const commits = [...graph.commits];
    const virtual = new Set<string>();
    if (changed > 0) {
      virtual.add(WORKTREE_ID);
      commits.unshift({
        id: WORKTREE_ID, shortId: "", parentIds: graph.head.commitId ? [graph.head.commitId] : [],
        authorName: "", authorEmail: "", date: new Date().toISOString(),
        summary: `Cambios sin commit · ${fileCount(changed)}`, message: "",
      });
    }
    const branchesByCommit = new Map<string, string[]>();
    for (const branch of graph.branches) {
      if (!branch.commitId) continue;
      branchesByCommit.set(branch.commitId, [...(branchesByCommit.get(branch.commitId) ?? []), branch.name]);
    }
    const checkpointsByCommit = new Map<string, CheckpointAnchor[]>();
    for (const anchor of graph.checkpoints) {
      if (!anchor.baseCommit) continue;
      checkpointsByCommit.set(anchor.baseCommit, [...(checkpointsByCommit.get(anchor.baseCommit) ?? []), anchor]);
    }
    return { ...layoutGraph(commits, virtual), branchesByCommit, checkpointsByCommit };
  }, [graph, changed]);

  if (!rows.length) {
    return (
      <div className="empty-state">
        <GitCommitHorizontal size={28} aria-hidden="true" />
        <div><h2>Sin commits todavía</h2><p>Haz el primer commit desde el panel de cambios.</p></div>
      </div>
    );
  }

  const svgWidth = PAD * 2 + (width - 1) * LANE;
  const head = graph.head;
  return (
    <div className="graph-rows" style={{ height: rows.length * ROW }}>
      <svg className="graph-svg" width={svgWidth} height={rows.length * ROW} aria-hidden="true">
        {rows.map((row, i) => row.segments.map((segment, k) => (
          <path key={`${i}-${k}`} d={segmentPath(i, segment)} stroke={segment.color}
            strokeDasharray={row.virtual ? "3 3" : undefined} />
        )))}
        {rows.map((row, i) => {
          const isHead = row.commit.id === head.commitId;
          const cx = laneX(row.lane);
          const cy = rowY(i);
          if (row.virtual) {
            return <circle key={row.commit.id} cx={cx} cy={cy} r={5} className="node-worktree" stroke={row.color} />;
          }
          return (
            <g key={row.commit.id}>
              {isHead && <circle cx={cx} cy={cy} r={8.5} className="node-head-ring" stroke={row.color} />}
              <circle cx={cx} cy={cy} r={row.commit.parentIds.length > 1 ? 4 : 5} fill={row.color}
                className="node" />
            </g>
          );
        })}
      </svg>
      <ol aria-label="Commits, del más reciente al más antiguo">
        {rows.map((row) => {
          const { commit } = row;
          const branches = branchesByCommit.get(commit.id) ?? [];
          const anchors = checkpointsByCommit.get(commit.id) ?? [];
          const isHead = commit.id === head.commitId;
          return (
            <li key={commit.id} className="graph-row" data-virtual={row.virtual}
              style={{ paddingLeft: svgWidth + 4, height: ROW }}>
              <span className="graph-labels">
                {isHead && head.detached && <span className="ref-chip ref-head">HEAD separado</span>}
                {branches.map((name) => {
                  const current = name === head.branch && isHead;
                  return (
                    <button key={name} className="ref-chip" data-current={current}
                      style={{ "--chip": row.color } as CSSProperties}
                      title={current ? `Rama actual: ${name}` : `Cambiar a ${name}`}
                      disabled={current} onClick={() => onAction({ kind: "checkout", branch: name })}>
                      {current ? <Check size={11} aria-hidden="true" /> : <GitBranch size={11} aria-hidden="true" />}
                      {name}
                    </button>
                  );
                })}
                {anchors.length > 0 && (
                  <button className="checkpoint-chip" data-agent={anchors[0].agentTool}
                    onClick={() => onOpenCheckpoint(anchors[0].checkpointId)}
                    title={anchors.slice(0, 6).map((a) =>
                      `${agentLabels[a.agentTool]} · ${a.promptSummary ?? "Sin resumen"}`).join("\n")}>
                    <Diamond size={10} aria-hidden="true" />
                    {anchors.length === 1 ? agentLabels[anchors[0].agentTool] : `${anchors.length} checkpoints`}
                  </button>
                )}
              </span>
              <span className="graph-summary" title={commit.message || commit.summary} dir="auto">
                {commit.summary || <em className="text-muted">Sin mensaje</em>}
              </span>
              {!row.virtual && (
                <span className="graph-meta">
                  <span className="truncate">{commit.authorName}</span>
                  <time dateTime={commit.date}>{ago(commit.date)}</time>
                  <code>{commit.shortId}</code>
                  <button className="row-action" aria-label={`Crear rama desde ${commit.shortId}`}
                    title="Crear rama de prueba aquí"
                    onClick={() => onAction({ kind: "create-branch", fromCommit: commit.id, label: commit.shortId })}>
                    <GitBranchPlus size={13} aria-hidden="true" />
                  </button>
                </span>
              )}
            </li>
          );
        })}
      </ol>
    </div>
  );
}

// --------------------------------------------------------------- panel

const statusMarker: Record<StatusKind, { marker: string; className: string; label: string }> = {
  added: { marker: "A", className: "added", label: "Nuevo" },
  untracked: { marker: "U", className: "added", label: "Sin seguimiento" },
  modified: { marker: "M", className: "modified", label: "Modificado" },
  renamed: { marker: "R", className: "modified", label: "Renombrado" },
  typechange: { marker: "T", className: "modified", label: "Cambio de tipo" },
  deleted: { marker: "D", className: "deleted", label: "Eliminado" },
  conflicted: { marker: "!", className: "deleted", label: "En conflicto" },
};

interface ChangedFile { path: string; kind: StatusKind; staged: boolean }

function changedFiles(graph: GitGraph): ChangedFile[] {
  const files = new Map<string, ChangedFile>();
  for (const entry of graph.status.conflicted) files.set(entry.path, { ...entry, staged: false });
  for (const entry of graph.status.unstaged) {
    if (!files.has(entry.path)) files.set(entry.path, { ...entry, staged: false });
  }
  for (const entry of graph.status.staged) {
    const existing = files.get(entry.path);
    files.set(entry.path, existing ? { ...existing, staged: true } : { ...entry, staged: true });
  }
  return [...files.values()].sort((a, b) => a.path.localeCompare(b.path));
}

function GitPanel({ project, graph, onAction, onNotice, onCommitted }: {
  project: Project;
  graph?: GitGraph;
  onAction: (action: GraphAction) => void;
  onNotice: (text: string) => void;
  onCommitted: () => void;
}) {
  const [excluded, setExcluded] = useState<Set<string>>(new Set());
  const [message, setMessage] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");

  const files = graph ? changedFiles(graph) : [];
  const selected = files.filter((f) => !excluded.has(f.path) && f.kind !== "conflicted");
  const toggle = (path: string) => setExcluded((current) => {
    const next = new Set(current);
    if (next.has(path)) next.delete(path); else next.add(path);
    return next;
  });
  const allSelected = files.length > 0 && files.every((f) => !excluded.has(f.path));

  async function commit(event: FormEvent) {
    event.preventDefault();
    if (pending || !selected.length || !message.trim()) return;
    setPending(true);
    setError("");
    try {
      const result = await invoke<CommitResult>("git_quick_commit", {
        projectId: project.id, message, paths: selected.map((f) => f.path),
      });
      setMessage("");
      setExcluded(new Set());
      onNotice(`Commit ${result.shortId} en ${result.branch ?? "HEAD"} · ${fileCount(result.filesCommitted)}`);
      onCommitted();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setPending(false);
    }
  }

  return (
    <aside className="inspector pane git-panel" aria-labelledby="git-panel-heading">
      <header className="pane-header gap-2.5">
        <GitBranch size={16} aria-hidden="true" className="text-muted" />
        <h2 id="git-panel-heading" className="truncate">
          {graph ? graph.head.branch ?? (graph.head.detached ? "HEAD separado" : "—") : "Rama"}
        </h2>
      </header>
      {!graph ? <div className="empty-state" role="status">Cargando…</div> : (
        <div className="git-panel-scroll">
          {graph.operation && (
            <p className="git-warning" role="alert">
              <TriangleExclamationPoint size={14} aria-hidden="true" />
              El repositorio está en mitad de un {graph.operation}. Termínalo en la terminal antes de cambiar de rama o hacer commit.
            </p>
          )}

          <section aria-labelledby="branches-heading">
            <div className="section-label px-1 pb-1" id="branches-heading">Ramas locales</div>
            {graph.branches.length ? (
              <ul className="branch-list">
                {graph.branches.map((branch) => (
                  <li key={branch.name} className="branch-row" data-current={branch.isHead}>
                    <GitBranch size={13} aria-hidden="true" className="shrink-0" />
                    <span className="min-w-0 flex-1 truncate" title={branch.name}>{branch.name}</span>
                    {branch.isHead ? <span className="text-muted text-[11px]">actual</span> : (
                      <button className="button button-quiet" onClick={() => onAction({ kind: "checkout", branch: branch.name })}>
                        <ArrowRightLeft size={12} aria-hidden="true" />Cambiar
                      </button>
                    )}
                  </li>
                ))}
              </ul>
            ) : <p className="px-1 text-muted">Sin ramas todavía.</p>}
          </section>

          <form className="commit-box" onSubmit={(event) => void commit(event)} aria-labelledby="changes-heading">
            <div className="flex items-center justify-between px-1 pb-1">
              <span className="section-label" id="changes-heading">Cambios · {fileCount(files.length)}</span>
              {files.length > 0 && (
                <button type="button" className="button button-quiet"
                  onClick={() => setExcluded(allSelected ? new Set(files.map((f) => f.path)) : new Set())}>
                  {allSelected ? "Ninguno" : "Todos"}
                </button>
              )}
            </div>
            {files.length ? (
              <ul className="change-list">
                {files.map((file) => {
                  const info = statusMarker[file.kind];
                  return (
                    <li key={file.path}>
                      <label className="change-row" title={`${info.label}${file.staged ? " · preparado" : ""} · ${file.path}`}>
                        <input type="checkbox" checked={!excluded.has(file.path) && file.kind !== "conflicted"}
                          disabled={file.kind === "conflicted" || pending} onChange={() => toggle(file.path)} />
                        <span className={`file-status ${info.className}`} aria-label={info.label}>{info.marker}</span>
                        <span className="min-w-0 flex-1 truncate" dir="auto">{file.path}</span>
                        {file.staged && <span className="staged-tag">preparado</span>}
                      </label>
                    </li>
                  );
                })}
              </ul>
            ) : <p className="px-1 pb-2 text-muted">El árbol de trabajo está limpio.</p>}
            <label className="dialog-field mt-2">
              <span className="sr-only">Mensaje del commit</span>
              <textarea rows={2} value={message} onChange={(event) => setMessage(event.target.value)}
                placeholder="Mensaje del commit" disabled={pending || !files.length || !!graph.operation} />
            </label>
            {error && <p className="error-text mt-2" role="alert">{error}</p>}
            <button type="submit" className="button button-primary mt-2 w-full"
              disabled={pending || !selected.length || !message.trim() || !!graph.operation}>
              <GitCommitHorizontal size={14} aria-hidden="true" />
              {pending ? "Haciendo commit…" : `Commit · ${fileCount(selected.length)}`}
            </button>
            <p className="mt-2 px-1 text-[11px] leading-relaxed text-muted">
              Solo entran los archivos marcados. Lo que ya tenías preparado y no marques sigue preparado.
            </p>
          </form>
        </div>
      )}
    </aside>
  );
}
