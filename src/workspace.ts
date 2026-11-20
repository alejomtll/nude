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
