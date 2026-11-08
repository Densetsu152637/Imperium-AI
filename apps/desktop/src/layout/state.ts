// The workspace as the user left it: panel sizes, which panels are closed, the selected project
// and session, and the folders they opened. Kept in the local `layout` document (store.ts), so
// the window reopens the same way after a restart, an update, or a reinstall.

import { getDoc, setDoc } from "../store";

export type Layout = {
  /** Proportions per split, by key ("columns", "stack.left", ...). */
  sizes: Record<string, number[]>;
  /** Panels the user closed. */
  hidden: string[];
  project: string | null;
  session: string | null;
  projectView: boolean;
  /** Project folders opened here that have no sessions yet. */
  folders: string[];
};

const EMPTY: Layout = { sizes: {}, hidden: [], project: null, session: null, projectView: false, folders: [] };
const LEGACY_PREFIX = "imperium.layout.";

let layout: Layout | null = null;

export function getLayout(): Layout {
  if (!layout) {
    const saved = getDoc<Partial<Layout>>("layout");
    layout = { ...EMPTY, ...saved };
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

export function updateLayout(patch: Partial<Layout>): void {
  layout = { ...getLayout(), ...patch };
  setDoc("layout", layout);
}

/** Forgets sizes and closed panels; the selection and folders stay. */
export function resetLayout(): void {
  updateLayout({ sizes: {}, hidden: [] });
}
