// The window is a row of columns. A column holds either one spanning panel or a stack of
// panels; columns and stack dividers are resizable and sizes are kept as proportions.

import { useRef, useState, type ReactNode, type PointerEvent as ReactPointerEvent } from "react";

export type PanelSpec = { id: string; title: string; body: ReactNode; actions?: ReactNode };
export type ColumnSpec =
  | { id: string; mode: "span"; panel: PanelSpec }
  | { id: string; mode: "stack"; panels: PanelSpec[] };

const STORAGE_KEY = "imperium.layout";

function loadSizes(key: string, count: number): number[] {
  try {
    const saved = JSON.parse(localStorage.getItem(`${STORAGE_KEY}.${key}`) ?? "null") as number[] | null;
    if (saved?.length === count) return saved;
  } catch {
    // Fall through to equal sizes.
  }
  return Array(count).fill(1 / count);
}

function saveSizes(key: string, sizes: number[]): void {
  try {
    localStorage.setItem(`${STORAGE_KEY}.${key}`, JSON.stringify(sizes));
  } catch {
    // Not remembering a layout is harmless.
  }
}

/** Splits space between children along one axis, with draggable dividers between them. */
function Split({
  storageKey,
  axis,
  initial,
  className,
  children,
}: {
  storageKey: string;
  axis: "row" | "column";
  initial?: number[];
  /** Extra classes, so a column can be styled as a whole (the side rails). */
  className?: string;
  children: ReactNode[];
}) {
  const [sizes, setSizes] = useState(() =>
    initial && !localStorage.getItem(`${STORAGE_KEY}.${storageKey}`) ? initial : loadSizes(storageKey, children.length),
  );
  // Panels added or removed after the first render (mods load later) start from equal (or
  // remembered) sizes for the new count, so no panel is left without room.
  if (sizes.length !== children.length) {
    setSizes(loadSizes(storageKey, children.length));
  }
  const container = useRef<HTMLDivElement>(null);

  const startDrag = (index: number) => (event: ReactPointerEvent) => {
    const box = container.current?.getBoundingClientRect();
    if (!box) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const total = axis === "row" ? box.width : box.height;
    const origin = axis === "row" ? event.clientX : event.clientY;
    const startSizes = sizes;
    const move = (moveEvent: PointerEvent) => {
      const delta = ((axis === "row" ? moveEvent.clientX : moveEvent.clientY) - origin) / total;
      const pair = startSizes[index] + startSizes[index + 1];
      const first = Math.min(Math.max(startSizes[index] + delta, 0.08), pair - 0.08);
      const next = [...startSizes];
      next[index] = first;
      next[index + 1] = pair - first;
      setSizes(next);
      saveSizes(storageKey, next);
    };
    const stop = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  };

  return (
    <div ref={container} className={`split split-${axis}${className ? ` ${className}` : ""}`}>
      {children.map((child, index) => (
        <div key={index} className="split-cell" style={{ flexGrow: sizes[index], flexBasis: 0 }}>
          {child}
          {index < children.length - 1 && (
            <div
              className={`divider divider-${axis}`}
              role="separator"
              aria-orientation={axis === "row" ? "vertical" : "horizontal"}
              onPointerDown={startDrag(index)}
            />
          )}
        </div>
      ))}
    </div>
  );
}

function Panel({ panel }: { panel: PanelSpec }) {
  return (
    <section className="panel" data-panel={panel.id} aria-labelledby={`panel-${panel.id}`}>
      <header className="panel-header">
        <h2 id={`panel-${panel.id}`}>{panel.title}</h2>
        {panel.actions}
      </header>
      <div className="panel-body">{panel.body}</div>
    </section>
  );
}

export function Columns({ columns, initial }: { columns: ColumnSpec[]; initial?: number[] }) {
  return (
    <Split storageKey="columns" axis="row" initial={initial}>
      {columns.map((column) =>
        column.mode === "span" ? (
          <Panel key={column.id} panel={column.panel} />
        ) : (
          <Split key={column.id} storageKey={`stack.${column.id}`} axis="column" className={`rail rail-${column.id}`}>
            {column.panels.map((panel) => (
              <Panel key={panel.id} panel={panel} />
            ))}
          </Split>
        ),
      )}
    </Split>
  );
}
