// The shortcut registry (src/settings/shortcuts.ts): parsing, matching, conflicts, and sanitising saved
// bindings. Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  SHORTCUT_ACTIONS,
  bindingFor,
  chordFromEvent,
  chordProblem,
  conflictsIn,
  findConflict,
  formatChord,
  matchesChord,
  normalizeChord,
  parseChord,
  sanitizeOverrides,
  shortcutHolding,
  triggers,
  withBinding,
  withoutBinding,
} from "../src/settings/shortcuts.ts";

const key = (k: string, mods: Partial<{ ctrlKey: boolean; metaKey: boolean; altKey: boolean; shiftKey: boolean }> = {}) => ({
  key: k,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  ...mods,
});

test("the defaults are valid, bindable and do not clash", () => {
  for (const action of SHORTCUT_ACTIONS) {
    assert.ok(parseChord(action.default), action.id);
    assert.equal(chordProblem(action.default), null, action.id);
  }
  assert.deepEqual(conflictsIn({}), []);
  assert.equal(new Set(SHORTCUT_ACTIONS.map((a) => a.id)).size, SHORTCUT_ACTIONS.length);
});

test("chords parse and normalise", () => {
  assert.equal(normalizeChord("Mod+shift+k"), "Mod+Shift+K");
  assert.equal(normalizeChord("Shift+Alt+Mod+K"), "Mod+Alt+Shift+K");
  assert.equal(normalizeChord("Mod++"), "Mod++");
  assert.equal(normalizeChord("Mod+Space"), "Mod+Space");
  assert.equal(parseChord(""), null);
  assert.equal(parseChord("Mod+"), null);
  assert.equal(parseChord("Ctrl+K"), null);
  assert.equal(normalizeChord("MOD+k"), "Mod+K");
  assert.equal(parseChord("Mod+Mod+K"), null);
  assert.equal(parseChord("Mod+Shift"), null);
});

test("an event matches exactly its chord, Ctrl or Cmd alike", () => {
  assert.equal(matchesChord(key("k", { ctrlKey: true }), "Mod+K"), true);
  assert.equal(matchesChord(key("K", { metaKey: true }), "Mod+K"), true);
  assert.equal(matchesChord(key("k", { ctrlKey: true, shiftKey: true }), "Mod+K"), false);
  assert.equal(matchesChord(key("k"), "Mod+K"), false);
  assert.equal(matchesChord(key(",", { ctrlKey: true }), "Mod+,"), true);
  assert.equal(matchesChord(key(" ", { altKey: true }), "Alt+Space"), true);
  assert.equal(matchesChord(key("F8"), "F8"), true);
});

test("an event becomes the chord that would match it", () => {
  assert.equal(chordFromEvent(key("k", { ctrlKey: true, shiftKey: true })), "Mod+Shift+K");
  assert.equal(chordFromEvent(key("Control", { ctrlKey: true })), null);
  const event = key("j", { metaKey: true, altKey: true });
  assert.equal(matchesChord(event, chordFromEvent(event)!), true);
});

test("a chord needs Mod or Alt unless it is a function key", () => {
  assert.equal(chordProblem("K"), "Hold Ctrl (Cmd on macOS) or Alt as well, so typing is not caught.");
  assert.equal(chordProblem("Shift+K") === null, false);
  assert.equal(chordProblem("Alt+K"), null);
  assert.equal(chordProblem("F9"), null);
  assert.equal(chordProblem("Mod+Escape") === null, false);
});

test("bindings fall back to the default and a default-equal binding is not stored", () => {
  assert.equal(bindingFor(undefined, "palette.toggle"), "Mod+K");
  assert.equal(bindingFor({ "palette.toggle": "Mod+P" }, "palette.toggle"), "Mod+P");
  const changed = withBinding({}, "palette.toggle", "mod+p");
  assert.deepEqual(changed, { "palette.toggle": "Mod+P" });
  assert.deepEqual(withBinding(changed, "palette.toggle", "Mod+K"), {});
  assert.deepEqual(withoutBinding(changed, "palette.toggle"), {});
  assert.equal(triggers(key("p", { ctrlKey: true }), changed, "palette.toggle"), true);
  assert.equal(triggers(key("k", { ctrlKey: true }), changed, "palette.toggle"), false);
});

test("conflicts name the action, or the reserved use, that holds the chord", () => {
  assert.deepEqual(findConflict({}, "palette.toggle", "Mod+,"), { with: "settings.toggle", label: "Settings" });
  assert.equal(findConflict({}, "palette.toggle", "mod+k"), null, "an action may keep its own chord");
  assert.equal(findConflict({}, "palette.toggle", "Mod+P"), null);
  assert.equal(findConflict({ "settings.toggle": "Mod+P" }, "palette.toggle", "Mod+P")?.with, "settings.toggle");
  assert.equal(findConflict({}, "palette.toggle", "Mod+Enter")?.with, "reserved");
  assert.equal(findConflict({}, "palette.toggle", "Mod+Space", ["Mod+Space"])?.with, "reserved");
  assert.equal(findConflict({}, "palette.toggle", "not a chord"), null);
});

test("two actions on one chord are reported", () => {
  assert.deepEqual(conflictsIn({ "palette.toggle": "Mod+," }), [["palette.toggle", "settings.toggle"]]);
});

test("saved bindings are cleaned: unknown, malformed, unbindable and clashing ones go", () => {
  assert.deepEqual(sanitizeOverrides(undefined), {});
  assert.deepEqual(sanitizeOverrides("x"), {});
  assert.deepEqual(sanitizeOverrides([]), {});
  assert.deepEqual(
    sanitizeOverrides({ "palette.toggle": "ctrl+p", "gone.action": "Mod+Q", "settings.toggle": 4, "session.stop": "K" }),
    {},
    "Ctrl+p is not Mod+P, a removed action, a non-string and a bare key all drop",
  );
  assert.deepEqual(sanitizeOverrides({ "palette.toggle": "mod+p" }), { "palette.toggle": "Mod+P" });
  // The palette taking the Settings default clashes, so it returns to its own default.
  assert.deepEqual(sanitizeOverrides({ "palette.toggle": "Mod+," }), {});
  // Two overrides on one chord: the earlier action in registry order is dropped, then the other is consistent.
  const both = sanitizeOverrides({ "palette.toggle": "Mod+P", "settings.toggle": "Mod+P" });
  assert.deepEqual(conflictsIn(both), []);
  // The push-to-talk key is reserved.
  assert.deepEqual(sanitizeOverrides({ "palette.toggle": "Mod+Space" }, ["Mod+Space"]), {});
});

test("AltGr never matches or records, and Ctrl+Alt on a typed character is refused", () => {
  const altGr = { ...key("@", { ctrlKey: true, altKey: true }), getModifierState: (k: string) => k === "AltGraph" };
  assert.equal(chordFromEvent(altGr), null);
  assert.equal(matchesChord(altGr, "Mod+Alt+@"), false);
  assert.notEqual(chordProblem("Mod+Alt+@"), null);
  assert.notEqual(chordProblem("Mod+Alt+,"), null);
  assert.equal(chordProblem("Mod+Alt+K"), null, "a letter is fine");
  assert.equal(chordProblem("Mod+Alt+F5"), null);
});

test("dead, composing and lock keys cannot be bound", () => {
  for (const k of ["Dead", "Process", "Unidentified", "CapsLock"]) {
    assert.equal(chordProblem(`Mod+${k}`), "That key cannot be used in a shortcut.", k);
  }
});

test("editing chords are reserved", () => {
  for (const k of ["C", "V", "X", "A", "Z", "Y", "Backspace"]) {
    assert.equal(findConflict({}, "palette.toggle", `Mod+${k}`)?.with, "reserved", k);
  }
  assert.deepEqual(sanitizeOverrides({ "palette.toggle": "Mod+C" }), {});
});

test("more editing chords are reserved, and refusals name the use in plain words", () => {
  for (const k of ["Mod+Shift+Z", "Mod+Delete", "Mod+Left", "Mod+Right"]) {
    assert.equal(findConflict({}, "palette.toggle", k)?.label, "text editing", k);
  }
  assert.equal(findConflict({}, "palette.toggle", "Mod+Enter")?.label, "sending a message");
  assert.equal(findConflict({}, "palette.toggle", "Mod+Space", ["Mod+Space"])?.label, "push-to-talk");
});

test("a push-to-talk key a binding already holds is found", () => {
  assert.equal(shortcutHolding({ "palette.toggle": "Mod+Space" }, ["Mod+Space"])?.id, "palette.toggle");
  assert.equal(shortcutHolding({}, ["Mod+Space"]), null);
  assert.equal(shortcutHolding({}, ["Mod+,"])?.id, "settings.toggle");
});

test("an extra Alt does not trigger the stop shortcut", () => {
  assert.equal(triggers(key(".", { ctrlKey: true }), {}, "session.stop"), true);
  assert.equal(triggers(key(".", { ctrlKey: true, altKey: true }), {}, "session.stop"), false);
});

test("a bare Shift chord is refused with the reason", () => {
  assert.equal(chordProblem("Shift+K"), "Hold Ctrl (Cmd on macOS) or Alt as well, so typing is not caught.");
});

test("shortcuts are written for the platform", () => {
  assert.equal(formatChord("Mod+Shift+K", false), "Ctrl+Shift+K");
  assert.equal(formatChord("Mod+Alt+K", true), "⌘⌥K");
});
