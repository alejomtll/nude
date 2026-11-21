/**
 * Wordmark "Nude" en tipografía modular de bloques: cada letra es una matriz
 * de celdas cuadradas (6 × 7) con trazos de dos módulos y contraformas
 * cuadradas. Se dibuja con rectángulos y `crispEdges` para que no se emborrone.
 */
const GLYPHS: Record<string, string[]> = {
  N: [
    "XX..XX",
    "XXX.XX",
    "XXXXXX",
    "XXXXXX",
    "XX.XXX",
    "XX..XX",
    "XX..XX",
  ],
  u: [
    "......",
    "......",
    "XX..XX",
    "XX..XX",
    "XX..XX",
    "XX..XX",
    "XXXXXX",
  ],
  d: [
    "....XX",
    "....XX",
    "XXXXXX",
    "XX..XX",
    "XX..XX",
    "XX..XX",
    "XXXXXX",
  ],
  e: [
    "......",
    "......",
    "XXXXXX",
    "XX..XX",
    "XXXXXX",
    "XX....",
    "XXXXXX",
  ],
};

const WIDTH = 6;
const HEIGHT = 7;
const GAP = 1;

function runs(text: string) {
  const rects: { x: number; y: number; w: number }[] = [];
  [...text].forEach((char, index) => {
    const glyph = GLYPHS[char];
    if (!glyph) return;
    const offset = index * (WIDTH + GAP);
    glyph.forEach((row, y) => {
      let start = -1;
      for (let x = 0; x <= row.length; x++) {
        const filled = row[x] === "X";
        if (filled && start < 0) start = x;
        if (!filled && start >= 0) {
          rects.push({ x: offset + start, y, w: x - start });
          start = -1;
        }
      }
    });
  });
  return rects;
}

const WORD = "Nude";
const RECTS = runs(WORD);
const VIEW_WIDTH = WORD.length * WIDTH + (WORD.length - 1) * GAP;

export function Wordmark({ height = 28, className, title = "Nude" }: {
  height?: number;
  className?: string;
  title?: string | null;
}) {
  return (
    <svg
      className={className}
      viewBox={`0 0 ${VIEW_WIDTH} ${HEIGHT}`}
      height={height}
      width={(height * VIEW_WIDTH) / HEIGHT}
      fill="currentColor"
      shapeRendering="crispEdges"
      role={title ? "img" : undefined}
      aria-label={title ?? undefined}
      aria-hidden={title ? undefined : true}
    >
      {RECTS.map(({ x, y, w }) => <rect key={`${x}-${y}`} x={x} y={y} width={w} height={1} />)}
    </svg>
  );
}
