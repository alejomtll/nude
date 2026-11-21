import { Plus, Shield, ShieldAlert, X } from "lucide-react";
import { type FormEvent, useEffect, useRef, useState } from "react";
import {
  agentLabels, errorMessage, fileCount, useGuardrails,
  type GuardrailAction, type GuardrailAlert, type Project,
} from "./workspace";

const SUGGESTIONS = [".env*", "migrations/**", "Cargo.lock", "package-lock.json", "*.pem"];

const actionLabels: Record<GuardrailAction, string> = { warn: "Avisar", auto_revert: "Revertir" };
const actionTitles: Record<GuardrailAction, string> = {
  warn: "Avisa al momento y deja el cambio",
  auto_revert: "Avisa y restaura el archivo desde el último snapshot",
};

function ActionToggle({ value, onChange, disabled }: {
  value: GuardrailAction;
  onChange: (action: GuardrailAction) => void;
  disabled?: boolean;
}) {
  return (
    <span className="segmented" role="radiogroup" aria-label="Acción">
      {(["warn", "auto_revert"] as const).map((action) => (
        <button key={action} type="button" role="radio" aria-checked={value === action} title={actionTitles[action]}
          disabled={disabled} onClick={() => value !== action && onChange(action)}>
          {actionLabels[action]}
        </button>
      ))}
    </span>
  );
}

/**
 * Botón de la cabecera del proyecto con el popover del firewall de archivos.
 * `open`/`setOpen` vienen de fuera para que la paleta de comandos pueda abrirlo.
 */
export function GuardrailsButton({ project, open, setOpen }: {
  project: Project;
  open: boolean;
  setOpen: (open: boolean) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const { resource, set, remove } = useGuardrails(project.id);
  const [pattern, setPattern] = useState("");
  const [action, setAction] = useState<GuardrailAction>("auto_revert");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const rules = resource.data ?? [];

  useEffect(() => {
    if (!open) return;
    input.current?.focus();
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent && event.key !== "Escape") return;
      if (event instanceof MouseEvent && container.current?.contains(event.target as Node)) return;
      setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", close);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open, setOpen]);

  async function attempt(task: () => Promise<void>) {
    setPending(true);
    setError("");
    try {
      await task();
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setPending(false);
    }
  }

  function add(event?: FormEvent, value = pattern) {
    event?.preventDefault();
    if (!value.trim()) return;
    void attempt(async () => {
      await set(value, action);
      setPattern("");
    });
  }

  const suggestions = SUGGESTIONS.filter((item) => !rules.some((rule) => rule.pattern === item));
  return (
    <div ref={container} className="guard-anchor">
      <button className="button button-compact" aria-haspopup="dialog" aria-expanded={open}
        title="Firewall de archivos: rutas que los agentes no deben tocar" onClick={() => setOpen(!open)}>
        <Shield size={13} aria-hidden="true" />Firewall
        {rules.length > 0 && <span className="guard-count">{rules.length}</span>}
      </button>
      {open && (
        <div className="popover guard-popover" role="dialog" aria-label="Firewall de archivos">
          <header>
            <h2>Firewall de archivos</h2>
            <p>Si un agente toca estas rutas, Nude te avisa al instante. Con <strong>Revertir</strong> el archivo vuelve al último snapshot.</p>
          </header>
          {resource.error && <p className="error-text">{resource.error}</p>}
          {rules.length > 0 ? (
            <ul className="guard-rules" aria-label="Rutas protegidas">
              {rules.map((rule) => (
                <li key={rule.id}>
                  <code title={rule.pattern}>{rule.pattern}</code>
                  <ActionToggle value={rule.action} disabled={pending}
                    onChange={(next) => void attempt(() => set(rule.pattern, next))} />
                  <button className="icon-button" aria-label={`Quitar ${rule.pattern}`} title="Quitar regla"
                    disabled={pending} onClick={() => void attempt(() => remove(rule.id))}>
                    <X size={13} aria-hidden="true" />
                  </button>
                </li>
              ))}
            </ul>
          ) : resource.data && <p className="guard-empty">Sin rutas protegidas.</p>}
          <form className="guard-add" onSubmit={add}>
            <input ref={input} value={pattern} onChange={(event) => setPattern(event.target.value)}
              placeholder=".env*, migrations/**, Cargo.lock" spellCheck={false} aria-label="Patrón glob" disabled={pending} />
            <ActionToggle value={action} onChange={setAction} disabled={pending} />
            <button type="submit" className="icon-button" aria-label="Añadir regla" title="Añadir regla"
              disabled={pending || !pattern.trim()}>
              <Plus size={14} aria-hidden="true" />
            </button>
          </form>
          {suggestions.length > 0 && (
            <div className="guard-suggestions" aria-label="Sugerencias">
              {suggestions.map((item) => (
                <button key={item} type="button" className="ref-chip" disabled={pending}
                  title={`Proteger ${item} (${actionLabels[action]})`} onClick={() => add(undefined, item)}>
                  {item}
                </button>
              ))}
            </div>
          )}
          <p className="guard-help">
            <code>*</code> no cruza carpetas, <code>**</code> sí. Sin <code>/</code> el patrón vale en cualquier carpeta.
          </p>
          {error && <p className="error-text" role="alert">{error}</p>}
        </div>
      )}
    </div>
  );
}

const violationStatus = (reverted: boolean, action: GuardrailAction, note: string | null) =>
  reverted ? "restaurado" : action === "auto_revert" ? note ?? "no restaurado" : "solo aviso";

/** Avisos en vivo del evento `guardrail-triggered`. */
export function GuardrailToasts({ alerts, dismiss, onOpenProject }: {
  alerts: (GuardrailAlert & { key: number })[];
  dismiss: (key: number) => void;
  onOpenProject: (projectId: string) => void;
}) {
  if (!alerts.length) return null;
  return (
    <div className="guard-toasts" role="log" aria-live="assertive" aria-label="Avisos del firewall de archivos">
      {alerts.map((alert) => {
        const reverted = alert.violations.filter((v) => v.reverted).length;
        return (
          <section key={alert.key} className="guard-toast" data-agent={alert.agentTool}>
            <header>
              <ShieldAlert size={14} aria-hidden="true" />
              <button className="guard-toast-title" onClick={() => onOpenProject(alert.projectId)}
                title="Abrir el proyecto">
                <span className="agent-dot" aria-hidden="true" />
                {agentLabels[alert.agentTool]} tocó {fileCount(alert.violations.length)} protegido{alert.violations.length === 1 ? "" : "s"}
                <span className="text-faint"> · {alert.projectName}</span>
              </button>
              <button className="icon-button" aria-label="Cerrar aviso" onClick={() => dismiss(alert.key)}>
                <X size={13} aria-hidden="true" />
              </button>
            </header>
            <ul>
              {alert.violations.slice(0, 5).map((violation) => (
                <li key={violation.filePath}>
                  <code title={`${violation.filePath} · regla ${violation.pattern}`}>{violation.filePath}</code>
                  <span data-reverted={violation.reverted}>
                    {violationStatus(violation.reverted, violation.action, violation.note)}
                  </span>
                </li>
              ))}
              {alert.violations.length > 5 && <li className="text-faint">y {alert.violations.length - 5} más</li>}
            </ul>
            {reverted > 0 && <p>Restaurado desde el último snapshot. El agente podría volver a intentarlo.</p>}
          </section>
        );
      })}
    </div>
  );
}
