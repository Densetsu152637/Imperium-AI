// Per-environment layout overrides (src/layout/shapes.ts): which layout applies where, that edits
// land in the right one, and that damaged saved data falls back instead of breaking the layout.
// Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  activeShape,
  clearOverride,
  DEFAULT_SHAPE,
  editShape,
  hasOverride,
  sanitizeOverrides,
  sanitizeShape,
  saveOverride,
  type Shapes,
} from "../src/layout/shapes.ts";

const GLOBAL = { arrangement: [{ id: "main", panels: ["session"] }], hidden: ["usage"], modules: {} };
const EMPTY: Shapes = { global: GLOBAL, overrides: {} };

test("an environment without a layout of its own uses the global one, and so does no environment", () => {
  assert.equal(activeShape(EMPTY, "env-a"), GLOBAL);
  assert.equal(activeShape(EMPTY, null), GLOBAL);
  assert.ok(!hasOverride(EMPTY, "env-a"));
});

test("edits go to the global layout until an environment saves a layout of its own", () => {
  const edited = editShape(EMPTY, "env-a", { hidden: ["usage", "models"] });
  assert.deepEqual(edited.global.hidden, ["usage", "models"]);
  assert.deepEqual(edited.overrides, {});
});

test("saving a project layout copies what the environment shows, and later edits stay in it", () => {
  const saved = saveOverride(EMPTY, "env-a");
  assert.ok(hasOverride(saved, "env-a"));
  assert.deepEqual(activeShape(saved, "env-a"), GLOBAL);
  const edited = editShape(saved, "env-a", { hidden: [] });
  assert.deepEqual(edited.overrides["env-a"].hidden, []);
  assert.deepEqual(edited.global.hidden, ["usage"], "the global layout is untouched");
  assert.deepEqual(activeShape(edited, "env-b").hidden, ["usage"], "other environments follow the global layout");
  assert.deepEqual(activeShape(edited, null).hidden, ["usage"]);
});

test("global edits do not reach an environment with its own layout", () => {
  const saved = saveOverride(EMPTY, "env-a");
  const edited = editShape(saved, "env-b", { hidden: ["agents"] });
  assert.deepEqual(activeShape(edited, "env-a").hidden, ["usage"]);
  assert.deepEqual(activeShape(edited, "env-b").hidden, ["agents"]);
});

test("using the global layout again drops the override and falls back", () => {
  const edited = editShape(saveOverride(EMPTY, "env-a"), "env-a", { hidden: ["models"] });
  const cleared = clearOverride(edited, "env-a");
  assert.ok(!hasOverride(cleared, "env-a"));
  assert.deepEqual(activeShape(cleared, "env-a"), edited.global);
  assert.equal(clearOverride(cleared, "env-a"), cleared, "nothing to clear");
});

test("there is nothing to save for no environment", () => {
  assert.equal(saveOverride(EMPTY, null), EMPTY);
  assert.equal(clearOverride(EMPTY, null), EMPTY);
});

test("an environment id that looks like a prototype key is an ordinary id", () => {
  const saved = saveOverride(EMPTY, "__proto__");
  assert.ok(hasOverride(saved, "__proto__"));
  assert.equal(({} as Record<string, unknown>).hidden, undefined);
  assert.ok(!hasOverride(EMPTY, "toString"));
});

test("saved shapes are read back as they were", () => {
  const shapes = editShape(saveOverride(EMPTY, "env-a"), "env-a", {
    arrangement: [{ id: "left", panels: ["projects"] }],
    modules: { agents: ["mod:nursery:pets"] },
  });
  const round = JSON.parse(JSON.stringify(shapes.overrides)) as unknown;
  assert.deepEqual(sanitizeOverrides(round), shapes.overrides);
});

test("damaged saved data falls back field by field instead of losing the layout", () => {
  assert.deepEqual(sanitizeShape(undefined), DEFAULT_SHAPE);
  assert.deepEqual(sanitizeShape({ arrangement: "x", hidden: ["usage", 3, "usage"], modules: [] }), { arrangement: null, hidden: ["usage"], modules: {} });
  // A panel listed in two columns is not an arrangement.
  const twice = [
    { id: "a", panels: ["x"] },
    { id: "b", panels: ["x"] },
  ];
  assert.equal(sanitizeShape({ arrangement: twice }).arrangement, null);
  assert.deepEqual(sanitizeOverrides({ ok: { hidden: ["usage"] }, bad: 7, __proto__: { hidden: [] } }), { ok: { arrangement: null, hidden: ["usage"], modules: {} } });
  assert.deepEqual(sanitizeOverrides([1, 2]), {});
});

test("saved modules that cannot be hosted are dropped one by one, not all together", () => {
  const shape = sanitizeShape({ modules: { agents: ["session"], models: ["mod:a:b"], session: ["mod:c:d"], mod_odd: ["mod:e:f", "models"] } });
  assert.deepEqual(shape.modules, { models: ["mod:a:b"] });
  assert.deepEqual(sanitizeShape({ modules: { agents: ["session"] } }).modules, {});
});

test("saved panel ids with spaces, slashes or colons are kept", () => {
  const shape = sanitizeShape({ hidden: ["mod:my mod/x@1:p q"], modules: { agents: ["mod:my mod/x@1:p q"] } });
  assert.deepEqual(shape.hidden, ["mod:my mod/x@1:p q"]);
  assert.deepEqual(shape.modules, { agents: ["mod:my mod/x@1:p q"] });
});
