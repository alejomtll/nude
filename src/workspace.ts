import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

export interface Project {
  id: string;
  name: string;
  rootPath: string;
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

export function useCheckpoints(projectId: string, revision: number) {
  const [resource, setResource] = useState<Resource<Checkpoint[]>>({});
  const [live, setLive] = useState<"connecting" | "on" | "off">("connecting");

  useEffect(() => {
    let active = true;
    let request = 0;
    let unlisten: (() => void) | undefined;
    setLive("connecting");

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
          if (active && payload.projectId === projectId) void refresh();
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
