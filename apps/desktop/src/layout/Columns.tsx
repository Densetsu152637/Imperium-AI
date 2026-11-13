// The window is a row of columns. A column holds either one spanning panel (the session) or a
// stack of panels; columns and stack dividers are resizable and sizes are kept as proportions in
// the saved layout (state.ts). Which panels sit in which column is the arrangement
// (arrangement.ts): it changes only in the panel editor, where panels are dragged by their
// headers to another place, another column, or a new column at a column's edge, and empty
// columns can be removed. Outside the editor panels cannot be dragged, but every drag has a keyboard
// alternative in the panel's ⋯ menu (*Move…*, moves.ts). Dragging a mod's panel onto the middle of
// another panel shows it there as a module (placement.ts); letting a panel go beyond the window's
// edge or on the editor's window zone opens it in a window of its own, and the chips of panels
// already in windows can be dragged back into a column. Panels with `onClose` can be closed; Settings → Panels reopens them.
// Panels with `onDetach` can move to a window of their own (panelWindows.ts).
// A stacked panel folds to its header with the chevron before its title (or a double-click on
// the header); the others in its column take its room. The first header of a stack also offers
// collapse all / expand all for the column. Folded panels are kept in the layout too.
// A header shows the chevron, the title and the panel's primary action; the panel's secondary
// actions, collapse all, detach and close sit in one ⋯ menu (panelMenu.ts) that stays quiet until
// the header is hovered or focused.

import { ChevronDown, ChevronsDownUp, ChevronsUpDown, ExternalLink, GripVertical, MoveRight, X } from "lucide-react";
import { useState, useRef, type MouseEvent, type ReactNode, type PointerEvent as ReactPointerEvent } from "react";

import { RowMenu, type RowMenuItem } from "../components/RowMenu";
import type { DropTarget } from "./arrangement";
import { panelMenuEntries } from "./panelMenu";
import { getLayout, updateLayout } from "./state";

export type PanelSpec = {
  id: string;
  title: string;
  body: ReactNode;
  /** The panel's primary action (at most one button, or a control and its button), always shown. */
  actions?: ReactNode;
  /** The panel's secondary actions, listed first in the header's ⋯ menu. */
  menu?: RowMenuItem[];
  onClose?: () => void;
  onDetach?: () => void;
  /** The keyboard alternatives to dragging the panel (layout/moves.ts), listed as *Move…* in its ⋯ menu. */
  moves?: RowMenuItem[];
  /** The canvas (the session): spans its column when alone there, and never folds. */
  span?: boolean;
  /** A mod's panel: it can be dragged into another panel and shown there as a module. */
  module?: boolean;
  /** The panel can host modules: dropping a module on its middle shows it as a section under the panel's own content. */
  host?: boolean;
  /** The modules shown inside this panel, below its own content. */
  hosted?: PanelSpec[];
};
/** A column and the panels shown in it, top to bottom; empty only in the editor. */
export type ColumnSpec = { id: string; panels: PanelSpec[] };

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

/** In the panel editor: the header is the panel's handle, and where a dragged one would land. */
type Edit = {
  onGrab: (event: ReactPointerEvent<HTMLElement>) => void;
  /** A dragged panel would land just above this one. */
  dropBefore: boolean;
  /** This panel is the one being dragged. */
  dragging: boolean;
  /** A dragged module would land inside this panel. */
  dropInto: boolean;
  /** Starts dragging one of the modules shown inside this panel, by its chip in the editor. */
  onGrabModule: (module: PanelSpec) => (event: ReactPointerEvent<HTMLElement>) => void;
};

/** The header's ⋯ menu: the panel's own secondary actions, then collapse all, detach and close. */
function PanelMenu({ panel, fold, collapsed }: { panel: PanelSpec; fold?: Fold; collapsed: boolean }) {
  const own = collapsed ? [] : (panel.menu ?? []);
  const column = fold?.column;
  const items: RowMenuItem[] = panelMenuEntries({
    own: own.length,
    column,
    moves: panel.moves?.length ?? 0,
    canDetach: !!panel.onDetach,
    canClose: !!panel.onClose,
  }).map((entry) => {
    switch (entry.kind) {
      case "panel":
        return own[entry.index];
      case "move":
        return { ...(panel.moves as RowMenuItem[])[entry.index], icon: MoveRight };
      case "fold-all":
        return { label: entry.label, icon: column?.allCollapsed ? ChevronsUpDown : ChevronsDownUp, onSelect: () => column?.onToggleAll() };
      case "detach":
        return { label: entry.label, icon: ExternalLink, onSelect: () => panel.onDetach?.() };
      case "close":
        return { label: entry.label, icon: X, onSelect: () => panel.onClose?.() };
    }
  });
  if (items.length === 0) return null;
  return <RowMenu label={`${panel.title} options`} items={items} className="panel-menu-button" />;
}

function Panel({ panel, fold, edit }: { panel: PanelSpec; fold?: Fold; edit?: Edit }) {
  const bodyId = `panel-body-${panel.id}`;
  const collapsed = fold?.collapsed ?? false;
  // A double-click on the header's empty space or title folds the panel, as in many docks.
  const onDoubleClick = (event: MouseEvent<HTMLElement>) => {
    if (edit || !fold || (event.target as HTMLElement).closest("button, input, select, textarea, a, [role='combobox']")) return;
    window.getSelection()?.removeAllRanges();
    fold.onToggle();
  };
  const classes = ["panel", collapsed && "collapsed", edit?.dropBefore && "drop-before", edit?.dropInto && "drop-into", edit?.dragging && "drag-source"];
  const hosted = panel.hosted ?? [];
  return (
    <section
      className={classes.filter(Boolean).join(" ")}
      data-panel={panel.id}
      data-host={panel.host ? "" : undefined}
      aria-labelledby={`panel-${panel.id}`}
    >
      <header className="panel-header" onDoubleClick={onDoubleClick} onPointerDown={edit?.onGrab}>
        {edit && <GripVertical className="panel-grip" aria-hidden="true" />}
        {!edit && fold && (
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
        {!edit && !collapsed && panel.actions}
        {!edit && <PanelMenu panel={panel} fold={fold} collapsed={collapsed} />}
      </header>
      {/* In the editor the modules inside the panel are listed here, outside the inert body, so each can be dragged out. */}
      {edit && hosted.length > 0 && (
        <ul className="panel-module-chips" aria-label={`Modules in ${panel.title}`}>
          {hosted.map((module) => (
            <li key={module.id} className="panel-module-chip" onPointerDown={edit.onGrabModule(module)}>
              <GripVertical className="panel-grip" aria-hidden="true" /> {module.title}
            </li>
          ))}
        </ul>
      )}
      {/* Folded bodies stay mounted (mod frames keep their state), just out of view and reach.
          In the editor they are inert, so a drag over a mod's frame or a text field still lands. */}
      <div className="panel-body" id={bodyId} hidden={collapsed} inert={edit ? true : undefined}>
        {panel.body}
        {hosted.map((module) => (
          <section key={module.id} className="panel-module" data-module={module.id} aria-labelledby={`panel-${module.id}`}>
            <header className="panel-module-header">
              <h3 id={`panel-${module.id}`}>{module.title}</h3>
              <PanelMenu panel={module} collapsed={false} />
            </header>
            <div className="panel-module-body">{module.body}</div>
          </section>
        ))}
      </div>
    </section>
  );
}

/** A dragged panel, while the pointer moves: the ghost follows it, the target lights up. */
type Drag = { panel: string; title: string; x: number; y: number; target: DropTarget | null };

/** Pointer travel before a press on a header becomes a drag, in pixels. */
const DRAG_THRESHOLD = 4;

/**
 * What lies under the pointer: outside the window or on the editor's window zone (a window of its
 * own), the middle of a panel that hosts modules when a module is dragged (into it), a column
 * edge (a new column), or a place in a column.
 */
function dropTargetAt(x: number, y: number, dragged: { id: string; module: boolean; detachable: boolean }): DropTarget | null {
  if (dragged.detachable && (x < 0 || y < 0 || x > window.innerWidth || y > window.innerHeight)) return { kind: "window" };
  const under = document.elementFromPoint(x, y);
  if (dragged.detachable && under?.closest("[data-drop-window]")) return { kind: "window" };
  if (dragged.module) {
    const host = under?.closest<HTMLElement>("[data-host]");
    if (host && host.dataset.panel !== dragged.id) {
      const box = host.getBoundingClientRect();
      const along = box.height > 0 ? (y - box.top) / box.height : 0;
      if (along > 0.25 && along < 0.75) return { kind: "module", host: host.dataset.panel ?? "" };
    }
  }
  const element = under?.closest<HTMLElement>("[data-drop-edge], [data-column]");
  if (!element) return null;
  if (element.dataset.dropEdge !== undefined) return { kind: "new-column", at: Number(element.dataset.dropEdge) };
  const column = element.dataset.column ?? "";
  // The first panel whose upper half is below the pointer is the one to land before.
  const panels = [...element.querySelectorAll<HTMLElement>("[data-panel]")].filter((panel) => panel.closest("[data-column]") === element);
  const before = panels.find((panel) => {
    const box = panel.getBoundingClientRect();
    return y < box.top + box.height / 2;
  });
  return { kind: "panel", column, before: before?.dataset.panel ?? null };
}

export function Columns({
  columns,
  initial,
  editing = false,
  windows = [],
  onDrop,
  onDock,
  onRemoveColumn,
}: {
  columns: ColumnSpec[];
  initial?: number[];
  /** The panel editor: panels are dragged by their headers between and into columns. */
  editing?: boolean;
  /** Panels in windows of their own, shown in the editor as chips to drag back into a column (or press to dock). */
  windows?: { id: string; title: string }[];
  /** A panel dropped in the editor. */
  onDrop?: (panel: string, target: DropTarget) => void;
  /** A panel's window chip pressed: dock it where it was. */
  onDock?: (panel: string) => void;
  /** The editor's *Remove column* on an empty column. */
  onRemoveColumn?: (column: string) => void;
}) {
  const [collapsed, setCollapsed] = useState<string[]>(() => getLayout().collapsed ?? []);
  const [drag, setDrag] = useState<Drag | null>(null);
  const save = (next: string[]) => {
    setCollapsed(next);
    updateLayout({ collapsed: next });
  };
  const toggle = (id: string) => save(collapsed.includes(id) ? collapsed.filter((other) => other !== id) : [...collapsed, id]);
  const setAll = (ids: string[], fold: boolean) =>
    save(fold ? [...new Set([...collapsed, ...ids])] : collapsed.filter((id) => !ids.includes(id)));

  // Leaving the editor mid-drag drops nothing.
  if (!editing && drag) setDrag(null);

  /** Starts dragging `panel` by its header; only in the editor. Esc cancels. */
  const grab = (panel: PanelSpec, detached = false) => (event: ReactPointerEvent<HTMLElement>) => {
    if (!editing || event.button !== 0) return;
    event.stopPropagation();
    event.preventDefault();
    // Captured, the pointer keeps reporting beyond the window's edge: letting go there opens a window.
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // Without capture, the editor's window zone still works.
    }
    const origin = { x: event.clientX, y: event.clientY };
    const dragged = { id: panel.id, module: !!panel.module && !detached, detachable: !detached && !!panel.onDetach };
    let started = false;
    let target: DropTarget | null = null;
    const move = (moveEvent: PointerEvent) => {
      if (!started && Math.hypot(moveEvent.clientX - origin.x, moveEvent.clientY - origin.y) < DRAG_THRESHOLD) return;
      started = true;
      target = dropTargetAt(moveEvent.clientX, moveEvent.clientY, dragged);
      setDrag({ panel: panel.id, title: panel.title, x: moveEvent.clientX, y: moveEvent.clientY, target });
    };
    const end = (drop: boolean) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
      window.removeEventListener("keydown", onKey, true);
      setDrag(null);
      if (drop && started && target) onDrop?.(panel.id, target);
    };
    const onUp = () => end(true);
    const onCancel = () => end(false);
    const onKey = (keyEvent: KeyboardEvent) => {
      if (keyEvent.key !== "Escape") return;
      keyEvent.preventDefault();
      keyEvent.stopPropagation();
      end(false);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
    window.addEventListener("keydown", onKey, true);
  };

  const target = drag?.target ?? null;
  const editFor = (panel: PanelSpec, column: string): Edit | undefined =>
    editing
      ? {
          onGrab: grab(panel),
          dropBefore: target?.kind === "panel" && target.column === column && target.before === panel.id,
          dragging: drag?.panel === panel.id,
          dropInto: target?.kind === "module" && target.host === panel.id,
          onGrabModule: (module) => grab(module),
        }
      : undefined;

  // Columns left of the session's are left rails, the rest right rails (borders face the session).
  const sessionIndex = columns.findIndex((column) => column.panels.some((panel) => panel.span));

  // Column widths are kept per set of columns, so a new or removed column does not squeeze the
  // others into equal shares: a set seen for the first time gives the session's column the most
  // room. The default set keeps the key it always had.
  const columnIds = columns.map((column) => column.id).join(",");
  const defaultSet = columnIds === "left,main,right";
  const sizesKey = defaultSet ? "columns" : `columns.${columnIds}`;
  const weights = columns.map((column) => (column.panels.some((panel) => panel.span) ? 2.2 : 1));
  const total = weights.reduce((sum, weight) => sum + weight, 0) || 1;
  const startSizes = defaultSet && initial ? initial : weights.map((weight) => weight / total);

  return (
    <div className="columns-root">
      {editing && (
        <div className="layout-window-strip" role="group" aria-label="Windows">
          <div className={`layout-window-zone${target?.kind === "window" ? " active" : ""}`} data-drop-window>
            <ExternalLink aria-hidden="true" /> Drop a panel here, or beyond the window's edge, to open it in its own window
          </div>
          {windows.map((entry) => (
            <button
              key={entry.id}
              className="ghost small layout-window-chip"
              onPointerDown={grab({ id: entry.id, title: entry.title, body: null }, true)}
              onClick={() => onDock?.(entry.id)}
              title={`${entry.title} is in its own window: press to dock it, or drag it into a column`}
            >
              <GripVertical aria-hidden="true" /> {entry.title}
            </button>
          ))}
        </div>
      )}
      <Split key={sizesKey} storageKey={sizesKey} axis="row" initial={startSizes} className={editing ? "editing" : undefined}>
        {columns.map((column, index) => {
          const ids = column.panels.map((panel) => panel.id);
          const side = sessionIndex >= 0 && index < sessionIndex ? "left" : "right";
          const dropEnd = target?.kind === "panel" && target.column === column.id && target.before === null;
          let content: ReactNode;
          if (column.panels.length === 0) {
            content = (
              <div className={`layout-empty-column rail rail-${side}`}>
                <span className="muted">Drop a panel here</span>
                {onRemoveColumn && columns.length > 1 && (
                  <button className="ghost small" onClick={() => onRemoveColumn(column.id)}>
                    Remove column
                  </button>
                )}
              </div>
            );
          } else if (column.panels.length === 1 && column.panels[0].span) {
            content = <Panel panel={column.panels[0]} edit={editFor(column.panels[0], column.id)} />;
          } else {
            const allCollapsed = ids.length > 0 && ids.every((id) => collapsed.includes(id));
            content = (
              <Split
                key={`${column.id}:${ids.join(",")}`}
                storageKey={`stack.${column.id}.${column.panels.length}`}
                axis="column"
                className={`rail rail-${column.id} rail-${side}`}
                collapsed={ids.map((id) => collapsed.includes(id))}
              >
                {column.panels.map((panel, panelIndex) => (
                  <Panel
                    key={panel.id}
                    panel={panel}
                    edit={editFor(panel, column.id)}
                    fold={
                      panel.span
                        ? undefined
                        : {
                            collapsed: collapsed.includes(panel.id),
                            onToggle: () => toggle(panel.id),
                            column:
                              panelIndex === 0 && ids.length > 1 ? { allCollapsed, onToggleAll: () => setAll(ids, !allCollapsed) } : undefined,
                          }
                    }
                  />
                ))}
              </Split>
            );
          }
          return (
            <div key={column.id} className={`layout-column${dropEnd ? " drop-end" : ""}`} data-column={column.id}>
              {editing && (
                <div
                  className={`layout-edge left${target?.kind === "new-column" && target.at === index ? " active" : ""}`}
                  data-drop-edge={index}
                  aria-hidden="true"
                />
              )}
              {content}
              {editing && index === columns.length - 1 && (
                <div
                  className={`layout-edge right${target?.kind === "new-column" && target.at === index + 1 ? " active" : ""}`}
                  data-drop-edge={index + 1}
                  aria-hidden="true"
                />
              )}
            </div>
          );
        })}
      </Split>
      {drag && (
        <div className="layout-drag-ghost" style={{ left: drag.x, top: drag.y }} aria-hidden="true">
          <GripVertical aria-hidden="true" /> {drag.title}
        </div>
      )}
    </div>
  );
}
