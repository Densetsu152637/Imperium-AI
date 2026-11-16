// Keyboard rules for lists of items that take one active item at a time: menus (WAI-ARIA menu
// button) and tabs (WAI-ARIA tabs). Pure functions of the key and the current state, so the
// patterns are tested without a DOM; the components only apply the result.

/** The next enabled index from `from` moving by `by` (1 or -1), wrapping; `from` when none else is enabled. `from` may be out of range (-1 starts at the first item, `count` at the last when moving back). */
export function stepWrap(count: number, from: number, by: 1 | -1, enabled: (index: number) => boolean): number {
  for (let i = 1; i <= count; i++) {
    const next = (((from + by * i) % count) + count) % count;
    if (enabled(next)) return next;
  }
  return from;
}

/** The first enabled index, or the last one when `last` is set; -1 when none is enabled. */
export function edge(count: number, last: boolean, enabled: (index: number) => boolean): number {
  for (let i = 0; i < count; i++) {
    const index = last ? count - 1 - i : i;
    if (enabled(index)) return index;
  }
  return -1;
}

export type MenuKey =
  | { kind: "move"; active: number }
  | { kind: "run"; active: number }
  /** Close and give focus back to the button. */
  | { kind: "close" }
  /** Close and let Tab move on. */
  | { kind: "leave" };

/** What a key does inside an open menu; null when the menu ignores it. Arrows wrap; Home and End jump; Enter or Space runs the active item; Escape closes; Tab leaves. */
export function menuKey(key: string, active: number, count: number, enabled: (index: number) => boolean): MenuKey | null {
  switch (key) {
    case "ArrowDown":
      return { kind: "move", active: stepWrap(count, active, 1, enabled) };
    case "ArrowUp":
      return { kind: "move", active: stepWrap(count, active, -1, enabled) };
    case "Home":
      return { kind: "move", active: edge(count, false, enabled) };
    case "End":
      return { kind: "move", active: edge(count, true, enabled) };
    case "Enter":
    case " ":
      return { kind: "run", active };
    case "Escape":
      return { kind: "close" };
    case "Tab":
      return { kind: "leave" };
    default:
      return null;
  }
}

/** The tab to move to for a key on tab `current`, or null when the key does nothing. Left and Right (Up and Down when vertical) wrap; Home and End jump; disabled tabs are skipped. */
export function tabKey(key: string, current: number, count: number, enabled: (index: number) => boolean, vertical = false): number | null {
  switch (key) {
    case vertical ? "ArrowDown" : "ArrowRight":
      return stepWrap(count, current, 1, enabled);
    case vertical ? "ArrowUp" : "ArrowLeft":
      return stepWrap(count, current, -1, enabled);
    case "Home":
      return edge(count, false, enabled);
    case "End":
      return edge(count, true, enabled);
    default:
      return null;
  }
}
