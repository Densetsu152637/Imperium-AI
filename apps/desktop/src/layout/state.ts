// The workspace as the user left it: which panels sit in which columns, panel sizes, which panels are closed, and the selected
// environment and session. Kept in the local `layout` document (store.ts), so
// the window reopens the same way after a restart, an update, or a reinstall.

import { getDoc, setDoc, subscribeDoc } from "../store";
import type { Arrangement } from "./arrangement";

export type Layout = {
  /** Proportions per split, by key ("columns", "stack.left", ...). */
  sizes: Record<string, number[]>;
  /** Which panels sit in which columns, as the panel editor left them; null for the default. */
  arrangement: Arrangement | null;
  /** Panels the user closed. */
  hidden: string[];
  /** Stacked panels folded to their header; the others in the column take their room. */
  collapsed: string[];
  /** The selected environment's id: new sessions start there. */
  environment: string | null;
  session: string | null;
  /** Whether the Sessions list shows only the selected environment's sessions. */
  projectView: boolean;
  /** How the Sessions list is ordered: most recent first, or grouped by environment or status. */
  sessionSort: "recent" | "environment" | "status";
  /** Before environments: the selected project folder, read once to pick its environment. */
  project: string | null;
  /** Before environments: folders opened here, each moved into an environment of its own. */
  folders: string[];
  /** Set once the folders above have become environments. */
  environmentsMigrated: boolean;
  /** Panels in their own windows, by panel id. */
  detached: Record<string, DetachedPanel>;
  /** Command palette commands by id, most recently run first. */
  recentCommands: string[];
};

export type DetachedPanel = {
  /** Whether the window stays above other windows. */
  pinned: boolean;
  /** Where the window was last left, in logical pixels. */
  bounds?: { x: number; y: number; width: number; height: number };
};

const EMPTY: Layout = {
  sizes: {},
  arrangement: null,
  hidden: [],
  collapsed: [],
  environment: null,
  session: null,
  projectView: false,
  sessionSort: "recent",
  project: null,
  folders: [],
  environmentsMigrated: false,
  detached: {},
  recentCommands: [],
};

/** The built-in Pet panel is gone (the Nursery mod's panel replaced it); saved layouts forget it. */
const REMOVED_PANELS = ["pet"];
const LEGACY_PREFIX = "imperium.layout.";

let layout: Layout | null = null;

export function getLayout(): Layout {
  if (!layout) {
    const saved = getDoc<Partial<Layout>>("layout");
    layout = { ...EMPTY, ...saved };
    if (layout.hidden.some((id) => REMOVED_PANELS.includes(id)) || REMOVED_PANELS.some((id) => id in layout!.detached)) {
      layout.hidden = layout.hidden.filter((id) => !REMOVED_PANELS.includes(id));
      layout.detached = Object.fromEntries(Object.entries(layout.detached).filter(([id]) => !REMOVED_PANELS.includes(id)));
      setDoc("layout", layout);
    }
    if (!saved) {
      // Split sizes used to be kept per key in local storage; bring them over once.
      try {
        for (const key of Object.keys(localStorage)) {
          if (!key.startsWith(LEGACY_PREFIX)) continue;
          const sizes = JSON.parse(localStorage.getItem(key) ?? "null") as number[] | null;
          if (Array.isArray(sizes)) layout.sizes[key.slice(LEGACY_PREFIX.length)] = sizes;
          localStorage.removeItem(key);
        }
      } catch {
        // Nothing to bring over.
      }
      setDoc("layout", layout);
    }
  }
  return layout;
}

// Only the main window writes the layout; the others read the copy it broadcasts.
subscribeDoc("layout", () => {
  layout = { ...EMPTY, ...getDoc<Partial<Layout>>("layout") };
});

export function updateLayout(patch: Partial<Layout>): void {
  layout = { ...getLayout(), ...patch };
  setDoc("layout", layout);
}

/** Forgets the arrangement, sizes, closed and collapsed panels, and detached windows (back to the defaults); the selection and folders stay. */
export function resetLayout(): void {
  updateLayout({ sizes: {}, arrangement: null, hidden: [], collapsed: [], detached: {} });
}
