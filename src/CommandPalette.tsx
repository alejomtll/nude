import { CornerDownLeft, Search } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { fuzzyScore } from "./workspace";

export interface PaletteCommand {
  id: string;
  group: string;
  title: string;
  /** Texto secundario a la derecha (ruta, atajo…). */
  hint?: string;
  /** Palabras extra para la búsqueda que no se muestran. */
  keywords?: string;
  icon?: ReactNode;
  run: () => void | Promise<void>;
}

/**
 * Paleta de comandos (⌘K). Filtra con búsqueda difusa, se navega con ↑/↓ y
 * se ejecuta con ↵. `Esc` o un clic fuera la cierran.
 */
export function CommandPalette({ commands, close }: { commands: PaletteCommand[]; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);

  useEffect(() => {
    const element = dialog.current;
    element?.showModal();
    return () => element?.close();
  }, []);

  const results = useMemo(() => {
    if (!query.trim()) return commands;
    return commands
      .map((command, order) => ({
        command,
        order,
        score: fuzzyScore(query, `${command.group} ${command.title} ${command.keywords ?? ""}`),
      }))
      .filter((item): item is typeof item & { score: number } => item.score !== null)
      .sort((a, b) => b.score - a.score || a.order - b.order)
      .map((item) => item.command);
  }, [commands, query]);

  useEffect(() => setActive(0), [query]);
  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  function execute(command: PaletteCommand | undefined) {
    if (!command) return;
    close();
    void command.run();
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) {
      event.preventDefault();
      setActive((index) => (results.length ? (index + 1) % results.length : 0));
    } else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) {
      event.preventDefault();
      setActive((index) => (results.length ? (index - 1 + results.length) % results.length : 0));
    } else if (event.key === "Enter") {
      event.preventDefault();
      execute(results[active]);
    }
  }

  const grouped = !query.trim();
  return (
    <dialog ref={dialog} className="palette" aria-label="Paleta de comandos"
      onCancel={(event) => { event.preventDefault(); close(); }}
      onMouseDown={(event) => { if (event.target === dialog.current) close(); }}>
      <div className="palette-search">
        <Search size={14} aria-hidden="true" />
        <input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={onKeyDown}
          placeholder="Escribe un comando o un proyecto…" spellCheck={false}
          role="combobox" aria-expanded="true" aria-controls="palette-results"
          aria-activedescendant={results[active] ? `palette-${results[active].id}` : undefined} />
        <kbd>esc</kbd>
      </div>
      <ul id="palette-results" ref={list} className="palette-results" role="listbox" aria-label="Comandos">
        {results.map((command, index) => (
          <li key={command.id} role="presentation">
            {grouped && command.group !== results[index - 1]?.group && (
              <div className="palette-group" role="presentation">{command.group}</div>
            )}
            <div id={`palette-${command.id}`} role="option" aria-selected={index === active} data-index={index}
              className="palette-item" onMouseMove={() => setActive(index)} onClick={() => execute(command)}>
              <span className="palette-icon" aria-hidden="true">{command.icon}</span>
              <span className="palette-title">{command.title}</span>
              {command.hint && <span className="palette-hint">{command.hint}</span>}
              {index === active && <CornerDownLeft size={12} aria-hidden="true" className="palette-enter" />}
            </div>
          </li>
        ))}
        {!results.length && <li className="palette-empty">Ningún comando coincide con «{query}».</li>}
      </ul>
    </dialog>
  );
}
