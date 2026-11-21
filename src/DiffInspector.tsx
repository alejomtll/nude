import { invoke } from "@tauri-apps/api/core";
import { ChevronDown, KeyRound, Repeat2, SquareArrowOutUpRight, TriangleExclamationPoint } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  errorMessage, fileCount, joinPath, openInEditor, useEditors,
  type FileAlert, type FileDiff, type OpenTarget, type Resource,
} from "./workspace";

export type Editors = ReturnType<typeof useEditors>;

const changes = {
  added: { label: "Agregado", marker: "A" },
  modified: { label: "Modificado", marker: "M" },
  deleted: { label: "Eliminado", marker: "D" },
};

interface PatchLine {
  kind: "hunk" | "added" | "deleted" | "context" | "note";
  oldNo?: number;
  newNo?: number;
  text: string;
}

/**
 * Parche unificado → filas estilo GitHub con números de línea. Las cabeceras
 * (`diff --git`, `index`, `---`, `+++`) se omiten: la ruta ya está arriba.
 */
export function parsePatch(patch: string): PatchLine[] {
  const rows = patch.split("\n");
  if (rows[rows.length - 1] === "") rows.pop();
  const lines: PatchLine[] = [];
  let oldNo = 0;
  let newNo = 0;
  let inHunk = false;
  for (const row of rows) {
    const hunk = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(row);
    if (hunk) {
      inHunk = true;
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[2]);
      lines.push({ kind: "hunk", text: row });
    } else if (!inHunk) {
      continue;
    } else if (row.startsWith("+")) {
      lines.push({ kind: "added", newNo: newNo++, text: row.slice(1) });
    } else if (row.startsWith("-")) {
      lines.push({ kind: "deleted", oldNo: oldNo++, text: row.slice(1) });
    } else if (row.startsWith("\\")) {
      lines.push({ kind: "note", text: row.slice(2) });
    } else {
      lines.push({ kind: "context", oldNo: oldNo++, newNo: newNo++, text: row.slice(1) });
    }
  }
  return lines;
}

/** Primera línea cambiada del archivo nuevo (para abrir el editor ahí). */
export function firstChangedLine(patch: string | null): number | null {
  if (!patch) return null;
  const lines = parsePatch(patch);
  const added = lines.find((line) => line.kind === "added");
  if (added?.newNo) return added.newNo;
  // Solo borrados: la línea nueva que queda justo donde estaban.
  const index = lines.findIndex((line) => line.kind === "deleted");
  if (index < 0) return null;
  const next = lines.slice(index).find((line) => line.newNo !== undefined);
  const previous = lines.slice(0, index).reverse().find((line) => line.newNo !== undefined);
  return next?.newNo ?? (previous?.newNo !== undefined ? previous.newNo + 1 : 1);
}

/** Botón discreto "abrir en el editor" con selector del editor preferido. */
function OpenInEditorButton({ editors, target, onError }: {
  editors: Editors;
  target: OpenTarget | null;
  onError: (message: string) => void;
}) {
  const [menu, setMenu] = useState(false);
  const container = useRef<HTMLSpanElement>(null);
  const { current } = editors;

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

  if (!current) return null;
  const open = (id = current.id) => {
    if (!target) return;
    openInEditor(id, target.path, target.line).catch((reason: unknown) => onError(errorMessage(reason)));
  };
  const where = target?.line ? ` (línea ${target.line})` : "";
  return (
    <span ref={container} className="editor-split" data-menu={menu}>
      <button className="icon-button" disabled={!target} onClick={() => open()}
        title={target ? `Abrir en ${current.name}${where}` : "El archivo ya no existe"}
        aria-label={`Abrir en ${current.name}`}>
        <SquareArrowOutUpRight size={13} aria-hidden="true" />
      </button>
      {editors.editors.length > 1 && (
        <button className="icon-button editor-more" aria-label="Elegir editor" aria-haspopup="menu" aria-expanded={menu}
          disabled={!target} onClick={() => setMenu((value) => !value)}>
          <ChevronDown size={11} aria-hidden="true" />
        </button>
      )}
      {menu && (
        <div className="menu" role="menu">
          {editors.editors.map((editor, index) => (
            <button key={editor.id} role="menuitem" autoFocus={index === 0} data-current={editor.id === current.id}
              onClick={() => { setMenu(false); editors.setPreferred(editor.id); open(editor.id); }}>
              {editor.id === "finder" ? "Mostrar en Finder" : `Abrir en ${editor.name}`}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

const signs = { added: "+", deleted: "-", context: " ", hunk: "", note: "" };

function UnifiedPatch({ patch }: { patch: string }) {
  const lines = useMemo(() => parsePatch(patch), [patch]);
  if (!lines.length) return <div className="empty-state">Sin líneas que mostrar.</div>;
  return (
    <div className="patch" role="table" aria-label="Parche unificado" tabIndex={0}>
      {lines.map((line, index) => (
        <div key={index} className={`patch-line patch-${line.kind}`} role="row">
          <span className="patch-no" role="cell">{line.oldNo ?? ""}</span>
          <span className="patch-no" role="cell">{line.newNo ?? ""}</span>
          <span className="patch-code" role="cell">
            <span className="patch-sign" aria-hidden="true">{signs[line.kind]}</span>
            {line.text || " "}
          </span>
        </div>
      ))}
    </div>
  );
}

const categoryLabel: Record<NonNullable<FileAlert["category"]>, string> = {
  secrets: "Secretos",
  dependencies: "Dependencias",
  migrations: "Migración",
  deployment: "Despliegue",
};

function AlertIcon({ alert }: { alert: FileAlert }) {
  if (alert.kind === "loop") return <Repeat2 size={12} aria-hidden="true" />;
  if (alert.category === "secrets") return <KeyRound size={12} aria-hidden="true" />;
  return <TriangleExclamationPoint size={12} aria-hidden="true" />;
}

function AlertBadge({ alert }: { alert: FileAlert }) {
  const label = alert.kind === "loop" ? `Bucle ×${alert.editStreak ?? "?"}` : categoryLabel[alert.category ?? "deployment"];
  return (
    <span className="alert-badge" data-kind={alert.kind} title={alert.message}>
      <AlertIcon alert={alert} />{label}
    </span>
  );
}

function AlertSummary({ alerts }: { alerts: FileAlert[] }) {
  const sensitive = alerts.filter((a) => a.kind === "sensitive");
  const loops = alerts.filter((a) => a.kind === "loop");
  return (
    <div className="alert-summary" role="note" aria-label="Avisos de seguridad">
      {sensitive.length > 0 && (
        <p>
          <TriangleExclamationPoint size={14} aria-hidden="true" className="shrink-0" />
          <span><strong>Archivos sensibles:</strong> {sensitive.map((a) => a.filePath).join(", ")}. Revísalos antes de seguir.</span>
        </p>
      )}
      {loops.length > 0 && (
        <p data-kind="loop">
          <Repeat2 size={14} aria-hidden="true" className="shrink-0" />
          <span><strong>Posible bucle:</strong> {loops.map((a) => `${a.filePath} (${a.editStreak} veces seguidas)`).join(", ")}. Si el agente no avanza, detenlo o deshaz la sesión.</span>
        </p>
      )}
    </div>
  );
}

interface InspectorContext {
  projectRoot: string;
  editors: Editors;
  onTarget: (target: OpenTarget | null) => void;
}

function CheckpointDiffs({ checkpointId, projectRoot, editors, onTarget }: { checkpointId: string } & InspectorContext) {
  const [resource, setResource] = useState<Resource<FileDiff[]>>({});
  const [alerts, setAlerts] = useState<FileAlert[]>([]);
  const [selected, setSelected] = useState<number>(0);
  const [attempt, setAttempt] = useState(0);
  const [openError, setOpenError] = useState("");
  const file = resource.data?.[selected];
  const target = useMemo<OpenTarget | null>(() => (
    file && file.changeType !== "deleted"
      ? { path: joinPath(projectRoot, file.filePath), line: firstChangedLine(file.patchText) }
      : null
  ), [file, projectRoot]);

  useEffect(() => { onTarget(target); setOpenError(""); }, [target, onTarget]);
  useEffect(() => () => onTarget(null), [onTarget]);

  useEffect(() => {
    let active = true;
    invoke<FileDiff[]>("get_checkpoint_diffs", { checkpointId }).then(
      (data) => { if (active) setResource({ data }); },
      (error: unknown) => { if (active) setResource({ error: errorMessage(error) }); },
    );
    // Los avisos son complementarios: si fallan, el inspector sigue funcionando.
    invoke<FileAlert[]>("get_checkpoint_alerts", { checkpointId }).then(
      (data) => { if (active) setAlerts(data); },
      () => { if (active) setAlerts([]); },
    );
    return () => { active = false; };
  }, [checkpointId, attempt]);

  if (resource.error) {
    return (
      <div className="empty-state" role="alert">
        <p>{resource.error}</p>
        <button className="button mt-3" onClick={() => { setResource({}); setAttempt(attempt + 1); }}>
          Reintentar
        </button>
      </div>
    );
  }

  const files = resource.data;
  if (!files) return <div className="empty-state" role="status">Cargando cambios…</div>;
  if (!file) return <div className="empty-state">Este checkpoint no tiene cambios de archivos.</div>;

  const alertsByFile = new Map<string, FileAlert[]>();
  for (const alert of alerts) alertsByFile.set(alert.filePath, [...(alertsByFile.get(alert.filePath) ?? []), alert]);
  const fileAlerts = alertsByFile.get(file.filePath) ?? [];

  return (
    <>
      {alerts.length > 0 && <AlertSummary alerts={alerts} />}
      <div className="file-list">
        <div className="list-caption">{fileCount(files.length)}</div>
        <ul aria-label={`Archivos cambiados: ${fileCount(files.length)}`}>
          {files.map((item, index) => (
            <li key={index}>
              <button
                className="file-row"
                title={`${changes[item.changeType].label} · ${item.filePath}`}
                aria-pressed={index === selected}
                aria-label={`${item.filePath}, ${changes[item.changeType].label}${alertsByFile.has(item.filePath) ? ", con aviso" : ""}`}
                onClick={() => setSelected(index)}
              >
                <span className={`file-status ${item.changeType}`} aria-hidden="true">
                  {changes[item.changeType].marker}
                </span>
                <span className="min-w-0 flex-1 truncate" dir="auto">{item.filePath}</span>
                {alertsByFile.get(item.filePath)?.map((alert) => (
                  <span key={alert.kind} className="alert-dot" data-kind={alert.kind} title={alert.message}>
                    <AlertIcon alert={alert} />
                  </span>
                ))}
              </button>
            </li>
          ))}
        </ul>
      </div>
      <div className="patch-header">
        <span className={`file-status ${file.changeType}`} aria-label={changes[file.changeType].label}>
          {changes[file.changeType].marker}
        </span>
        <span className="patch-path" title={file.filePath} dir="ltr">{file.filePath}</span>
        {fileAlerts.map((alert) => <AlertBadge key={alert.kind} alert={alert} />)}
        <OpenInEditorButton editors={editors} target={target} onError={setOpenError} />
      </div>
      {openError && <p className="inline-error" role="alert">{openError}</p>}
      {file.patchText ? (
        <UnifiedPatch key={selected} patch={file.patchText} />
      ) : (
        <div className="empty-state">Sin parche de texto disponible para este archivo.</div>
      )}
    </>
  );
}

export function DiffInspector({ checkpointId, ...context }: { checkpointId?: string } & InspectorContext) {
  return (
    <aside className="inspector pane" aria-labelledby="inspector-heading">
      <header className="pane-header">
        <h2 id="inspector-heading" className="pane-title">Cambios</h2>
      </header>
      {checkpointId ? <CheckpointDiffs key={checkpointId} checkpointId={checkpointId} {...context} /> : (
        <div className="empty-state">
          <p>Selecciona un checkpoint para ver sus cambios.</p>
        </div>
      )}
    </aside>
  );
}
