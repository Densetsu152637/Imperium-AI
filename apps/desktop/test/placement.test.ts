// Modules inside panels, drops into windows, and the keyboard alternatives to dragging
// (src/layout/placement.ts, src/layout/moves.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { Arrangement } from "../src/layout/arrangement.ts";
import { applyMove, panelMoves } from "../src/layout/moves.ts";
import { activeModules, canHost, dropPanel, hostOf, placeInto, sameModules, takeOut, type Placement } from "../src/layout/placement.ts";

const isModule = (id: string) => id.startsWith("mod:");
const ARRANGEMENT: Arrangement = [
  { id: "left", panels: ["projects", "sessions"] },
  { id: "main", panels: ["session"] },
  { id: "right", panels: ["agents", "models", "mod:nursery:pets", "mod:clock:tick"] },
];
const PLACEMENT: Placement = { arrangement: ARRANGEMENT, modules: {} };
let counter = 0;
const newId = () => `column-${++counter}`;
const everything = () => true;
const title = (id: string) => id;

test("only mod panels are modules, and a module hosts nothing", () => {
  assert.ok(canHost("agents", "mod:nursery:pets", isModule));
  assert.ok(!canHost("agents", "models", isModule), "a built-in panel is not a module");
  assert.ok(!canHost("mod:clock:tick", "mod:nursery:pets", isModule), "a module cannot host");
  assert.ok(!canHost("agents", "agents", isModule));
});

test("placing a module into a host moves it from its old host and ignores impossible placements", () => {
  const first = placeInto({}, "mod:nursery:pets", "agents", isModule);
  assert.deepEqual(first, { agents: ["mod:nursery:pets"] });
  const second = placeInto(first, "mod:nursery:pets", "models", isModule);
  assert.deepEqual(second, { models: ["mod:nursery:pets"] });
  assert.equal(placeInto(second, "models", "agents", isModule), second);
  assert.equal(hostOf(second, "mod:nursery:pets"), "models");
  assert.deepEqual(takeOut(second, "mod:nursery:pets"), {});
});

test("a module whose host is not on screen shows in its own place, and goes back when the host returns", () => {
  const modules = { agents: ["mod:nursery:pets", "mod:clock:tick"] };
  assert.deepEqual(activeModules(modules, () => false, everything), {});
  assert.deepEqual(activeModules(modules, everything, (id) => id !== "mod:clock:tick"), { agents: ["mod:nursery:pets"] });
  assert.deepEqual(activeModules(modules, everything, everything), modules);
});

test("dragging a module onto a panel hosts it there; dragging it to a column takes it out", () => {
  const hosted = dropPanel(PLACEMENT, "mod:nursery:pets", { kind: "module", host: "agents" }, true, newId, isModule);
  assert.deepEqual(hosted.modules, { agents: ["mod:nursery:pets"] });
  assert.equal(hosted.openWindow, false);
  const out = dropPanel(hosted, "mod:nursery:pets", { kind: "panel", column: "left", before: "sessions" }, true, newId, isModule);
  assert.deepEqual(out.modules, {});
  assert.deepEqual(out.arrangement[0].panels, ["projects", "mod:nursery:pets", "sessions"]);
});

test("a drop that cannot happen changes nothing: not editing, a built-in panel as a module, a module as host", () => {
  const target = { kind: "module", host: "agents" } as const;
  assert.deepEqual(dropPanel(PLACEMENT, "mod:nursery:pets", target, false, newId, isModule).modules, {});
  assert.deepEqual(dropPanel(PLACEMENT, "models", target, true, newId, isModule).modules, {});
  assert.deepEqual(dropPanel(PLACEMENT, "mod:nursery:pets", { kind: "module", host: "mod:clock:tick" }, true, newId, isModule).modules, {});
  assert.equal(dropPanel(PLACEMENT, "models", { kind: "window" }, false, newId, isModule).openWindow, false);
});

test("dropping a panel beyond the window's edge asks for a window of its own and keeps its place for docking back", () => {
  const dropped = dropPanel(PLACEMENT, "models", { kind: "window" }, true, newId, isModule);
  assert.equal(dropped.openWindow, true);
  assert.deepEqual(dropped.arrangement, ARRANGEMENT);
});

test("sameModules compares hosts and order, not key order", () => {
  assert.ok(sameModules({ a: ["x"], b: ["y"] }, { b: ["y"], a: ["x"] }));
  assert.ok(!sameModules({ a: ["x", "y"] }, { a: ["y", "x"] }));
});

const movesOf = (panel: string, placement: Placement = PLACEMENT, shown: (id: string) => boolean = everything) =>
  panelMoves({ placement, panel, shown, title, isModule }).map((move) => move.label);

test("a stacked panel can move up, down, to other columns, to a new column and (a module) into hosts", () => {
  assert.deepEqual(movesOf("models"), [
    "Move up",
    "Move down",
    "Move to column 1",
    "Move to column 2",
    "Move to a new column at the left",
    "Move to a new column at the right",
  ]);
  assert.deepEqual(movesOf("mod:nursery:pets"), [
    "Move up",
    "Move down",
    "Move to column 1",
    "Move to column 2",
    "Move to a new column at the left",
    "Move to a new column at the right",
    "Move into projects",
    "Move into sessions",
    "Move into session",
    "Move into agents",
    "Move into models",
  ]);
});

test("the first panel cannot move up, the last cannot move down, and a lone panel needs no new column", () => {
  assert.ok(!movesOf("projects").includes("Move up"));
  assert.ok(movesOf("projects").includes("Move down"));
  assert.ok(!movesOf("mod:clock:tick").includes("Move down"));
  assert.ok(!movesOf("session").includes("Move to a new column at the left"));
});

test("closed and detached panels are not offered as places, and up and down skip them", () => {
  const shown = (id: string) => id !== "agents" && id !== "sessions";
  assert.ok(!movesOf("models", PLACEMENT, shown).includes("Move up"));
  assert.ok(!movesOf("projects", PLACEMENT, shown).includes("Move down"));
});

test("a menu move lands exactly where the same drag would", () => {
  const down = panelMoves({ placement: PLACEMENT, panel: "agents", shown: everything, title, isModule }).find((move) => move.label === "Move down");
  assert.ok(down);
  const result = applyMove(PLACEMENT, "agents", down, newId, isModule);
  assert.deepEqual(result.arrangement[2].panels, ["models", "agents", "mod:nursery:pets", "mod:clock:tick"]);
  const toColumn = panelMoves({ placement: PLACEMENT, panel: "agents", shown: everything, title, isModule }).find((move) => move.label === "Move to column 1");
  assert.ok(toColumn);
  assert.deepEqual(applyMove(PLACEMENT, "agents", toColumn, newId, isModule).arrangement[0].panels, ["projects", "sessions", "agents"]);
});

test("a hosted module can be moved out through the menu, and has no up or down", () => {
  const hosted: Placement = { arrangement: ARRANGEMENT, modules: { agents: ["mod:nursery:pets"] } };
  const labels = movesOf("mod:nursery:pets", hosted);
  assert.ok(!labels.includes("Move up") && !labels.includes("Move down"));
  assert.ok(labels.includes("Move to column 1") && labels.includes("Move into models") && !labels.includes("Move into agents"));
  const out = panelMoves({ placement: hosted, panel: "mod:nursery:pets", shown: everything, title, isModule }).find((move) => move.label === "Move to column 1");
  assert.ok(out);
  const result = applyMove(hosted, "mod:nursery:pets", out, newId, isModule);
  assert.deepEqual(result.modules, {});
  assert.ok(result.arrangement[0].panels.includes("mod:nursery:pets"));
});
