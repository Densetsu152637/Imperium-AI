// The keyboard alternatives to dragging: what a panel's ⋯ menu offers under *Move…*, and what
// choosing one does. Every drag in the panel editor has one: up or down within the column, another
// column, a new column at either edge, into another panel as a module (for module panels) and back
// out of a host by moving to a column, and into a window of its own (the menu's *Open in its own
// window*). Pure (tested in test/placement.test.ts); the moves reuse the drop rules (placement.ts), so a menu
// choice and a drag always land the same way.

import type { DropTarget } from "./arrangement";
import { canHost, dropPanel, hostOf, type Dropped, type Modules, type Placement } from "./placement.ts";

export type PanelMove = { label: string; target: DropTarget };

const hostedAnywhere = (modules: Modules, id: string): boolean => hostOf(modules, id) !== null;

/**
 * The moves open to `panel`, in menu order. `shown` says which panels are on screen (closed and
 * detached ones are skipped, as nobody can see them), `title` names panels for the labels.
 * A panel hosted in another panel has no column position, so it gets no up or down.
 */
export function panelMoves(options: {
  placement: Placement;
  panel: string;
  shown: (id: string) => boolean;
  title: (id: string) => string;
  isModule: (id: string) => boolean;
}): PanelMove[] {
  const { placement, panel, shown, title } = options;
  const hosted = hostedAnywhere(placement.modules, panel);
  const moves: PanelMove[] = [];
  const drawn = (id: string) => shown(id) && !hostedAnywhere(placement.modules, id);

  const home = hosted ? undefined : placement.arrangement.find((column) => column.panels.includes(panel));
  if (home) {
    const visible = home.panels.filter(drawn);
    const index = visible.indexOf(panel);
    if (index > 0) moves.push({ label: "Move up", target: { kind: "panel", column: home.id, before: visible[index - 1] } });
    if (index >= 0 && index < visible.length - 1) {
      moves.push({ label: "Move down", target: { kind: "panel", column: home.id, before: visible[index + 2] ?? null } });
    }
  }

  // Only columns with something on screen count, so the numbers match what the user sees.
  const columns = placement.arrangement.filter((column) => column.panels.some((id) => id !== panel && drawn(id)));
  columns.forEach((column, index) => {
    if (column.id === home?.id) return;
    moves.push({ label: `Move to column ${index + 1}`, target: { kind: "panel", column: column.id, before: null } });
  });
  // A new column only helps a panel that has company where it is (or sits in a host).
  const company = home ? home.panels.some((id) => id !== panel && drawn(id)) : true;
  if (company) {
    moves.push({ label: "Move to a new column at the left", target: { kind: "new-column", at: 0 } });
    moves.push({ label: "Move to a new column at the right", target: { kind: "new-column", at: placement.arrangement.length } });
  }

  const current = hostOf(placement.modules, panel);
  for (const id of placement.arrangement.flatMap((column) => column.panels)) {
    if (id !== current && drawn(id) && canHost(id, panel, options.isModule)) {
      moves.push({ label: `Move into ${title(id)}`, target: { kind: "module", host: id } });
    }
  }
  return moves;
}

/** The placement after `move` (the same drop a drag would make). */
export function applyMove(
  placement: Placement,
  panel: string,
  move: PanelMove,
  newColumnId: () => string,
  isModule: (id: string) => boolean,
): Dropped {
  return dropPanel(placement, panel, move.target, true, newColumnId, isModule);
}
