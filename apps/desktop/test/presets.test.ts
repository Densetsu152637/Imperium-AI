// Layout presets (src/layout/presets.ts): the exported file, and that importing refuses anything
// malformed or of a version this app does not know. Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { makePreset, missingMods, modOf, modsUsed, parsePreset, PRESET_MAX_CHARS, presetFileName, presetText } from "../src/layout/presets.ts";
import type { LayoutShape } from "../src/layout/shapes.ts";

const SHAPE: LayoutShape = {
  arrangement: [
    { id: "left", panels: ["projects", "sessions"] },
    { id: "main", panels: ["session"] },
    { id: "right", panels: ["agents", "mod:nursery:pets"] },
  ],
  hidden: ["usage", "mod:clock:tick"],
  modules: { agents: ["mod:nursery:pets"] },
};

const valid = () => JSON.parse(presetText(makePreset(SHAPE, "Wide review"))) as Record<string, unknown>;
const refused = (data: unknown) => {
  const result = parsePreset(typeof data === "string" ? data : JSON.stringify(data));
  assert.equal(result.ok, false, "expected a refusal");
  return result.ok ? "" : result.error;
};

test("an exported preset imports as the same layout, with the mods it needs listed", () => {
  const preset = makePreset(SHAPE, "  Wide review ");
  assert.equal(preset.name, "Wide review");
  assert.deepEqual(preset.mods, ["clock", "nursery"]);
  const result = parsePreset(presetText(preset));
  assert.ok(result.ok);
  assert.deepEqual(result.preset.shape, SHAPE);
  assert.deepEqual(result.preset, preset);
});

test("a preset is plain data: version 1, no code, a readable file name", () => {
  const data = valid();
  assert.equal(data.format, "imperium-layout-preset");
  assert.equal(data.version, 1);
  assert.deepEqual(Object.keys(data), ["format", "version", "name", "arrangement", "hidden", "modules", "mods"]);
  assert.equal(presetFileName(makePreset(SHAPE, "Wide review!")), "imperium-layout-wide-review.json");
  assert.equal(presetFileName(makePreset(SHAPE, "")), "imperium-layout-layout.json");
});

test("a default arrangement exports as null and comes back as null", () => {
  const preset = makePreset({ arrangement: null, hidden: [], modules: {} }, "Default");
  const result = parsePreset(presetText(preset));
  assert.ok(result.ok);
  assert.equal(result.preset.shape.arrangement, null);
});

test("unknown or missing versions are refused with a reason", () => {
  assert.match(refused({ ...valid(), version: 2 }), /newer than this app understands/);
  assert.match(refused({ ...valid(), version: 0 }), /not one this app understands/);
  assert.match(refused({ ...valid(), version: "1" }), /not one this app understands/);
  const { version: _gone, ...without } = valid();
  assert.match(refused(without), /not one this app understands/);
});

test("anything that is not this format is refused", () => {
  assert.match(refused("not json {"), /not valid JSON/);
  assert.match(refused("[]"), /not a layout preset/);
  assert.match(refused("null"), /not a layout preset/);
  assert.match(refused({ ...valid(), format: "something-else" }), /not an Imperium layout preset/);
  assert.match(refused({ ...valid(), script: "alert(1)" }), /fields this app does not know: script/);
  assert.match(refused("x".repeat(PRESET_MAX_CHARS + 1)), /too large/);
});

test("malformed fields are refused", () => {
  assert.match(refused({ ...valid(), name: "" }), /name/);
  assert.match(refused({ ...valid(), name: 5 }), /name/);
  assert.match(refused({ ...valid(), name: "x".repeat(81) }), /name/);
  assert.match(refused({ ...valid(), arrangement: "left" }), /arrangement/);
  assert.match(refused({ ...valid(), arrangement: [] }), /arrangement/);
  assert.match(refused({ ...valid(), arrangement: [{ id: "a", panels: ["x"] }, { id: "b", panels: ["x"] }] }), /arrangement/);
  assert.match(refused({ ...valid(), arrangement: [{ id: "a", panels: ["x"], run: "x" }] }), /arrangement/);
  assert.match(refused({ ...valid(), arrangement: [{ id: "a\u0007b", panels: ["x"] }] }), /arrangement/);
  assert.match(refused({ ...valid(), hidden: "usage" }), /closed panels/);
  assert.match(refused({ ...valid(), hidden: ["usage", "usage"] }), /closed panels/);
  assert.match(refused({ ...valid(), hidden: ["__proto__"] }), /closed panels/);
  assert.match(refused({ ...valid(), modules: [] }), /modules/);
  assert.match(refused({ ...valid(), modules: { agents: ["agents"] } }), /modules/);
  assert.match(refused({ ...valid(), modules: { a: ["m"], b: ["m"] } }), /modules/);
  assert.match(refused({ ...valid(), modules: { a: ["m"], m: ["n"] } }), /modules/);
  assert.match(refused({ ...valid(), modules: { a: [] } }), /modules/);
  assert.match(refused({ ...valid(), mods: "nursery" }), /mods/);
  assert.match(refused({ ...valid(), mods: ["__proto__"] }), /mods/);
});

test("importing never changes what it reads: the parsed layout owns its data", () => {
  const text = presetText(makePreset(SHAPE, "A"));
  const first = parsePreset(text);
  const second = parsePreset(text);
  assert.ok(first.ok && second.ok);
  first.preset.shape.hidden.push("models");
  assert.deepEqual(second.preset.shape.hidden, SHAPE.hidden);
});

test("a preset lists the mods it needs, and missing ones are found against the installed list", () => {
  assert.deepEqual(modsUsed(SHAPE), ["clock", "nursery"]);
  assert.deepEqual(modsUsed({ arrangement: null, hidden: [], modules: {} }), []);
  const preset = makePreset(SHAPE, "A");
  assert.deepEqual(missingMods(preset, ["nursery"]), ["clock"]);
  assert.deepEqual(missingMods(preset, ["nursery", "clock"]), []);
});

test("modules must be mod panels in a host that can take them: the session or a built-in panel is never hosted", () => {
  assert.match(refused({ ...valid(), modules: { agents: ["session"] } }), /modules/);
  assert.match(refused({ ...valid(), modules: { agents: ["models"] } }), /modules/);
  assert.match(refused({ ...valid(), modules: { session: ["mod:nursery:pets"] } }), /modules/);
});

test("odd mod and panel ids (spaces, slashes, colons, long) survive export and import", () => {
  const odd = "mod:my mod/é@1:a:panel one";
  const long = `mod:long:${"p".repeat(200)}`;
  const shape: LayoutShape = {
    arrangement: [{ id: "left", panels: ["projects", odd, long] }],
    hidden: [],
    modules: { agents: [odd] },
  };
  const preset = makePreset(shape, "Odd", ["my mod/é@1:a", "long"]);
  assert.deepEqual(preset.mods, ["long", "my mod/é@1:a"], "a mod id with a colon is not truncated when the installed mods are known");
  const result = parsePreset(presetText(preset));
  assert.ok(result.ok);
  assert.deepEqual(result.preset, preset);
  assert.equal(modOf(odd, ["my mod/é@1:a"]), "my mod/é@1:a");
});
