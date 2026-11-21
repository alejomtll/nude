import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { errorMessage, type FileAlert, type FileDiff } from "./workspace";

interface PreviewEvent {
  checkpointId: string;
  url: string;
  capturedAt: number;
}

interface PreviewImage extends PreviewEvent {
  imageBase64: string;
}

export interface CachedPreview {
  src: string;
  url: string;
  capturedAt: number;
}

interface DiffData {
  files?: FileDiff[];
  alerts?: FileAlert[];
  error?: string;
  preview?: CachedPreview | null;
  previewVersion?: number;
}

const EMPTY: DiffData = {};

export class CheckpointDetails {
  private readonly cache = new Map<string, DiffData>();
  private readonly pending = new Map<string, symbol>();
  private readonly listeners = new Set<() => void>();

  subscribe = (listener: () => void) => {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  };

  get(checkpointId: string | undefined): DiffData {
    return (checkpointId && this.cache.get(checkpointId)) || EMPTY;
  }

  private update(checkpointId: string, values: Partial<DiffData>) {
    this.cache.set(checkpointId, { ...this.get(checkpointId), ...values });
    this.listeners.forEach((listener) => listener());
  }

  invalidatePreview(checkpointId: string) {
    if (!this.cache.has(checkpointId) && !this.pending.has(`preview:${checkpointId}`)) return;
    this.pending.delete(`preview:${checkpointId}`);
    this.update(checkpointId, {
      preview: undefined,
      previewVersion: (this.get(checkpointId).previewVersion ?? 0) + 1,
    });
  }

  load(projectId: string, checkpointId: string) {
    const data = this.get(checkpointId);
    if (!data.files && !data.error) {
      this.request(`diff:${checkpointId}`, () => invoke<FileDiff[]>("get_checkpoint_diffs", { checkpointId }),
        (files) => this.update(checkpointId, { files }),
        (reason) => this.update(checkpointId, { error: errorMessage(reason) }));
    }
    if (!data.alerts) {
      this.request(`alerts:${checkpointId}`, () => invoke<FileAlert[]>("get_checkpoint_alerts", { checkpointId }),
        (alerts) => this.update(checkpointId, { alerts }),
        () => this.update(checkpointId, { alerts: [] }));
    }
    if (data.preview === undefined) {
      this.request(`preview:${checkpointId}`, () => invoke<PreviewImage | null>(
        "get_checkpoint_preview", { projectId, checkpointId },
      ), (image) => this.update(checkpointId, {
        preview: image ? {
          src: `data:image/png;base64,${image.imageBase64}`,
          url: image.url,
          capturedAt: image.capturedAt,
        } : null,
      }), () => this.update(checkpointId, { preview: null }));
    }
  }

  retry(projectId: string, checkpointId: string) {
    this.update(checkpointId, { error: undefined });
    this.load(projectId, checkpointId);
  }

  private request<T>(key: string, fetch: () => Promise<T>, ready: (data: T) => void, failed: (error: unknown) => void) {
    if (this.pending.has(key)) return;
    const request = Symbol();
    this.pending.set(key, request);

    const finish = (apply: () => void) => {
      if (this.pending.get(key) !== request) return;
      this.pending.delete(key);
      apply();
    };
    void fetch().then(
      (value) => finish(() => ready(value)),
      (error: unknown) => finish(() => failed(error)),
    );
  }
}

export function useCheckpointDetailsCache() {
  const [cache] = useState(() => new CheckpointDetails());
  useEffect(() => {
    let active = true;
    let stop: (() => void) | undefined;
    void listen<PreviewEvent>("preview-captured", ({ payload }) => {
      if (active) cache.invalidatePreview(payload.checkpointId);
    }).then((unlisten) => {
      if (active) stop = unlisten;
      else unlisten();
    }, () => undefined);
    return () => { active = false; stop?.(); };
  }, [cache]);
  return cache;
}
