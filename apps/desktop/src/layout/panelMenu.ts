// What a docked panel's header menu (the ⋯ beside its title) offers. The header keeps the fold
// chevron, the title and the panel's primary action on screen; everything else about the panel
// (its own secondary actions, folding the whole column, detaching, closing) is in this one menu.

export type PanelMenuEntry =
  | { kind: "panel"; index: number }
  | { kind: "fold-all"; label: string }
  | { kind: "move"; index: number }
  | { kind: "detach"; label: string }
  | { kind: "close"; label: string };

/**
 * The menu's entries in order: the panel's own items first, then the column's fold-all toggle
 * (on the first panel of a stack), the moves, detaching, and closing last.
 */
export function panelMenuEntries(options: {
  /** How many items the panel itself adds. */
  own: number;
  /** On the first panel of a stack: whether every panel in the column is folded. */
  column?: { allCollapsed: boolean };
  /** How many *Move…* entries the panel has (layout/moves.ts): the keyboard alternatives to dragging it. */
  moves?: number;
  canDetach: boolean;
  canClose: boolean;
}): PanelMenuEntry[] {
  const entries: PanelMenuEntry[] = Array.from({ length: options.own }, (_, index) => ({ kind: "panel", index }));
  if (options.column) {
    entries.push({ kind: "fold-all", label: options.column.allCollapsed ? "Expand all in this column" : "Collapse all in this column" });
  }
  for (let index = 0; index < (options.moves ?? 0); index++) entries.push({ kind: "move", index });
  if (options.canDetach) entries.push({ kind: "detach", label: "Open in its own window" });
  if (options.canClose) entries.push({ kind: "close", label: "Close panel" });
  return entries;
}
