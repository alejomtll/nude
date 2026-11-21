import { Search } from "lucide-react";
import { type KeyboardEvent, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { fuzzyScore } from "./workspace";

/** Orden fijo de las secciones de la paleta. */
export const PALETTE_GROUPS = ["Acciones", "Navegación", "Proyectos"] as const;
export type PaletteGroup = (typeof PALETTE_GROUPS)[number];

export interface PaletteCommand {
  id: string;
  group: PaletteGroup;
  title: string;
  /** Texto secundario (ruta, línea…), en gris y monoespaciado. */
  hint?: string;
  /** Atajo de teclado, alineado a la derecha (p. ej. `⌘B`). */
  shortcut?: string;
  /** Palabras extra para la búsqueda que no se muestran. */
  keywords?: string;
  icon?: ReactNode;
  run: () => void | Promise<void>;
}

/**
 * Paleta de comandos (⌘K). Se monta con un portal en `document.body` para que
 * ningún panel con `overflow` o contexto de apilamiento la recorte. Búsqueda
 * difusa, ↑/↓ (o ⌃N/⌃P) para moverse, ↵ para ejecutar, `Esc` o clic fuera
 * para cerrar.
 */
export function CommandPalette({ commands, close }: { commands: PaletteCommand[]; close: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLUListElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);

  // Devuelve el foco a donde estaba al cerrar.
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    input.current?.focus();
    return () => previous?.focus();
  }, []);

  const results = useMemo(() => {
    const byGroup = (a: PaletteCommand, b: PaletteCommand) =>
      PALETTE_GROUPS.indexOf(a.group) - PALETTE_GROUPS.indexOf(b.group);
    if (!query.trim()) return [...commands].sort(byGroup);
    return commands
      .map((command, order) => ({
        command,
        order,
        score: fuzzyScore(query, `${command.title} ${command.group} ${command.keywords ?? ""}`),
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

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const move = (delta: number) => {
      event.preventDefault();
      setActive((index) => (results.length ? (index + delta + results.length) % results.length : 0));
    };
    if (event.key === "ArrowDown" || (event.ctrlKey && event.key === "n")) move(1);
    else if (event.key === "ArrowUp" || (event.ctrlKey && event.key === "p")) move(-1);
    else if (event.key === "Enter") {
      event.preventDefault();
      execute(results[active]);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab") {
      event.preventDefault(); // el foco se queda en la paleta
    }
  }

  const grouped = !query.trim();
  const activeId = results[active] ? `palette-${results[active].id}` : undefined;

  return createPortal(
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/60 px-4 pt-[14vh]"
      onMouseDown={(event) => { if (event.target === event.currentTarget) close(); }}>
      <div role="dialog" aria-modal="true" aria-label="Paleta de comandos" onKeyDown={onKeyDown}
        className="w-full max-w-[540px] overflow-hidden rounded-lg border border-[#2E2E2E] bg-[#181818] shadow-2xl">
        <div className="flex items-center gap-2.5 border-b border-[#2E2E2E] px-3.5 text-[#5C5C5C]">
          <Search size={14} aria-hidden="true" className="shrink-0" />
          <input ref={input} value={query} onChange={(event) => setQuery(event.target.value)}
            placeholder="Escribe un comando o un proyecto…" spellCheck={false} autoComplete="off"
            role="combobox" aria-expanded="true" style={{ outline: "none" }} aria-controls="palette-results" aria-activedescendant={activeId}
            className="h-12 min-w-0 flex-1 bg-transparent text-[14px] text-[#EDEDED] outline-none placeholder:text-[#5C5C5C]" />
          <kbd className="rounded border border-[#2E2E2E] px-1.5 py-px font-mono text-[10px] text-[#5C5C5C]">esc</kbd>
        </div>
        <ul id="palette-results" ref={list} role="listbox" aria-label="Comandos"
          className="max-h-[360px] divide-y divide-[#222222] overflow-y-auto">
          {results.map((command, index) => {
            const selected = index === active;
            const header = grouped && command.group !== results[index - 1]?.group;
            return (
              <li key={command.id} role="presentation">
                {header && (
                  <div role="presentation" className="px-3.5 pb-1 pt-2.5 text-[11px] font-medium text-[#5C5C5C]">
                    {command.group}
                  </div>
                )}
                <div id={`palette-${command.id}`} role="option" aria-selected={selected} data-index={index}
                  onMouseMove={() => setActive(index)} onClick={() => execute(command)}
                  className={`flex min-h-9 cursor-pointer items-center gap-2.5 px-3.5 ${selected
                    ? "bg-[#262626] text-[#EDEDED] shadow-[inset_2px_0_0_#EDEDED]"
                    : "text-[#A3A3A3]"}`}>
                  <span aria-hidden="true" className={`grid w-4 shrink-0 place-items-center ${selected ? "text-[#EDEDED]" : "text-[#5C5C5C]"}`}>
                    {command.icon}
                  </span>
                  <span className="min-w-0 truncate">{command.title}</span>
                  <span className="ml-auto flex min-w-0 shrink-0 items-center gap-2 pl-3">
                    {command.hint && (
                      <span className="max-w-[180px] truncate font-mono text-[11px] text-[#5C5C5C]">{command.hint}</span>
                    )}
                    {command.shortcut && (
                      <kbd className="rounded border border-[#2E2E2E] px-1.5 py-px font-mono text-[10px] text-[#8A8A8A]">
                        {command.shortcut}
                      </kbd>
                    )}
                  </span>
                </div>
              </li>
            );
          })}
          {!results.length && (
            <li className="px-3.5 py-6 text-center text-[#5C5C5C]">Ningún comando coincide con «{query}».</li>
          )}
        </ul>
      </div>
    </div>,
    document.body,
  );
}
