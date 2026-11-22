import { invoke } from "@tauri-apps/api/core";
import { Expand, Image as ImageIcon, X } from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import { type CachedPreview } from "./checkpointDetails";
import { errorMessage, type Checkpoint, type Project } from "./workspace";

interface PreviewInfo {
  projectId: string;
  checkpointId: string;
  url: string;
  capturedAt: number;
}

const ports = [5173, 3000, 4321, 8080];
const captureTime = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "medium" });

export function PreviewButton({ project, onUrl }: {
  project: Project;
  onUrl: (url: string | null) => void;
}) {
  const popover = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [value, setValue] = useState(project.previewUrl ?? "");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState("");

  useEffect(() => {
    if (!open) return;
    input.current?.focus();
    function closeOutside(event: PointerEvent) {
      if (!popover.current?.contains(event.target as Node)) setOpen(false);
    }
    function onEscape(event: KeyboardEvent) {
      if (event.key !== "Escape") return;
      setOpen(false);
      trigger.current?.focus();
    }
    document.addEventListener("pointerdown", closeOutside);
    document.addEventListener("keydown", onEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOutside);
      document.removeEventListener("keydown", onEscape);
    };
  }, [open]);

  async function run(capture: boolean) {
    if (busy) return;
    setBusy(true);
    setFeedback("");
    try {
      const previewUrl = await invoke<string | null>("set_project_preview_url", {
        projectId: project.id,
        url: value.trim() || null,
      });
      onUrl(previewUrl);
      setValue(previewUrl ?? "");
      if (!capture || !previewUrl) {
        setFeedback(previewUrl ? "Preview activado." : "Preview desactivado.");
        return;
      }
      const [latest] = await invoke<Checkpoint[]>("get_project_checkpoints", { projectId: project.id });
      if (!latest) {
        setFeedback("Guarda un checkpoint para asociar la captura.");
        return;
      }
      const result = await invoke<PreviewInfo | null>("capture_checkpoint_preview", {
        projectId: project.id, checkpointId: latest.id,
      });
      setFeedback(result ? "Captura guardada en el último checkpoint." : "Captura no disponible. Revisa el servidor local e inténtalo de nuevo.");
    } catch (error) {
      setFeedback(errorMessage(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="preview-control" ref={popover}>
      <button type="button" className="button button-compact" aria-expanded={open}
        title={project.previewUrl ?? "Configurar preview"} ref={trigger} onClick={() => setOpen(!open)}>
        <ImageIcon size={13} aria-hidden="true" />Preview
        {project.previewUrl && <span className="preview-active" aria-label="Activado" />}
      </button>
      {open && (
        <form className="popover preview-popover" aria-label="Configurar Preview"
          onSubmit={(event: FormEvent) => { event.preventDefault(); void run(false); }}>
          <label className="dialog-field">
            URL del servidor local
            <input ref={input} value={value} spellCheck={false} disabled={busy}
              placeholder="http://localhost:5173" onChange={(event) => setValue(event.currentTarget.value)} />
          </label>
          <div className="mt-2 flex flex-wrap gap-1">
            {ports.map((port) => (
              <button key={port} type="button" className="button button-compact mono" disabled={busy}
                onClick={() => setValue(`http://localhost:${port}`)}>:{port}</button>
            ))}
          </div>
          <div className="mt-3 flex justify-between gap-2">
            <button className="button button-compact" disabled={busy} type="submit">Guardar</button>
            <button className="button button-compact" disabled={busy || !value.trim()} type="button"
              onClick={() => void run(true)}>{busy ? "Procesando…" : "Capturar ahora"}</button>
          </div>
          {feedback ? <p className="mt-2 text-muted" role="status">{feedback}</p> : (
            <p className="mt-2 text-muted">Guarda la URL vacía para desactivar las capturas.</p>
          )}
        </form>
      )}
    </div>
  );
}

export function CheckpointPreview({ image }: { image: CachedPreview }) {
  const [expanded, setExpanded] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);

  useEffect(() => {
    if (!expanded) return;
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, [expanded]);

  const date = captureTime.format(image.capturedAt);
  return (
    <div className="checkpoint-preview">
      <div className="preview-caption">
        <span>Captura visual</span>
        <time dateTime={new Date(image.capturedAt).toISOString()} title={image.url}>{date}</time>
      </div>
      <button type="button" className="preview-thumbnail" aria-label="Expandir captura visual"
        onClick={() => setExpanded(true)}>
        <img src={image.src} alt="Vista previa capturada en el checkpoint" decoding="async" draggable={false} />
        <Expand size={14} aria-hidden="true" />
      </button>
      {expanded && (
        <dialog ref={dialog} className="preview-modal" aria-label="Captura visual del checkpoint"
          onCancel={(event) => { event.preventDefault(); setExpanded(false); }}
          onClick={(event) => { if (event.currentTarget === event.target) setExpanded(false); }}>
          <div className="preview-modal-content">
            <div className="preview-caption">
              <span className="min-w-0 truncate" title={image.url}>{image.url}</span>
              <button type="button" className="icon-button" aria-label="Cerrar captura" onClick={() => setExpanded(false)}>
                <X size={16} aria-hidden="true" />
              </button>
            </div>
            <img src={image.src} alt={`Vista previa del ${date}`} decoding="async" />
          </div>
        </dialog>
      )}
    </div>
  );
}
