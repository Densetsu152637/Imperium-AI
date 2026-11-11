// The panel editor's arrangement (src/layout/arrangement.ts): which panels sit in which columns,
// what is saved, and that panels move only in the editor. Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  addColumn,
  applyDrop,
  reconcile,
  removeColumn,
  sameArrangement,
  visibleColumns,
  type Arrangement,
} from "../src/layout/arrangement.ts";

const DEFAULTS: Arrangement = [
  { id: "left", panels: ["projects", "sessions", "approvals"] },
  { id: "main", panels: ["session"] },
  { id: "right", panels: ["agents", "models", "usage"] },
];

let counter = 0;
const newId = () => `column-${++counter}`;

test("no saved arrangement means the defaults, as a copy", () => {
  const resolved = reconcile(null, DEFAULTS);
  assert.deepEqual(resolved, DEFAULTS);
  resolved[0].panels.push("x");
  assert.equal(DEFAULTS[0].panels.length, 3);
  assert.deepEqual(reconcile([], DEFAULTS), DEFAULTS);
});

test("a saved arrangement is kept, and panels it does not know join their default column", () => {
  const saved: Arrangement = [
    { id: "main", panels: ["session", "agents"] },
    { id: "left", panels: ["sessions", "projects"] },
    { id: "right", panels: ["models"] },
  ];
  assert.deepEqual(reconcile(saved, DEFAULTS), [
    { id: "main", panels: ["session", "agents"] },
    // approvals follows sessions, its default predecessor.
    { id: "left", panels: ["sessions", "approvals", "projects"] },
    { id: "right", panels: ["models", "usage"] },
  ]);
});

test("a new panel whose default column was removed joins its predecessor's column, else the last", () => {
  const saved: Arrangement = [
    { id: "main", panels: ["session", "projects", "sessions", "approvals"] },
    { id: "column-a", panels: ["models"] },
  ];
  // agents leads the right column and has no predecessor: the last column; usage follows models.
  assert.deepEqual(reconcile(saved, DEFAULTS), [
    { id: "main", panels: ["session", "projects", "sessions", "approvals"] },
    { id: "column-a", panels: ["agents", "models", "usage"] },
  ]);
});

test("saved panels that are not present now (a mod not loaded yet) keep their place", () => {
  const saved: Arrangement = [{ id: "left", panels: ["mod:nursery:room", "projects"] }, ...DEFAULTS.slice(1)];
  const resolved = reconcile(saved, DEFAULTS);
  assert.deepEqual(resolved[0].panels, ["mod:nursery:room", "projects", "sessions", "approvals"]);
  // Drawn without it until it loads.
  const shown = new Set(["projects", "sessions", "session", "agents"]);
  const drawn = visibleColumns(resolved, (id) => (shown.has(id) ? id : undefined), false);
  assert.deepEqual(drawn, [
    { id: "left", panels: ["projects", "sessions"] },
    { id: "main", panels: ["session"] },
    { id: "right", panels: ["agents"] },
  ]);
});

test("outside the editor panels cannot be rearranged", () => {
  const before = reconcile(null, DEFAULTS);
  assert.equal(applyDrop(before, "agents", { kind: "panel", column: "left", before: "projects" }, false, newId), before);
  assert.equal(applyDrop(before, "agents", { kind: "new-column", at: 0 }, false, newId), before);
  assert.equal(addColumn(before, false, newId), before);
  const withEmpty: Arrangement = [...before, { id: "spare", panels: [] }];
  assert.equal(removeColumn(withEmpty, "spare", () => true, false), withEmpty);
});

test("in the editor a panel moves within a column, to another column, or to its end", () => {
  const start = reconcile(null, DEFAULTS);
  const within = applyDrop(start, "sessions", { kind: "panel", column: "left", before: "projects" }, true, newId);
  assert.deepEqual(within[0].panels, ["sessions", "projects", "approvals"]);
  const across = applyDrop(start, "models", { kind: "panel", column: "left", before: "sessions" }, true, newId);
  assert.deepEqual(across[0].panels, ["projects", "models", "sessions", "approvals"]);
  assert.deepEqual(across[2].panels, ["agents", "usage"]);
  const atEnd = applyDrop(start, "projects", { kind: "panel", column: "right", before: null }, true, newId);
  assert.deepEqual(atEnd[0].panels, ["sessions", "approvals"]);
  assert.deepEqual(atEnd[2].panels, ["agents", "models", "usage", "projects"]);
  // Onto itself, or onto a column that is not there: nothing changes.
  assert.equal(applyDrop(start, "agents", { kind: "panel", column: "right", before: "agents" }, true, newId), start);
  assert.equal(applyDrop(start, "agents", { kind: "panel", column: "gone", before: null }, true, newId), start);
});

test("in the editor a drop at a column's edge makes a new column there", () => {
  const start = reconcile(null, DEFAULTS);
  const next = applyDrop(start, "usage", { kind: "new-column", at: 3 }, true, () => "column-new");
  assert.deepEqual(next.map((column) => column.id), ["left", "main", "right", "column-new"]);
  assert.deepEqual(next[3].panels, ["usage"]);
  assert.deepEqual(next[2].panels, ["agents", "models"]);
  const first = applyDrop(start, "agents", { kind: "new-column", at: 0 }, true, () => "column-first");
  assert.deepEqual(first[0], { id: "column-first", panels: ["agents"] });
});

test("columns are added empty, and removed only when they show nothing", () => {
  const start = reconcile(null, DEFAULTS);
  const added = addColumn(start, true, () => "column-x");
  assert.deepEqual(added[3], { id: "column-x", panels: [] });
  // Empty columns are drawn only in the editor.
  const lookup = (id: string) => id;
  assert.equal(visibleColumns(added, lookup, true).length, 4);
  assert.equal(visibleColumns(added, lookup, false).length, 3);

  assert.deepEqual(removeColumn(added, "column-x", () => true, true), start);
  // A column with a panel on screen stays.
  assert.equal(removeColumn(start, "right", () => true, true), start);
  // A column whose panels are all closed goes; they move to the column before it, keeping their place.
  const closed = new Set(["agents", "models", "usage"]);
  const removed = removeColumn(start, "right", (id) => !closed.has(id), true);
  assert.deepEqual(removed, [
    { id: "left", panels: ["projects", "sessions", "approvals"] },
    { id: "main", panels: ["session", "agents", "models", "usage"] },
  ]);
  // The first column's go to the next one; the last column stays.
  const firstGone = removeColumn(start, "left", () => false, true);
  assert.deepEqual(firstGone[0], { id: "main", panels: ["projects", "sessions", "approvals", "session"] });
  const only: Arrangement = [{ id: "solo", panels: [] }];
  assert.equal(removeColumn(only, "solo", () => false, true), only);
});

test("the arrangement round-trips through the saved layout document", () => {
  const edited = applyDrop(reconcile(null, DEFAULTS), "models", { kind: "new-column", at: 1 }, true, () => "column-m");
  const stored = JSON.parse(JSON.stringify({ arrangement: edited })) as { arrangement: Arrangement };
  const restored = reconcile(stored.arrangement, DEFAULTS);
  assert.ok(sameArrangement(restored, edited));
  assert.deepEqual(restored[1], { id: "column-m", panels: ["models"] });
});
