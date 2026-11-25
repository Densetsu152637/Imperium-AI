// Which columns the window can show. Every column has a minimum width; when the columns do not all
// fit at their minimums, the rightmost side columns fold away (the title bar's "More panels"
// switcher opens them), and below 600 px only one column shows at a time: the session, with the
// sidebar and the side panels opening as sheets over it. Pure, so the rule is tested without a
// window (test/fold.test.ts); App measures the window, Columns draws what it returns, and the
// saved column proportions are never touched (a folded column keeps its share for when the window
// widens again).
//
// Widths are in layout pixels: window pixels divided by the UI scale, the unit `--px` stands for,
// so a column keeps its minimum on screen at every UI scale.

/** The kinds of column that have their own minimum: left of the session, the session's, and right of it. */
export type ColumnKind = "sidebar" | "session" | "side";

/** A column's id and the kind that decides its minimum width. */
export type FoldColumn = { id: string; kind: ColumnKind };

/**
 * Minimum widths in layout pixels. styles/tokens.css repeats them as `--col-min-*` (the columns'
 * `min-width`), and test/fold.test.ts fails when the two drift apart.
 */
export const COLUMN_MIN: Readonly<Record<ColumnKind, number>> = { sidebar: 220, session: 360, side: 280 };

/** Below this width one column shows at a time and the panel editor is off. */
export const NARROW_BELOW = 600;

/** What the window shows: every column id is in exactly one of `shown` and `folded`, in column order. */
export type Fold = { narrow: boolean; shown: string[]; folded: string[] };

/** Whether a window this wide shows one column at a time. An unknown width counts as wide. */
export const isNarrow = (width: number): boolean => Number.isFinite(width) && width < NARROW_BELOW;

/**
 * The kinds of `columns` (left to right): the one holding the spanning session panel is the
 * session's, those before it are sidebars and those after it side columns. Without a session
 * (it is in a window of its own) every column is a side column.
 */
export function classifyColumns(columns: { id: string; span: boolean }[]): FoldColumn[] {
  const session = columns.findIndex((column) => column.span);
  return columns.map((column, index) => ({
    id: column.id,
    kind: session < 0 ? "side" : index === session ? "session" : index < session ? "sidebar" : "side",
  }));
}

/**
 * Which of `columns` show in a window `width` wide. The session's column (the first, without one)
 * always shows. Below 600 px it is alone. From there up, columns fold from the right until the
 * rest fit at their minimums; a width equal to the total fits.
 */
export function foldColumns(width: number, columns: FoldColumn[], minimums: Readonly<Record<ColumnKind, number>> = COLUMN_MIN): Fold {
  const ids = columns.map((column) => column.id);
  if (columns.length === 0) return { narrow: isNarrow(width), shown: [], folded: [] };
  const anchor = (columns.find((column) => column.kind === "session") ?? columns[0]).id;
  if (isNarrow(width)) return { narrow: true, shown: [anchor], folded: ids.filter((id) => id !== anchor) };
  const kept = new Set(ids);
  if (Number.isFinite(width)) {
    let needed = columns.reduce((sum, column) => sum + minimums[column.kind], 0);
    for (let index = columns.length - 1; index >= 0 && needed > width; index--) {
      const column = columns[index];
      if (column.id === anchor) continue;
      kept.delete(column.id);
      needed -= minimums[column.kind];
    }
  }
  return { narrow: false, shown: ids.filter((id) => kept.has(id)), folded: ids.filter((id) => !kept.has(id)) };
}

/**
 * The new shares of two neighbouring columns when a divider between them moves by `delta` (a share
 * of the open columns' room): the pair keeps its total, and neither goes below its own floor (its
 * minimum width as a share, never less than `floor`). A column the window already holds at its
 * minimum starts from there, so a drag past it moves nothing and coming back answers at once.
 */
export function dragShares(start: [number, number], delta: number, minimums: [number, number], floor = 0.08): [number, number] {
  const both = start[0] + start[1];
  let low = Math.max(minimums[0], floor);
  let high = both - Math.max(minimums[1], floor);
  if (low > high) low = high = both * (low / (low + Math.max(minimums[1], floor)));
  const first = Math.min(Math.max(Math.min(Math.max(start[0], low), high) + delta, low), high);
  return [first, both - first];
}

/** The window's width given the one just read: 0 (minimised or hidden) is no reading, so the last known width stays. */
export const knownWidth = (read: number, previous: number): number => (read > 0 ? read : previous);
