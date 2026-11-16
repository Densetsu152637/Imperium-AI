// Key handling for menus and tabs (shared/keyNav.ts) and popover placement (shared/floating.ts):
// the WAI-ARIA menu button and tabs patterns, tested without a DOM.

import assert from "node:assert/strict";
import { test } from "node:test";

import { placeList, placeTooltip, type Box } from "../src/shared/floating.ts";
import { edge, menuKey, stepWrap, tabKey } from "../src/shared/keyNav.ts";

const all = () => true;
const except = (...off: number[]) => (index: number) => !off.includes(index);

test("stepping wraps at both ends and skips disabled items", () => {
  assert.equal(stepWrap(4, 3, 1, all), 0);
  assert.equal(stepWrap(4, 0, -1, all), 3);
  assert.equal(stepWrap(4, 0, 1, except(1, 2)), 3);
  assert.equal(stepWrap(4, -1, 1, except(0)), 1);
  assert.equal(stepWrap(4, 4, -1, all), 3);
  assert.equal(stepWrap(3, 1, 1, except(0, 2)), 1, "with nothing else enabled it stays");
});

test("edges are the first and last enabled item, and -1 when none is", () => {
  assert.equal(edge(4, false, except(0)), 1);
  assert.equal(edge(4, true, except(3)), 2);
  assert.equal(edge(2, false, () => false), -1);
});

test("a menu: arrows move, Home and End jump, Enter and Space run, Escape closes, Tab leaves", () => {
  assert.deepEqual(menuKey("ArrowDown", 0, 3, all), { kind: "move", active: 1 });
  assert.deepEqual(menuKey("ArrowUp", 0, 3, all), { kind: "move", active: 2 });
  assert.deepEqual(menuKey("ArrowDown", 0, 3, except(1)), { kind: "move", active: 2 });
  assert.deepEqual(menuKey("Home", 2, 3, except(0)), { kind: "move", active: 1 });
  assert.deepEqual(menuKey("End", 0, 3, all), { kind: "move", active: 2 });
  assert.deepEqual(menuKey("Enter", 1, 3, all), { kind: "run", active: 1 });
  assert.deepEqual(menuKey(" ", 2, 3, all), { kind: "run", active: 2 });
  assert.deepEqual(menuKey("Escape", 0, 3, all), { kind: "close" });
  assert.deepEqual(menuKey("Tab", 0, 3, all), { kind: "leave" });
  assert.equal(menuKey("a", 0, 3, all), null);
});

test("tabs: Left and Right wrap, Home and End jump, disabled tabs are skipped", () => {
  assert.equal(tabKey("ArrowRight", 2, 3, all), 0);
  assert.equal(tabKey("ArrowLeft", 0, 3, all), 2);
  assert.equal(tabKey("ArrowRight", 0, 3, except(1)), 2);
  assert.equal(tabKey("Home", 2, 3, except(0)), 1);
  assert.equal(tabKey("End", 0, 3, except(2)), 1);
  assert.equal(tabKey("ArrowDown", 0, 3, all), null, "Up and Down belong to vertical tabs");
  assert.equal(tabKey("ArrowDown", 0, 3, all, true), 1);
  assert.equal(tabKey("ArrowRight", 0, 3, all, true), null);
  assert.equal(tabKey("Enter", 0, 3, all), null);
});

const anchor = (left: number, top: number, width = 100, height = 30): Box => ({ left, top, width, height, right: left + width, bottom: top + height });
const viewport = { width: 1000, height: 800 };

test("a list opens below its anchor, and above when there is no room below and more above", () => {
  const below = placeList(anchor(50, 100), viewport, { needed: 180, maxHeight: 320, minHeight: 120 });
  assert.equal(below.above, false);
  assert.equal(below.top, 134);
  assert.equal(below.maxHeight, 320);
  const above = placeList(anchor(50, 700), viewport, { needed: 180, maxHeight: 320, minHeight: 120 });
  assert.equal(above.above, true);
  assert.equal(above.top, 696);
  assert.equal(above.maxHeight, 320);
  const cramped = placeList(anchor(50, 50), { width: 1000, height: 150 }, { needed: 180, minHeight: 120 });
  assert.equal(cramped.maxHeight, 120, "never below the minimum");
  assert.equal(placeList(anchor(50, 100), viewport, { needed: 10 }).right, 850);
});

test("a tooltip is centred over its anchor, flips below at the top, and stays inside the window", () => {
  const size = { width: 80, height: 24 };
  const over = placeTooltip(anchor(500, 400, 40, 30), size, viewport);
  assert.deepEqual(over, { left: 480, top: 370, above: true });
  const under = placeTooltip(anchor(500, 10, 40, 30), size, viewport);
  assert.equal(under.above, false);
  assert.equal(under.top, 46);
  assert.equal(placeTooltip(anchor(0, 400, 20, 30), size, viewport).left, 8);
  assert.equal(placeTooltip(anchor(990, 400, 20, 30), size, viewport).left, 912);
});
