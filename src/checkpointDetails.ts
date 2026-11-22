import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { errorMessage, type FileAlert, type FileDiff } from "./workspace";

interface PreviewEvent {
  checkpointId: string;
  url: string;
  capturedAt: number;
}

/** Respuesta de `get_checkpoint_preview`: metadatos + ruta del PNG en disco. */
interface PreviewFile extends PreviewEvent {
  imagePath: string;
}

export interface CachedPreview {
  /** URL `asset:` del PNG (vía `convertFileSrc`), nunca Base64. */
  src: string;
  url: string;
  capturedAt: number;
}

/** Lo que el inspector necesita de un checkpoint. `preview` es `null` si no hay
 *  captura y `undefined` si todavía no se ha consultado. */
export interface CheckpointDetailsEntry {
  files: FileDiff[];
  alerts: FileAlert[];
  preview?: CachedPreview | null;
}

/** Checkpoints recordados (los más antiguos se descartan primero). */
const CACHE_LIMIT = 300;
/** Ninguna carga puede quedarse en "Cargando cambios…" para siempre. */
const REQUEST_TIMEOUT_MS = 20_000;

function withTimeout<T>(promise: Promise<T>): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = window.setTimeout(
      () => reject(new Error("El checkpoint tardó demasiado en cargar. Reintenta.")),
      REQUEST_TIMEOUT_MS,
    );
    promise.then(
      (value) => { window.clearTimeout(timer); resolve(value); },
      (error: unknown) => { window.clearTimeout(timer); reject(error); },
    );
  });
}

function previewFrom(file: PreviewFile | null): CachedPreview | null {
  if (!file) return null;
  // `?v=` evita que WebKit reutilice la imagen anterior si se recaptura.
  return { src: `${convertFileSrc(file.imagePath)}?v=${file.capturedAt}`, url: file.url, capturedAt: file.capturedAt };
}

/**
 * Caché en memoria de diffs, avisos y capturas por checkpoint, compartida por
 * todo el espacio de trabajo. Cada checkpoint se pide una sola vez aunque se
 * visite varias veces mientras la petición está en curso.
 */
export class CheckpointDetails {
  private readonly cache = new Map<string, CheckpointDetailsEntry>();
  private readonly inflight = new Map<string, Promise<CheckpointDetailsEntry>>();
  private readonly listeners = new Set<(checkpointId: string) => void>();

  /** Lectura síncrona, sin IPC. */
  peek(checkpointId: string | undefined): CheckpointDetailsEntry | undefined {
    return checkpointId ? this.cache.get(checkpointId) : undefined;
  }

  /** `true` si ya está todo (diff, avisos y captura) y no hace falta IPC. */
  isComplete(checkpointId: string) {
    const entry = this.cache.get(checkpointId);
    return entry !== undefined && entry.preview !== undefined;
  }

  subscribe(listener: (checkpointId: string) => void) {
    this.listeners.add(listener);
    return () => { this.listeners.delete(listener); };
  }

  private store(checkpointId: string, entry: CheckpointDetailsEntry) {
    this.cache.delete(checkpointId); // reinsertar = más reciente
    this.cache.set(checkpointId, entry);
    while (this.cache.size > CACHE_LIMIT) {
      const oldest = this.cache.keys().next().value;
      if (oldest === undefined) break;
      this.cache.delete(oldest);
    }
  }

  /** Se recapturó la imagen del checkpoint: solo se vuelve a pedir la captura. */
  invalidatePreview(checkpointId: string) {
    const entry = this.cache.get(checkpointId);
    if (!entry) return;
    this.cache.set(checkpointId, { ...entry, preview: undefined });
    this.listeners.forEach((listener) => listener(checkpointId));
  }

  /** Completa lo que falte del checkpoint. Lo que llega se guarda siempre en
   *  caché, aunque la vista ya muestre otro checkpoint. */
  fetch(projectId: string, checkpointId: string): Promise<CheckpointDetailsEntry> {
    const running = this.inflight.get(checkpointId);
    if (running) return running;

    const known = this.cache.get(checkpointId);
    const preview = invoke<PreviewFile | null>("get_checkpoint_preview", { projectId, checkpointId })
      .then(previewFrom, () => null);
    const request = withTimeout(known
      ? preview.then((image) => ({ ...known, preview: image }))
      : Promise.all([
        invoke<FileDiff[]>("get_checkpoint_diffs", { checkpointId }),
        // Los avisos y la captura son complementarios: si fallan, no bloquean el diff.
        invoke<FileAlert[]>("get_checkpoint_alerts", { checkpointId }).catch((): FileAlert[] => []),
        preview,
      ]).then(([files, alerts, image]) => ({ files, alerts, preview: image })))
      .then((entry) => {
        this.store(checkpointId, entry);
        return entry;
      })
      .finally(() => this.inflight.delete(checkpointId));
    this.inflight.set(checkpointId, request);
    return request;
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

interface DetailsView {
  checkpointId?: string;
  entry?: CheckpointDetailsEntry;
  error?: string;
}

/**
 * Detalles del checkpoint seleccionado, con guard de peticiones obsoletas:
 * cada cambio de selección incrementa `requestCounterRef` y cualquier respuesta
 * con un `reqId` antiguo se descarta (aunque sí queda en la caché). Lo ya
 * visitado se muestra al instante, sin IPC, aunque `ready` sea `false` (por
 * ejemplo, mientras se arrastra el scrubber).
 */
export function useCheckpointDetails(
  cache: CheckpointDetails,
  projectId: string,
  checkpointId: string | undefined,
  ready: boolean,
) {
  const requestCounterRef = useRef(0);
  const currentRef = useRef(checkpointId);
  currentRef.current = checkpointId;
  const [view, setView] = useState<DetailsView>({});
  const [version, setVersion] = useState(0);

  useEffect(() => cache.subscribe((id) => {
    if (id === currentRef.current) setVersion((value) => value + 1);
  }), [cache]);

  useEffect(() => {
    const reqId = ++requestCounterRef.current;
    if (!checkpointId) {
      setView({});
      return;
    }
    const cached = cache.peek(checkpointId);
    // Se sustituye la vista al momento: nada del checkpoint anterior (ni su
    // captura) queda pegado mientras carga el nuevo.
    setView({ checkpointId, entry: cached });
    if (cache.isComplete(checkpointId) || !ready) return;

    cache.fetch(projectId, checkpointId).then(
      (entry) => {
        if (reqId !== requestCounterRef.current) return;
        setView({ checkpointId, entry });
      },
      (reason: unknown) => {
        if (reqId !== requestCounterRef.current) return;
        setView({ checkpointId, entry: cache.peek(checkpointId), error: errorMessage(reason) });
      },
    );
  }, [cache, projectId, checkpointId, ready, version]);

  // Hasta que corre el efecto, nunca se pinta la vista de otro checkpoint.
  const current: DetailsView = view.checkpointId === checkpointId
    ? view
    : { checkpointId, entry: cache.peek(checkpointId) };
  return { ...current, retry: () => setVersion((value) => value + 1) };
}
