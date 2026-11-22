import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";

export interface Project {
  id: string;
  name: string;
  rootPath: string;
  previewUrl: string | null;
}

export type AgentTool = "claude-code" | "codex" | "opencode" | "manual";

export interface Checkpoint {
  id: string;
  projectId: string;
  agentTool: AgentTool;
  modelName: string | null;
  promptSummary: string | null;
  filesChangedCount: number;
  createdAt: string;
  /** Orden de inserción en SQLite: desempata checkpoints con el mismo `createdAt`. */
  seq: number;
}

/** Orden cronológico estricto y determinista: `createdAt`, `seq` e `id` ascendentes. */
export function compareCheckpointsAsc(a: Checkpoint, b: Checkpoint) {
  if (a.createdAt !== b.createdAt) return a.createdAt < b.createdAt ? -1 : 1;
  if (a.seq !== b.seq) return a.seq - b.seq;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

export interface FileDiff {
  filePath: string;
  changeType: "added" | "modified" | "deleted";
  patchText: string | null;
}

export type Resource<T> =
  | { data?: undefined; error?: undefined }
  | { data: T; error?: string }
  | { data?: undefined; error: string };

export function errorMessage(error: unknown) {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return "Ocurrió un error inesperado.";
}

export const fileCount = (count: number) => `${count} ${count === 1 ? "archivo" : "archivos"}`;

/**
 * Checkpoints del proyecto (del más reciente al más antiguo). Con `hold`
 * activo (el usuario está moviendo el scrubber) la lista no se recarga: los
 * avisos `checkpoint-created` quedan pendientes y se aplican al soltar, para
 * que el arreglo que alimenta el scrubber no cambie bajo el puntero.
 */
export function useCheckpoints(projectId: string, revision: number, hold = false) {
  const [resource, setResource] = useState<Resource<Checkpoint[]>>({});
  const [live, setLive] = useState<"connecting" | "on" | "off">("connecting");
  const holdRef = useRef(hold);
  const staleRef = useRef(false);
  const refreshRef = useRef<() => void>(() => undefined);
  holdRef.current = hold;

  useEffect(() => {
    if (!hold && staleRef.current) {
      staleRef.current = false;
      refreshRef.current();
    }
  }, [hold]);

  useEffect(() => {
    let active = true;
    let request = 0;
    let unlisten: (() => void) | undefined;
    setLive("connecting");
    refreshRef.current = () => void refresh();

    async function refresh() {
      const current = ++request;
      try {
        const data = await invoke<Checkpoint[]>("get_project_checkpoints", { projectId });
        if (active && current === request) setResource({ data });
      } catch (error) {
        if (active && current === request) {
          setResource((previous) => ({ data: previous.data, error: errorMessage(error) }));
        }
      }
    }

    async function subscribe() {
      try {
        const stop = await listen<Checkpoint>("checkpoint-created", ({ payload }) => {
          if (!active || payload.projectId !== projectId) return;
          if (holdRef.current) staleRef.current = true;
          else void refresh();
        });
        if (!active) {
          stop();
          return;
        }
        unlisten = stop;
        setLive("on");
      } catch {
        if (active) setLive("off");
      }
      // Subscribe first so a checkpoint created during the initial query is covered.
      if (active) void refresh();
    }

    void subscribe();
    return () => {
      active = false;
      unlisten?.();
    };
  }, [projectId, revision]);

  return { resource, live };
}

// ------------------------------------------------------------ Fase 4: Git

export interface HeadInfo {
  branch: string | null;
  commitId: string | null;
  detached: boolean;
  unborn: boolean;
}

export interface BranchInfo {
  name: string;
  commitId: string | null;
  isHead: boolean;
  upstream: string | null;
}

export interface CommitInfo {
  id: string;
  shortId: string;
  parentIds: string[];
  authorName: string;
  authorEmail: string;
  date: string;
  summary: string;
  message: string;
}

export type StatusKind =
  | "added" | "modified" | "deleted" | "renamed" | "typechange" | "untracked" | "conflicted";

export interface StatusEntry {
  path: string;
  kind: StatusKind;
}

export interface CheckpointAnchor {
  checkpointId: string;
  baseCommit: string | null;
  agentTool: AgentTool;
  modelName: string | null;
  promptSummary: string | null;
  filesChangedCount: number;
  createdAt: string;
}

export interface GitGraph {
  head: HeadInfo;
  branches: BranchInfo[];
  commits: CommitInfo[];
  status: { staged: StatusEntry[]; unstaged: StatusEntry[]; conflicted: StatusEntry[] };
  operation: string | null;
  checkpoints: CheckpointAnchor[];
}

export interface CommitResult {
  commitId: string;
  shortId: string;
  branch: string | null;
  filesCommitted: number;
}

export interface FileAlert {
  filePath: string;
  kind: "sensitive" | "loop";
  category: "secrets" | "dependencies" | "migrations" | "deployment" | null;
  message: string;
  editStreak: number | null;
}

export interface UndoAgentPlan {
  sessionId: string;
  agentTool: AgentTool;
  modelName: string | null;
  sessionCheckpoints: number;
  laterCheckpoints: number;
  startedAt: string;
  targetCheckpoint: Checkpoint | null;
  baseCommit: string | null;
  alreadyUndone: boolean;
}

export const agentLabels: Record<AgentTool, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  manual: "Manual",
};

/**
 * Ejecuta `load` al montar, cuando cambia `deps`, al crearse un checkpoint del
 * proyecto y al volver el foco a la ventana (el usuario pudo usar la terminal).
 */
export function useProjectQuery<T>(
  projectId: string,
  revision: number,
  load: () => Promise<T>,
) {
  const [resource, setResource] = useState<Resource<T>>({});
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let active = true;
    let request = 0;
    let unlisten: (() => void) | undefined;

    async function refresh() {
      const current = ++request;
      try {
        const data = await load();
        if (active && current === request) setResource({ data });
      } catch (error) {
        if (active && current === request) {
          const message = errorMessage(error);
          setResource((previous): Resource<T> =>
            previous.data !== undefined ? { data: previous.data, error: message } : { error: message });
        }
      }
    }

    const onFocus = () => void refresh();
    window.addEventListener("focus", onFocus);
    void listen<Checkpoint>("checkpoint-created", ({ payload }) => {
      if (active && payload.projectId === projectId) void refresh();
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }, () => undefined);
    void refresh();

    return () => {
      active = false;
      unlisten?.();
      window.removeEventListener("focus", onFocus);
    };
    // `load` se recrea en cada render; las dependencias reales son estas.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId, revision, tick]);

  return { resource, reload: () => setTick((t) => t + 1) };
}

export function useGitGraph(projectId: string, revision: number) {
  return useProjectQuery(projectId, revision, () => invoke<GitGraph>("get_git_graph", { projectId }));
}

export function useUndoPlan(projectId: string, revision: number) {
  return useProjectQuery(projectId, revision, () =>
    invoke<UndoAgentPlan | null>("preview_undo_last_agent", { projectId }));
}

/** Nombre sugerido para una rama de prueba antes de lanzar un agente. */
export function suggestedBranchName(now = new Date()) {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `prueba/agente-${pad(now.getMonth() + 1)}${pad(now.getDate())}-${pad(now.getHours())}${pad(now.getMinutes())}`;
}

/**
 * Convierte un prompt con Markdown crudo en una línea legible: quita bloques
 * y marcas de código, encabezados, énfasis, citas, viñetas, enlaces y saltos
 * de línea (reales o escapados como `\n`).
 */
export function cleanPrompt(text: string | null | undefined): string {
  if (!text) return "";
  return text
    .replace(/\\[nrt]/g, "\n")
    .replace(/```[\w-]*\s*/g, " ")
    .replace(/`+/g, "")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}#{1,6}\s+/gm, "")
    .replace(/^\s*>\s?/gm, "")
    .replace(/^\s*(?:[-*+•]|\d+[.)])\s+/gm, "")
    .replace(/(\*\*|__|~~)(.+?)\1/g, "$2")
    .replace(/(^|[\s(])[*_]([^*_\s][^*_]*?)[*_](?=[\s).,:;!?]|$)/g, "$1$2")
    .replace(/[*#]{2,}/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

// ----------------------------------------------- Editores y firewall de archivos

export type EditorId = "cursor" | "vscode" | "zed" | "finder";

export interface EditorInfo {
  id: EditorId;
  name: string;
  installed: boolean;
}

export type GuardrailAction = "warn" | "auto_revert";

export interface Guardrail {
  id: number;
  projectId: string;
  pattern: string;
  action: GuardrailAction;
  createdAt: string;
}

export interface GuardrailViolation {
  filePath: string;
  pattern: string;
  action: GuardrailAction;
  changeType: FileDiff["changeType"];
  reverted: boolean;
  note: string | null;
}

export interface GuardrailAlert {
  projectId: string;
  projectName: string;
  agentTool: AgentTool;
  modelName: string | null;
  detectedAt: string;
  violations: GuardrailViolation[];
}

/** Archivo que está mirando el inspector (para "abrir en el editor"). */
export interface OpenTarget {
  path: string;
  line: number | null;
}

const EDITOR_KEY = "nude.editor";
const fallbackEditors: EditorInfo[] = [{ id: "finder", name: "Finder", installed: true }];

export function joinPath(root: string, relative: string) {
  return `${root.replace(/\/+$/, "")}/${relative.replace(/^\/+/, "")}`;
}

/** Editores instalados + el preferido (guardado en `localStorage`). */
export function useEditors() {
  const [editors, setEditors] = useState<EditorInfo[]>(fallbackEditors);
  const [preferred, setPreferredState] = useState<EditorId | null>(() => {
    try {
      return (window.localStorage.getItem(EDITOR_KEY) as EditorId | null) ?? null;
    } catch {
      return null;
    }
  });

  useEffect(() => {
    let active = true;
    invoke<EditorInfo[]>("list_editors").then(
      (data) => { if (active) setEditors(data.filter((editor) => editor.installed)); },
      () => undefined,
    );
    return () => { active = false; };
  }, []);

  const setPreferred = (id: EditorId) => {
    setPreferredState(id);
    try {
      window.localStorage.setItem(EDITOR_KEY, id);
    } catch {
      // Solo es una preferencia.
    }
  };
  const current = editors.find((editor) => editor.id === preferred)
    ?? editors.find((editor) => editor.id !== "finder")
    ?? editors[0];
  return { editors, current, setPreferred };
}

export function openInEditor(editor: EditorId, path: string, line: number | null = null) {
  return invoke<void>("open_in_editor", { path, line, editor });
}

export function useGuardrails(projectId: string) {
  const [resource, setResource] = useState<Resource<Guardrail[]>>({});
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let active = true;
    invoke<Guardrail[]>("list_guardrails", { projectId }).then(
      (data) => { if (active) setResource({ data }); },
      (error: unknown) => { if (active) setResource({ error: errorMessage(error) }); },
    );
    return () => { active = false; };
  }, [projectId, tick]);

  const reload = () => setTick((t) => t + 1);
  return {
    resource,
    async set(pattern: string, action: GuardrailAction) {
      await invoke<Guardrail>("set_guardrail", { projectId, pattern, action });
      reload();
    },
    async remove(guardrailId: number) {
      await invoke("remove_guardrail", { guardrailId });
      reload();
    },
  };
}

/** Alertas del firewall en vivo (todas las de la sesión de la app). */
export function useGuardrailAlerts() {
  const [alerts, setAlerts] = useState<(GuardrailAlert & { key: number })[]>([]);
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    let next = 0;
    void listen<GuardrailAlert>("guardrail-triggered", ({ payload }) => {
      if (active) setAlerts((current) => [{ ...payload, key: next++ }, ...current].slice(0, 4));
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }, () => undefined);
    return () => {
      active = false;
      unlisten?.();
    };
  }, []);
  return { alerts, dismiss: (key: number) => setAlerts((current) => current.filter((a) => a.key !== key)) };
}

/**
 * Puntuación de búsqueda difusa para la paleta: las letras de `query` deben
 * aparecer en orden. Premia coincidencias seguidas y al inicio de palabra.
 * `null` si no coincide.
 */
export function fuzzyScore(query: string, text: string): number | null {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const t = text.toLowerCase();
  let score = 0;
  let from = 0;
  let previous = -2;
  for (const char of q) {
    if (char === " ") continue;
    const index = t.indexOf(char, from);
    if (index < 0) return null;
    score += index === previous + 1 ? 3 : 1;
    if (index === 0 || /[\s/·:-]/.test(t[index - 1])) score += 2;
    previous = index;
    from = index + 1;
  }
  return score - t.length * 0.01;
}

// ------------------------------------------------- Diagnóstico del watcher

export type WatchState = "starting" | "active" | "error" | "stopped";

export interface WatchStatus {
  projectId: string;
  state: WatchState;
  error: string | null;
  watchingSince: string | null;
  lastEventAt: string | null;
  lastScanAt: string | null;
  lastCheckpointAt: string | null;
}

export interface AgentSighting {
  agentTool: AgentTool;
  modelName: string | null;
  cwd: string | null;
  lastActivity: string;
  attributable: boolean;
}

export interface WorktreeDiagnostic {
  path: string;
  branch: string | null;
  projectId: string | null;
  lastSession: AgentSighting | null;
}

export interface WatcherDiagnostics {
  status: WatchStatus;
  rootPath: string;
  lastSession: AgentSighting | null;
  worktrees: WorktreeDiagnostic[];
  attributionWindowMinutes: number;
  firewallWindowMinutes: number;
}

/** Estado de vigilancia del proyecto, en vivo (evento `watcher-status`). */
export function useWatchStatus(projectId: string) {
  const [status, setStatus] = useState<WatchStatus | null>(null);
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    const load = () => invoke<WatchStatus>("get_watcher_status", { projectId }).then(
      (data) => { if (active) setStatus(data); },
      () => undefined,
    );
    void listen<WatchStatus>("watcher-status", ({ payload }) => {
      if (active && payload.projectId === projectId) setStatus(payload);
    }).then((stop) => {
      if (active) unlisten = stop;
      else stop();
    }, () => undefined);
    void load();
    window.addEventListener("focus", load);
    return () => {
      active = false;
      unlisten?.();
      window.removeEventListener("focus", load);
    };
  }, [projectId]);
  return { status, setStatus };
}

// ------------------------------------------------------- carriles paralelos

export type LaneStatus = "active" | "merged" | "discarded";

export interface LaneFile {
  filePath: string;
  changeType: FileDiff["changeType"];
}

export interface LaneActivity {
  agentTool: AgentTool;
  modelName: string | null;
  cwd: string | null;
  lastActivity: string;
  /** Escrita en los últimos 15 min: el agente sigue trabajando. */
  attributable: boolean;
}

/** Carril activo (`list_parallel_lanes`). */
export interface Lane {
  id: string;
  projectId: string;
  name: string;
  branchName: string;
  worktreePath: string;
  agentHint: AgentTool | null;
  status: LaneStatus;
  /** Milisegundos Unix. */
  createdAt: number;
  laneProjectId: string | null;
  baseBranch: string | null;
  files: LaneFile[];
  conflicts: string[];
  lastActivity: LaneActivity | null;
  checkpointCount: number;
  lastCheckpointAt: string | null;
  error: string | null;
}

export interface LaneDiff {
  laneId: string;
  baseBranch: string | null;
  baseCommit: string;
  files: FileDiff[];
  conflicts: string[];
}

export interface MergeLaneResult {
  merged: boolean;
  conflicts: string[];
  commitId: string | null;
  branch: string | null;
  filesMerged: number;
  cleanupError: string | null;
}

export function createLane(projectId: string, agentHint: AgentTool | null = null, name = "") {
  return invoke<Lane>("create_parallel_lane", { projectId, name, agentHint });
}

/**
 * Carriles activos del proyecto. Se refresca cuando cambian (`lanes-changed`),
 * cuando el watcher captura un checkpoint dentro de un carril y, como red de
 * seguridad, cada 15 s mientras haya carriles.
 */
export function useLanes(projectId: string | undefined, revision: number) {
  const [resource, setResource] = useState<Resource<Lane[]>>({});
  const [tick, setTick] = useState(0);
  const laneProjects = useRef(new Set<string>());

  useEffect(() => {
    setResource({});
  }, [projectId]);

  useEffect(() => {
    if (!projectId) return;
    let active = true;
    invoke<Lane[]>("list_parallel_lanes", { projectId }).then(
      (data) => {
        if (!active) return;
        laneProjects.current = new Set(data.flatMap((lane) => lane.laneProjectId ? [lane.laneProjectId] : []));
        setResource({ data });
      },
      (error: unknown) => {
        if (!active) return;
        const message = errorMessage(error);
        setResource((previous) => previous.data ? { data: previous.data, error: message } : { error: message });
      },
    );
    return () => { active = false; };
  }, [projectId, revision, tick]);

  useEffect(() => {
    if (!projectId) return;
    let timer: number | undefined;
    const soon = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => setTick((value) => value + 1), 400);
    };
    let active = true;
    const stops: (() => void)[] = [];
    const subscribe = <T,>(event: string, handler: (payload: T) => void) => {
      void listen<T>(event, ({ payload }) => handler(payload)).then((stop) => {
        if (active) stops.push(stop);
        else stop();
      }, () => undefined);
    };
    subscribe<string>("lanes-changed", (changed) => { if (changed === projectId) soon(); });
    subscribe<Checkpoint>("checkpoint-created", (checkpoint) => {
      if (laneProjects.current.has(checkpoint.projectId) || checkpoint.projectId === projectId) soon();
    });
    // El agente pudo trabajar mientras la ventana estaba en segundo plano.
    window.addEventListener("focus", soon);
    const poll = window.setInterval(() => {
      if (laneProjects.current.size > 0 && document.visibilityState === "visible") setTick((value) => value + 1);
    }, 15_000);
    return () => {
      active = false;
      window.clearTimeout(timer);
      window.clearInterval(poll);
      window.removeEventListener("focus", soon);
      stops.forEach((stop) => stop());
    };
  }, [projectId]);

  return { resource, reload: () => setTick((value) => value + 1) };
}

/** Copia texto al portapapeles (con alternativa si WebKit lo bloquea). */
export async function copyText(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    if (!ok) throw new Error("No se pudo copiar al portapapeles.");
  }
}

/** `cd` listo para pegar en la terminal (la ruta lleva espacios). */
export const cdCommand = (path: string) => `cd '${path.replace(/'/g, `'\\''`)}'`;

/** `/Users/nombre/...` → `~/...` */
export const tildePath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
