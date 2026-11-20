import { invoke } from "@tauri-apps/api/core";
import { FileCode2, FileDiff as FileDiffIcon, KeyRound, Repeat2, TriangleExclamationPoint } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { errorMessage, fileCount, type FileAlert, type FileDiff, type Resource } from "./workspace";

const changes = {
  added: { label: "Agregado", marker: "A" },
  modified: { label: "Modificado", marker: "M" },
  deleted: { label: "Eliminado", marker: "D" },
};

function UnifiedPatch({ patch }: { patch: string }) {
  const lines = useMemo(() => {
    const rows = patch.split("\n");
    if (rows[rows.length - 1] === "") rows.pop();
    const groups: { kind: string; lines: string[] }[] = [];
    let inHunk = false;
    for (const line of rows) {
      let kind = "meta";
      if (line.startsWith("@@ ")) {
        inHunk = true;
        kind = "hunk";
      } else if (inHunk) {
        if (line.startsWith("+")) kind = "added";
        else if (line.startsWith("-")) kind = "deleted";
        else if (line.startsWith(" ")) kind = "context";
      }

      const previous = groups[groups.length - 1];
      if (previous?.kind === kind) previous.lines.push(line);
      else groups.push({ kind, lines: [line] });
    }
    return groups;
  }, [patch]);

  return (
    <pre className="patch" aria-label="Parche unificado" tabIndex={0}>
      <code>{lines.map(({ kind, lines }, index) => (
        <span key={index} className={`patch-row patch-${kind}`}>{lines.join("\n")}</span>
      ))}</code>
    </pre>
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

function CheckpointDiffs({ checkpointId }: { checkpointId: string }) {
  const [resource, setResource] = useState<Resource<FileDiff[]>>({});
  const [alerts, setAlerts] = useState<FileAlert[]>([]);
  const [selected, setSelected] = useState<number>(0);
  const [attempt, setAttempt] = useState(0);

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
  const file = files[selected];
  if (!file) return <div className="empty-state">Este checkpoint no tiene cambios de archivos.</div>;

  const alertsByFile = new Map<string, FileAlert[]>();
  for (const alert of alerts) alertsByFile.set(alert.filePath, [...(alertsByFile.get(alert.filePath) ?? []), alert]);
  const fileAlerts = alertsByFile.get(file.filePath) ?? [];

  return (
    <>
      {alerts.length > 0 && <AlertSummary alerts={alerts} />}
      <div className="file-list">
        <div className="section-label px-2 pb-1">{fileCount(files.length)}</div>
        <ul aria-label="Archivos cambiados">
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
        <FileCode2 size={15} aria-hidden="true" className="shrink-0" />
        <span className="min-w-0 flex-1 truncate" title={file.filePath}>{file.filePath}</span>
        {fileAlerts.map((alert) => <AlertBadge key={alert.kind} alert={alert} />)}
      </div>
      {file.patchText ? (
        <UnifiedPatch key={selected} patch={file.patchText} />
      ) : (
        <div className="empty-state">Sin parche de texto disponible para este archivo.</div>
      )}
    </>
  );
}

export function DiffInspector({ checkpointId }: { checkpointId?: string }) {
  return (
    <aside className="inspector pane" aria-labelledby="inspector-heading">
      <header className="pane-header gap-2.5">
        <FileDiffIcon size={16} aria-hidden="true" className="text-muted" />
        <h2 id="inspector-heading">Inspector de diffs</h2>
      </header>
      {checkpointId ? <CheckpointDiffs key={checkpointId} checkpointId={checkpointId} /> : (
        <div className="empty-state">
          <FileDiffIcon size={26} aria-hidden="true" />
          <p>Selecciona un checkpoint para ver sus cambios.</p>
        </div>
      )}
    </aside>
  );
}
