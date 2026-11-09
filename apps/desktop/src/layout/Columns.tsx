// The window is a row of columns. A column holds either one spanning panel or a stack of
// panels; columns and stack dividers are resizable and sizes are kept as proportions in the
// saved layout (state.ts). Panels with `onClose` can be closed; Settings → Panels reopens them.
// Panels with `onDetach` can move to a window of their own (panelWindows.ts).
// A stacked panel folds to its header with the chevron before its title (or a double-click on
// the header); the others in its column take its room. The first header of a stack also offers
// collapse all / expand all for the column. Folded panels are kept in the layout too.

import { ChevronDown, ChevronsDownUp, ChevronsUpDown, ExternalLink, X } from "lucide-react";
import { useState, useRef, type MouseEvent, type ReactNode, type PointerEvent as ReactPointerEvent } from "react";

import { getLayout, updateLayout } from "./state";

export type PanelSpec = { id: string; title: string; body: ReactNode; actions?: ReactNode; onClose?: () => void; onDetach?: () => void };
export type ColumnSpec =
  | { id: string; mode: "span"; panel: PanelSpec }
  | { id: string; mode: "stack"; panels: PanelSpec[] };

function loadSizes(key: string, count: number): number[] {
  const saved = getLayout().sizes[key];
  return saved?.length === count ? saved : Array(count).fill(1 / count);
}

function saveSizes(key: string, sizes: number[]): void {
  updateLayout({ sizes: { ...getLayout().sizes, [key]: sizes } });
}

/** Splits space between children along one axis, with draggable dividers between them. */
function Split({
  storageKey,
  axis,
  initial,
  className,
  collapsed,
  children,
}: {
  storageKey: string;
  axis: "row" | "column";
  initial?: number[];
  /** Extra classes, so a column can be styled as a whole (the side rails). */
  className?: string;
  /** Children folded to their header (stacks only): they keep their header's height. */
  collapsed?: boolean[];
  children: ReactNode[];
}) {
  const [sizes, setSizes] = useState(() =>
    initial && !getLayout().sizes[storageKey] ? initial : loadSizes(storageKey, children.length),
  );
  // Panels added or removed after the first render (mods load later) start from equal (or
  // remembered) sizes for the new count, so no panel is left without room.
  if (sizes.length !== children.length) {
    setSizes(loadSizes(storageKey, children.length));
  }
  const container = useRef<HTMLDivElement>(null);
  const folded = (index: number) => collapsed?.[index] ?? false;

  /** The open cells a divider below cell `index` moves between: the nearest above and below. */
  const neighbours = (index: number): [number, number] | null => {
    let before = index;
    while (before >= 0 && folded(before)) before--;
    let after = index + 1;
    while (after < children.length && folded(after)) after++;
    return before >= 0 && after < children.length ? [before, after] : null;
  };

  const startDrag = (pair: [number, number]) => (event: ReactPointerEvent) => {
    const box = container.current?.getBoundingClientRect();
    if (!box) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const [a, b] = pair;
    // Only the open cells share the room, so a delta is measured against their share of it.
    const open = sizes.reduce((sum, size, index) => (folded(index) ? sum : sum + size), 0);
    const total = (axis === "row" ? box.width : box.height) / (open || 1);
    const origin = axis === "row" ? event.clientX : event.clientY;
    const startSizes = sizes;
    // Folding animates the cells; a drag follows the pointer directly.
    container.current?.classList.add("dragging");
    const move = (moveEvent: PointerEvent) => {
      const delta = ((axis === "row" ? moveEvent.clientX : moveEvent.clientY) - origin) / total;
      const both = startSizes[a] + startSizes[b];
      const first = Math.min(Math.max(startSizes[a] + delta, 0.08), both - 0.08);
      const next = [...startSizes];
      next[a] = first;
      next[b] = both - first;
      setSizes(next);
      saveSizes(storageKey, next);
    };
    const stop = () => {
      container.current?.classList.remove("dragging");
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", stop);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", stop);
  };

  // Grow factors summing to less than one leave room unused, so the open cells' shares are
  // scaled up to fill what folded ones give back.
  const openShare = sizes.reduce((sum, size, index) => (folded(index) ? sum : sum + size), 0) || 1;

  return (
    <div ref={container} className={`split split-${axis}${className ? ` ${className}` : ""}`}>
      {children.map((child, index) => {
        const pair = index < children.length - 1 ? neighbours(index) : null;
        return (
          <div
            key={index}
            className={`split-cell${folded(index) ? " collapsed" : ""}`}
            style={folded(index) ? undefined : { flexGrow: sizes[index] / openShare, flexBasis: 0 }}
          >
            {child}
            {pair && (
              <div
                className={`divider divider-${axis}`}
                role="separator"
                aria-orientation={axis === "row" ? "vertical" : "horizontal"}
                onPointerDown={startDrag(pair)}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}

type Fold = {
  collapsed: boolean;
  onToggle: () => void;
  /** On the first panel of a stack: folds or opens every panel in the column. */
  column?: { allCollapsed: boolean; onToggleAll: () => void };
};

function Panel({ panel, fold }: { panel: PanelSpec; fold?: Fold }) {
  const bodyId = `panel-body-${panel.id}`;
  const collapsed = fold?.collapsed ?? false;
  // A double-click on the header's empty space or title folds the panel, as in many docks.
  const onDoubleClick = (event: MouseEvent<HTMLElement>) => {
    if (!fold || (event.target as HTMLElement).closest("button, input, select, textarea, a, [role='combobox']")) return;
    window.getSelection()?.removeAllRanges();
    fold.onToggle();
  };
  return (
    <section
      className={`panel${collapsed ? " collapsed" : ""}`}
      data-panel={panel.id}
      aria-labelledby={`panel-${panel.id}`}
    >
      <header className="panel-header" onDoubleClick={onDoubleClick}>
        {fold && (
          <button
            className="ghost panel-fold icon-only"
            onClick={fold.onToggle}
            aria-expanded={!collapsed}
            aria-controls={bodyId}
            aria-label={`${collapsed ? "Expand" : "Collapse"} ${panel.title}`}
            title={collapsed ? "Expand panel" : "Collapse panel (or double-click the header)"}
          >
            <ChevronDown aria-hidden="true" />
          </button>
        )}
        <h2 id={`panel-${panel.id}`}>{panel.title}</h2>
        {!collapsed && panel.actions}
        {fold?.column && (
          <button
            className="ghost panel-close panel-fold-all icon-only"
            onClick={fold.column.onToggleAll}
            aria-label={fold.column.allCollapsed ? "Expand all panels in this column" : "Collapse all panels in this column"}
            title={fold.column.allCollapsed ? "Expand all" : "Collapse all"}
          >
            {fold.column.allCollapsed ? <ChevronsUpDown aria-hidden="true" /> : <ChevronsDownUp aria-hidden="true" />}
          </button>
        )}
        {panel.onDetach && (
          <button
            className="ghost panel-close panel-detach icon-only"
            onClick={panel.onDetach}
            aria-label={`Detach ${panel.title}`}
            title="Open in its own window"
          >
            <ExternalLink />
          </button>
        )}
        {panel.onClose && (
          <button className="ghost panel-close icon-only" onClick={panel.onClose} aria-label={`Close ${panel.title}`} title="Close panel">
            <X />
          </button>
        )}
      </header>
      {/* Folded bodies stay mounted (mod frames keep their state), just out of view and reach. */}
      <div className="panel-body" id={bodyId} hidden={collapsed}>
        {panel.body}
      </div>
    </section>
  );
}

export function Columns({ columns, initial }: { columns: ColumnSpec[]; initial?: number[] }) {
  const [collapsed, setCollapsed] = useState<string[]>(() => getLayout().collapsed ?? []);
  const save = (next: string[]) => {
    setCollapsed(next);
    updateLayout({ collapsed: next });
  };
  const toggle = (id: string) => save(collapsed.includes(id) ? collapsed.filter((other) => other !== id) : [...collapsed, id]);
  const setAll = (ids: string[], fold: boolean) =>
    save(fold ? [...new Set([...collapsed, ...ids])] : collapsed.filter((id) => !ids.includes(id)));

  return (
    <Split storageKey="columns" axis="row" initial={initial}>
      {columns.map((column) => {
        if (column.mode === "span") return <Panel key={column.id} panel={column.panel} />;
        const ids = column.panels.map((panel) => panel.id);
        const allCollapsed = ids.length > 0 && ids.every((id) => collapsed.includes(id));
        return (
          <Split
            key={`${column.id}:${ids.join(",")}`}
            storageKey={`stack.${column.id}.${column.panels.length}`}
            axis="column"
            className={`rail rail-${column.id}`}
            collapsed={ids.map((id) => collapsed.includes(id))}
          >
            {column.panels.map((panel, index) => (
              <Panel
                key={panel.id}
                panel={panel}
                fold={{
                  collapsed: collapsed.includes(panel.id),
                  onToggle: () => toggle(panel.id),
                  column: index === 0 && ids.length > 1 ? { allCollapsed, onToggleAll: () => setAll(ids, !allCollapsed) } : undefined,
                }}
              />
            ))}
          </Split>
        );
      })}
    </Split>
  );
}
