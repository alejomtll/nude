import { LoaderCircle, ScanSearch, ShieldAlert, ShieldCheck } from "lucide-react";
import {
  activityLabels, auditorLabels, laneFingerprint, runLaneAudit, setLaneAuditor, useLaneAudit,
  type AuditorCli, type AuditReport, type Lane, type TerminalActivity,
} from "./workspace";

const auditors: AuditorCli[] = ["auto", "claude", "codex"];
const severityLabels = { high: "Alta", medium: "Media", low: "Baja" } as const;

/** Punto de estado de la terminal: verde trabajando, ámbar pulsante esperando. */
export function ActivityDot({ activity, className = "" }: { activity: TerminalActivity | null; className?: string }) {
  if (!activity) return null;
  return (
    <span className={`activity-dot ${className}`} data-activity={activity} role="img"
      aria-label={activityLabels[activity]} title={activityLabels[activity]} />
  );
}

/**
 * «Auditar cambios»: revisión local pre-merge con `claude -p` o `codex exec`
 * (solo lectura). El reporte se comparte entre la pestaña Cambios y el
 * diálogo de Aplicar.
 */
export function LaneAuditPanel({ lane, onSelectFile, variant = "lane" }: {
  lane: Lane;
  /** Pulsar un hallazgo con archivo lo abre en el diff. */
  onSelectFile?: (path: string) => void;
  variant?: "lane" | "dialog";
}) {
  const audit = useLaneAudit(lane.id);
  const fingerprint = laneFingerprint(lane);
  const stale = Boolean(audit.report && audit.fingerprint !== fingerprint);
  const disabled = audit.running || !lane.files.length || Boolean(lane.error);

  return (
    <div className="lane-audit" data-variant={variant}>
      <div className="lane-audit-bar">
        <button type="button" className="button button-compact" disabled={disabled}
          onClick={() => void runLaneAudit(lane.id, audit.auditor, fingerprint)}
          title="Revisión local con el CLI de tu equipo, en solo lectura (máx. 45 s)">
          {audit.running
            ? <LoaderCircle size={12} className="spin" aria-hidden="true" />
            : <ScanSearch size={12} aria-hidden="true" />}
          {audit.running ? "Auditando…" : audit.report ? "Volver a auditar" : "Auditar cambios"}
        </button>
        <label className="lane-audit-cli">
          <span className="sr-only">Auditor</span>
          <select value={audit.auditor} disabled={audit.running}
            onChange={(event) => setLaneAuditor(lane.id, event.target.value as AuditorCli)}>
            {auditors.map((cli) => <option key={cli} value={cli}>{auditorLabels[cli]}</option>)}
          </select>
        </label>
        {audit.running && <span className="text-faint truncate">{lane.files.length} archivos · hasta 45 s</span>}
      </div>
      {audit.error && !audit.running && <p className="lane-audit-error" role="alert">{audit.error}</p>}
      {audit.report && <AuditCard report={audit.report} stale={stale} onSelectFile={onSelectFile} />}
    </div>
  );
}

function AuditCard({ report, stale, onSelectFile }: {
  report: AuditReport;
  stale: boolean;
  onSelectFile?: (path: string) => void;
}) {
  const ready = report.verdict === "ready";
  return (
    <section className="audit-card" data-verdict={report.verdict} aria-label="Reporte de auditoría">
      <header className="audit-card-header">
        <span className="audit-verdict">
          {ready ? <ShieldCheck size={13} aria-hidden="true" /> : <ShieldAlert size={13} aria-hidden="true" />}
          {ready ? "Listo para aplicar" : "Requiere revisión"}
        </span>
        <span className="text-faint truncate">
          {auditorLabels[report.auditor]} · {report.filesReviewed} {report.filesReviewed === 1 ? "archivo" : "archivos"}
          {report.truncated ? " (diff recortado)" : ""} · {Math.max(1, Math.round(report.durationMs / 1000))} s
        </span>
      </header>
      {stale && <p className="audit-stale">El carril cambió desde esta auditoría.</p>}
      <p className="audit-summary">{report.summary}</p>
      {report.findings.length > 0 && (
        <ul className="audit-findings">
          {report.findings.map((finding, index) => (
            <li key={index}>
              <span className="audit-severity" data-severity={finding.severity}>{severityLabels[finding.severity]}</span>
              <span className="min-w-0">
                {finding.file && (onSelectFile
                  ? <button type="button" className="audit-file mono" onClick={() => onSelectFile(finding.file!)}
                    title="Ver en el diff">{finding.file}</button>
                  : <span className="audit-file mono">{finding.file}</span>)}
                <span className="audit-message">{finding.message}</span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
