import { ArrowRight, ClipboardPaste, Plus } from "lucide-react";
import { Wordmark } from "./Wordmark";
import { type Project, type Resource } from "./workspace";

export function ProjectLauncher({ projects, onOpen, onFinder, onPaste, onRetry }: {
  projects: Resource<Project[]>;
  onOpen: (id: string) => void;
  onFinder: () => void;
  onPaste: () => void;
  onRetry: () => void;
}) {
  return (
    <main className="launcher" aria-labelledby="launcher-title">
      <h1 id="launcher-title" className="sr-only">Nude</h1>
      <Wordmark height={74} title={null} className="launcher-wordmark" />
      <section className="launcher-projects mx-auto mt-8 max-w-md" aria-label="Proyectos registrados">
        {projects.error ? (
          <div className="px-3 py-2.5" role="alert">
            <p className="error-text">{projects.error}</p>
            <button className="button button-compact mt-2" onClick={onRetry}>Reintentar</button>
          </div>
        ) : projects.data ? (
          projects.data.length ? (
            <ul className="launcher-list">
              {projects.data.map((project) => (
                <li key={project.id}>
                  <button className="launcher-row" aria-label={`Abrir ${project.name}`}
                    title={project.rootPath} onClick={() => onOpen(project.id)}>
                    <span className="grid min-w-0 gap-0.5">
                      <span className="project-name" dir="auto">{project.name}</span>
                      <span className="launcher-path truncate font-mono" dir="ltr">
                        {project.rootPath.replace(/^\/Users\/[^/]+/, "~")}
                      </span>
                    </span>
                    <span className="launcher-open" aria-hidden="true">
                      Abrir <ArrowRight size={12} />
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          ) : <p className="px-3 py-2.5 text-muted">Sin proyectos registrados.</p>
        ) : <p className="px-3 py-2.5 text-muted" role="status">Cargando proyectos…</p>}
        <div className="launcher-actions">
          <button className="button button-compact button-quiet" disabled={!projects.data} onClick={onFinder}>
            <Plus size={14} aria-hidden="true" />Abrir carpeta en Finder
          </button>
          <button className="button button-compact button-quiet" disabled={!projects.data} onClick={onPaste}>
            <ClipboardPaste size={13} aria-hidden="true" />Pegar ruta
          </button>
        </div>
      </section>
    </main>
  );
}
