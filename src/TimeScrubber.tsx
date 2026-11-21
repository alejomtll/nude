import { RotateCcw } from "lucide-react";
import { useEffect, useRef } from "react";
import { agentLabels, type Checkpoint } from "./workspace";

const exactDate = new Intl.DateTimeFormat("es", { dateStyle: "medium", timeStyle: "medium" });

export function TimeScrubber({ checkpoints, selected, onSelect, onRestore, onDraggingChange }: {
  checkpoints: Checkpoint[];
  selected: Checkpoint;
  onSelect: (checkpoint: Checkpoint) => void;
  onRestore: (checkpoint: Checkpoint) => void;
  onDraggingChange: (dragging: boolean) => void;
}) {
  const pointer = useRef<number | null>(null);
  useEffect(() => {
    const endDrag = () => {
      if (pointer.current === null) return;
      pointer.current = null;
      onDraggingChange(false);
    };
    const release = (event: PointerEvent) => {
      if (event.pointerId === pointer.current) endDrag();
    };
    window.addEventListener("pointerup", release, true);
    window.addEventListener("pointercancel", release, true);
    window.addEventListener("blur", endDrag, true);
    return () => {
      window.removeEventListener("pointerup", release, true);
      window.removeEventListener("pointercancel", release, true);
      window.removeEventListener("blur", endDrag, true);
    };
  }, [onDraggingChange]);

  const last = checkpoints.length - 1;
  const index = last - checkpoints.findIndex((checkpoint) => checkpoint.id === selected.id);
  const date = exactDate.format(Date.parse(selected.createdAt));
  const agent = [agentLabels[selected.agentTool], selected.modelName].filter(Boolean).join(" · ");
  return (
    <section className="time-scrubber" aria-label="Recorrer checkpoints">
      <div className="scrubber-label">
        <label htmlFor="checkpoint-scrubber">Historial visual</label>
        <span className="mono">{index + 1} / {checkpoints.length}</span>
      </div>
      <input id="checkpoint-scrubber" className="scrubber-range" type="range"
        min={0} max={last} step={1} value={index} disabled={last === 0}
        aria-valuetext={`${date} · ${agent}`}
        onPointerDown={(event) => {
          if (!event.isPrimary || event.button !== 0) return;
          pointer.current = event.pointerId;
          onDraggingChange(true);
        }}
        onChange={(event) => {
          const checkpoint = checkpoints[last - Number(event.currentTarget.value)];
          if (checkpoint) onSelect(checkpoint);
        }} />
      <div className="scrubber-actions">
        <div className="min-w-0">
          <time dateTime={selected.createdAt} className="mono">{date}</time>
          <span className="scrubber-agent" title={agent}>{agent}</span>
        </div>
        <button className="button button-compact" type="button" onClick={() => onRestore(selected)}>
          <RotateCcw size={12} aria-hidden="true" />Restaurar este estado
        </button>
      </div>
    </section>
  );
}
