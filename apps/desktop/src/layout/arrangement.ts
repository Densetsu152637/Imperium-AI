// Which panels sit in which columns, in order: the arrangement the panel editor changes. Kept in
// the local `layout` document (state.ts) beside the sizes and closed panels; `null` there means
// the default arrangement. Pure, so the rules are tested without a window
// (test/arrangement.test.ts); App builds it and Columns draws it.
//
// The arrangement lists panel ids, including panels that are closed, detached, or from a mod that
// has not loaded yet, so each comes back to its place. Panels the saved arrangement does not know
// (new built-ins, newly enabled mods) join their default column.

/** A column and its panels' ids, top to bottom. */
export type LayoutColumn = { id: string; panels: string[] };

/** The columns left to right. */
export type Arrangement = LayoutColumn[];

/**
 * Where a dragged panel lands: in a column before a panel (or at its end), in a new column, inside
 * another panel as a module, or out of the window into one of its own. The last two are not
 * column placements: placement.ts handles them, and `applyDrop` leaves the arrangement alone.
 */
export type DropTarget =
  | { kind: "panel"; column: string; before: string | null }
  | { kind: "new-column"; at: number }
  | { kind: "module"; host: string }
  | { kind: "window" };

const copy = (arrangement: Arrangement): Arrangement => arrangement.map((column) => ({ id: column.id, panels: [...column.panels] }));

/**
 * The saved arrangement brought up to date with the panels the app has: panels it does not list
 * join the column of the same id in `defaults`, after their default predecessor when that is in
 * the same column, else at the column's end; failing that, the column that holds their default
 * predecessor, or the last column. No saved arrangement means the defaults.
 */
export function reconcile(saved: Arrangement | null | undefined, defaults: Arrangement): Arrangement {
  if (!saved || saved.length === 0) return copy(defaults);
  const result = copy(saved);
  const placed = new Set(result.flatMap((column) => column.panels));
  const columnOf = (panel: string) => result.find((column) => column.panels.includes(panel));
  for (const fallback of defaults) {
    fallback.panels.forEach((panel, index) => {
      if (placed.has(panel)) return;
      const previous = index > 0 ? fallback.panels[index - 1] : null;
      const target =
        result.find((column) => column.id === fallback.id) ?? (previous ? columnOf(previous) : undefined) ?? result[result.length - 1];
      const after = previous ? target.panels.indexOf(previous) : -1;
      if (after >= 0) target.panels.splice(after + 1, 0, panel);
      else if (index === 0) target.panels.unshift(panel);
      else target.panels.push(panel);
      placed.add(panel);
    });
  }
  return result;
}

/** `arrangement` without `panel`, wherever it was; columns stay, even when left empty. */
function without(arrangement: Arrangement, panel: string): Arrangement {
  return arrangement.map((column) => ({ id: column.id, panels: column.panels.filter((id) => id !== panel) }));
}

/**
 * The arrangement after dropping `panel` on `target`. Rearranging happens only in the panel
 * editor: outside it (`editing` false) the arrangement comes back unchanged, the same object.
 * A drop on an unknown column, or on the panel itself, changes nothing either.
 */
export function applyDrop(
  arrangement: Arrangement,
  panel: string,
  target: DropTarget,
  editing: boolean,
  newColumnId: () => string,
): Arrangement {
  if (!editing || target.kind === "module" || target.kind === "window") return arrangement;
  if (target.kind === "new-column") {
    const next = without(arrangement, panel);
    const at = Math.max(0, Math.min(target.at, next.length));
    next.splice(at, 0, { id: newColumnId(), panels: [panel] });
    return next;
  }
  if (target.before === panel || !arrangement.some((column) => column.id === target.column)) return arrangement;
  return without(arrangement, panel).map((column) => {
    if (column.id !== target.column) return column;
    const index = target.before === null ? -1 : column.panels.indexOf(target.before);
    const panels = [...column.panels];
    panels.splice(index < 0 ? panels.length : index, 0, panel);
    return { id: column.id, panels };
  });
}

/** The arrangement with an empty column added at the right (editor only). */
export function addColumn(arrangement: Arrangement, editing: boolean, newColumnId: () => string): Arrangement {
  return editing ? [...copy(arrangement), { id: newColumnId(), panels: [] }] : arrangement;
}

/**
 * The arrangement without `column`, when it shows no panel (`shown` says which panels are on
 * screen) and is not the last column (editor only). Panels it still lists — closed, detached, or
 * not loaded — move to the column before it (or after it, for the first), keeping their place
 * for when they come back.
 */
export function removeColumn(arrangement: Arrangement, column: string, shown: (panel: string) => boolean, editing: boolean): Arrangement {
  const index = arrangement.findIndex((c) => c.id === column);
  if (!editing || index < 0 || arrangement.length <= 1 || arrangement[index].panels.some(shown)) return arrangement;
  const next = copy(arrangement);
  const [removed] = next.splice(index, 1);
  const neighbour = next[Math.max(0, index - 1)];
  if (index === 0) neighbour.panels.unshift(...removed.panels);
  else neighbour.panels.push(...removed.panels);
  return next;
}

/**
 * The columns to draw, with each panel id looked up (`lookup` returns undefined for one that is
 * not shown). Outside the editor, columns with nothing to show are left out; in it they stay, as
 * drop targets the user can remove.
 */
export function visibleColumns<T>(arrangement: Arrangement, lookup: (panel: string) => T | undefined, editing: boolean): { id: string; panels: T[] }[] {
  return arrangement
    .map((column) => ({ id: column.id, panels: column.panels.map(lookup).filter((panel): panel is T => panel !== undefined) }))
    .filter((column) => editing || column.panels.length > 0);
}

/** Whether two arrangements list the same columns and panels in the same order. */
export const sameArrangement = (a: Arrangement, b: Arrangement): boolean => JSON.stringify(a) === JSON.stringify(b);
