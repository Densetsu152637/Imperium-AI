// The declarative shortcut registry: every rebindable keyboard shortcut, its default, and the
// pure logic around it (parsing, matching, conflict detection, sanitising what is saved). Kept
// free of React so node's test runner can import it. A chord is written "Mod+Shift+K": Mod is
// Ctrl, or Cmd on macOS; the last part is the key. Settings keep only the bindings the user
// changed, so a later version's changed default reaches everyone who has not rebound that action.

export type ShortcutId = "palette.toggle" | "settings.toggle" | "session.stop";

export type ShortcutAction = {
  id: ShortcutId;
  label: string;
  description: string;
  /** The binding until the user changes it. */
  default: string;
};

export const SHORTCUT_ACTIONS: readonly ShortcutAction[] = [
  { id: "palette.toggle", label: "Command palette", description: "Open or close the command palette, in the main window and panel windows.", default: "Mod+K" },
  { id: "settings.toggle", label: "Settings", description: "Open or close Settings.", default: "Mod+," },
  { id: "session.stop", label: "Stop or resume session", description: "Stop the running session, or resume a stopped one.", default: "Mod+." },
];

/** What the user changed, by action; an action without an entry uses its default. */
export type ShortcutOverrides = Partial<Record<ShortcutId, string>>;

/**
 * Chords that mean something fixed and cannot be given to an action: sending a message. The
 * push-to-talk key is reserved too, but it is a setting, so callers pass it in.
 */
export const RESERVED_CHORDS: readonly string[] = ["Mod+Enter", "Mod+Shift+Enter"];

export type Chord = { mod: boolean; alt: boolean; shift: boolean; key: string };

/** The parts of a keyboard event the registry reads. */
export type KeyLike = { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean };

const MODIFIER_KEYS = new Set(["Control", "Meta", "Alt", "Shift", "AltGraph", "OS"]);

const normalKey = (key: string): string => (key === " " ? "Space" : key.length === 1 ? key.toLowerCase() : key);

/** Parses "Mod+Shift+K"; null when it is malformed (no key, a repeated or unknown modifier). */
export function parseChord(text: string): Chord | null {
  if (typeof text !== "string" || text.length === 0) return null;
  // A trailing "+" is the plus key itself ("Mod++").
  const parts = text.endsWith("++") ? [...text.slice(0, -2).split("+"), "+"] : text.split("+");
  const key = parts.pop();
  if (!key) return null;
  const chord: Chord = { mod: false, alt: false, shift: false, key: normalKey(key) };
  for (const part of parts) {
    const lower = part.toLowerCase();
    const flag = lower === "mod" ? "mod" : lower === "alt" ? "alt" : lower === "shift" ? "shift" : null;
    if (!flag || chord[flag]) return null;
    chord[flag] = true;
  }
  return MODIFIER_KEYS.has(key) ? null : chord;
}

/** The canonical text of a chord: modifiers in Mod, Alt, Shift order, letters upper-case. */
export function chordText(chord: Chord): string {
  const key = chord.key.length === 1 ? chord.key.toUpperCase() : chord.key;
  return [chord.mod && "Mod", chord.alt && "Alt", chord.shift && "Shift", key].filter(Boolean).join("+");
}

/** A chord in canonical form, or null if it does not parse. */
export function normalizeChord(text: string): string | null {
  const chord = parseChord(text);
  return chord ? chordText(chord) : null;
}

/**
 * The chord an event makes, for the rebinding box; null while only modifiers are down. Ctrl and
 * Cmd both count as Mod, so a binding made on one platform works on another.
 */
export function chordFromEvent(event: KeyLike): string | null {
  if (MODIFIER_KEYS.has(event.key)) return null;
  return chordText({ mod: event.ctrlKey || event.metaKey, alt: event.altKey, shift: event.shiftKey, key: normalKey(event.key) });
}

/** Whether an event is exactly this chord (an extra modifier does not match). */
export function matchesChord(event: KeyLike, text: string): boolean {
  const chord = parseChord(text);
  return (
    chord !== null &&
    chord.mod === (event.ctrlKey || event.metaKey) &&
    chord.alt === event.altKey &&
    chord.shift === event.shiftKey &&
    chord.key === normalKey(event.key)
  );
}

/**
 * Whether a chord may be bound at all: it needs Mod or Alt (or be a function key), so a bound
 * shortcut never fires while typing text. Returns the reason it may not, or null.
 */
export function chordProblem(text: string): string | null {
  const chord = parseChord(text);
  if (!chord) return "That is not a key combination.";
  if (chord.key === "Escape") return "Escape is kept for closing things.";
  if (/^F\d{1,2}$/.test(chord.key)) return null;
  if (!chord.mod && !chord.alt) return "Hold Ctrl (Cmd on macOS) or Alt as well, so typing is not caught.";
  return null;
}

const byId = new Map<ShortcutId, ShortcutAction>(SHORTCUT_ACTIONS.map((action) => [action.id, action]));

export const isShortcutId = (id: string): id is ShortcutId => byId.has(id as ShortcutId);

/** The binding in force for an action: the user's, else the default. */
export function bindingFor(overrides: ShortcutOverrides | undefined, id: ShortcutId): string {
  return overrides?.[id] ?? byId.get(id)!.default;
}

/** Every action's binding in force, in registry order. */
export function allBindings(overrides: ShortcutOverrides | undefined): { action: ShortcutAction; chord: string }[] {
  return SHORTCUT_ACTIONS.map((action) => ({ action, chord: bindingFor(overrides, action.id) }));
}

/** Whether an event triggers an action under the bindings in force. */
export const triggers = (event: KeyLike, overrides: ShortcutOverrides | undefined, id: ShortcutId): boolean =>
  matchesChord(event, bindingFor(overrides, id));

export type Conflict = { with: ShortcutId | "reserved"; label: string };

/**
 * What a chord would clash with if `id` were bound to it: another action holding it now, or a
 * reserved chord (`reserved` adds to RESERVED_CHORDS, e.g. the push-to-talk key). Compared in
 * canonical form, so different spellings of one chord are the same.
 */
export function findConflict(
  overrides: ShortcutOverrides | undefined,
  id: ShortcutId,
  chord: string,
  reserved: readonly string[] = [],
): Conflict | null {
  const wanted = normalizeChord(chord);
  if (!wanted) return null;
  for (const { action, chord: held } of allBindings(overrides)) {
    if (action.id !== id && normalizeChord(held) === wanted) return { with: action.id, label: action.label };
  }
  for (const fixed of [...RESERVED_CHORDS, ...reserved]) {
    if (normalizeChord(fixed) === wanted) return { with: "reserved", label: `${fixed} (kept for another use)` };
  }
  return null;
}

/** Every pair of actions that share a binding under these overrides (empty when consistent). */
export function conflictsIn(overrides: ShortcutOverrides | undefined): [ShortcutId, ShortcutId][] {
  const all = allBindings(overrides);
  const pairs: [ShortcutId, ShortcutId][] = [];
  for (let i = 0; i < all.length; i++) {
    for (let j = i + 1; j < all.length; j++) {
      if (normalizeChord(all[i]!.chord) === normalizeChord(all[j]!.chord)) pairs.push([all[i]!.action.id, all[j]!.action.id]);
    }
  }
  return pairs;
}

/** Sets one binding; one equal to the default is dropped, so the default can move later. */
export function withBinding(overrides: ShortcutOverrides | undefined, id: ShortcutId, chord: string): ShortcutOverrides {
  const next: ShortcutOverrides = { ...(overrides ?? {}) };
  const canonical = normalizeChord(chord);
  if (!canonical || canonical === normalizeChord(byId.get(id)!.default)) delete next[id];
  else next[id] = canonical;
  return next;
}

/** Takes one action back to its default. */
export function withoutBinding(overrides: ShortcutOverrides | undefined, id: ShortcutId): ShortcutOverrides {
  const next: ShortcutOverrides = { ...(overrides ?? {}) };
  delete next[id];
  return next;
}

/**
 * Makes whatever was saved safe to use: drops actions that no longer exist, values that do not
 * parse or may not be bound, ones equal to the default, and any binding that clashes with
 * another action or a reserved chord (that action goes back to its default; the result is
 * checked again until it is consistent).
 */
export function sanitizeOverrides(saved: unknown, reserved: readonly string[] = []): ShortcutOverrides {
  let result: ShortcutOverrides = {};
  if (saved && typeof saved === "object" && !Array.isArray(saved)) {
    for (const action of SHORTCUT_ACTIONS) {
      const value = (saved as Record<string, unknown>)[action.id];
      if (typeof value !== "string") continue;
      const chord = normalizeChord(value);
      if (chord && chordProblem(chord) === null) result = withBinding(result, action.id, chord);
    }
  }
  for (let pass = 0; pass <= SHORTCUT_ACTIONS.length; pass++) {
    const clash = SHORTCUT_ACTIONS.find(
      (action) => result[action.id] !== undefined && findConflict(result, action.id, result[action.id]!, reserved) !== null,
    );
    if (!clash) break;
    result = withoutBinding(result, clash.id);
  }
  // A default can itself collide with an override (another action took it): move that one back too.
  for (const [a, b] of conflictsIn(result)) {
    if (result[b] !== undefined) result = withoutBinding(result, b);
    else if (result[a] !== undefined) result = withoutBinding(result, a);
  }
  return result;
}

/** A chord as this platform writes it: "Mod+," is "Ctrl+," here and "⌘," on macOS. */
export function formatChord(chord: string, mac: boolean): string {
  return mac
    ? chord.replace(/Mod\+/g, "⌘").replace(/Shift\+/g, "⇧").replace(/Alt\+/g, "⌥")
    : chord.replace(/Mod\+/g, "Ctrl+");
}
