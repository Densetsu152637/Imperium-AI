// Which columns the window shows (src/layout/fold.ts): minimum widths, folding from the right,
// and one column at a time below 600 px. Run with `pnpm test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { classifyColumns, COLUMN_MIN, dragShares, foldColumns, isNarrow, knownWidth, NARROW_BELOW, type FoldColumn } from "../src/layout/fold.ts";

const col = (id: string, span = false) => ({ id, span });
const DEFAULT = classifyColumns([col("left"), col("main", true), col("right")]);
/** sidebar 220 + session 360 + side 280. */
const ALL = 860;

test("the minimums are 220, 360 and 280, and narrow starts below 600", () => {
  assert.deepEqual({ ...COLUMN_MIN }, { sidebar: 220, session: 360, side: 280 });
  assert.equal(NARROW_BELOW, 600);
});

test("the left columns are sidebars, the session's its own, the right ones sides", () => {
  assert.deepEqual(
    classifyColumns([col("a"), col("b"), col("main", true), col("c"), col("d")]).map((column) => column.kind),
    ["sidebar", "sidebar", "session", "side", "side"],
  );
  assert.deepEqual(
    classifyColumns([col("a"), col("b")]).map((column) => column.kind),
    ["side", "side"],
    "without a session every column is a side column",
  );
});

test("a window that exactly fits every column shows them all", () => {
  assert.deepEqual(foldColumns(ALL, DEFAULT), { narrow: false, shown: ["left", "main", "right"], folded: [] });
  assert.deepEqual(foldColumns(1920, DEFAULT).folded, []);
});

test("one pixel short folds the rightmost side column", () => {
  assert.deepEqual(foldColumns(ALL - 1, DEFAULT), { narrow: false, shown: ["left", "main"], folded: ["right"] });
});

test("the sidebar and the session (580) always fit once the window is wide enough not to be narrow", () => {
  assert.deepEqual(foldColumns(NARROW_BELOW, DEFAULT), { narrow: false, shown: ["left", "main"], folded: ["right"] });
});

test("600 px is not narrow and 599 px is: the session alone", () => {
  assert.equal(isNarrow(600), false);
  assert.equal(isNarrow(599), true);
  assert.equal(isNarrow(599.5), true);
  assert.deepEqual(foldColumns(599, DEFAULT), { narrow: true, shown: ["main"], folded: ["left", "right"] });
  assert.deepEqual(foldColumns(420, DEFAULT), { narrow: true, shown: ["main"], folded: ["left", "right"] });
  assert.deepEqual(foldColumns(0, DEFAULT).shown, ["main"]);
});

test("a window with no side columns never folds above 599 px, and the session alone shows below", () => {
  const alone = classifyColumns([col("main", true)]);
  assert.deepEqual(foldColumns(360, alone), { narrow: true, shown: ["main"], folded: [] });
  assert.deepEqual(foldColumns(700, alone), { narrow: false, shown: ["main"], folded: [] });
  const sidebarOnly = classifyColumns([col("left"), col("main", true)]);
  assert.deepEqual(foldColumns(600, sidebarOnly), { narrow: false, shown: ["left", "main"], folded: [] });
  const sideOnly = classifyColumns([col("main", true), col("right")]);
  assert.deepEqual(foldColumns(639, sideOnly), { narrow: false, shown: ["main"], folded: ["right"] });
  assert.deepEqual(foldColumns(640, sideOnly).folded, []);
});

test("many side columns fold one at a time from the right, the session and the left column last", () => {
  const many = classifyColumns([col("left"), col("main", true), col("r1"), col("r2"), col("r3"), col("r4")]);
  // 220 + 360 + 4 * 280 = 1700
  assert.deepEqual(foldColumns(1700, many).folded, []);
  assert.deepEqual(foldColumns(1699, many).folded, ["r4"]);
  assert.deepEqual(foldColumns(1419, many).folded, ["r3", "r4"]);
  assert.deepEqual(foldColumns(1139, many).folded, ["r2", "r3", "r4"]);
  assert.deepEqual(foldColumns(859, many), { narrow: false, shown: ["left", "main"], folded: ["r1", "r2", "r3", "r4"] });
  assert.deepEqual(foldColumns(600, many).shown, ["left", "main"]);
});

test("extra left columns fold after the right ones, nearest the session first", () => {
  const two = classifyColumns([col("l1"), col("l2"), col("main", true), col("right")]);
  // 220 + 220 + 360 + 280 = 1080
  assert.deepEqual(foldColumns(1080, two).folded, []);
  assert.deepEqual(foldColumns(1079, two).folded, ["right"]);
  assert.deepEqual(foldColumns(799, two), { narrow: false, shown: ["l1", "main"], folded: ["l2", "right"] });
});

test("the session's column stays even when it alone is wider than the window", () => {
  const wide: FoldColumn[] = [{ id: "main", kind: "session" }, { id: "right", kind: "side" }];
  assert.deepEqual(foldColumns(650, wide, { sidebar: 0, session: 900, side: 280 }), { narrow: false, shown: ["main"], folded: ["right"] });
});

test("without a session the first column stays", () => {
  const sides = classifyColumns([col("a"), col("b"), col("c")]);
  assert.deepEqual(foldColumns(700, sides), { narrow: false, shown: ["a", "b"], folded: ["c"] });
  assert.deepEqual(foldColumns(300, sides), { narrow: true, shown: ["a"], folded: ["b", "c"] });
});

test("no columns, and a width that is not a number, are handled", () => {
  assert.deepEqual(foldColumns(800, []), { narrow: false, shown: [], folded: [] });
  assert.deepEqual(foldColumns(Number.NaN, DEFAULT), { narrow: false, shown: ["left", "main", "right"], folded: [] });
});

test("styles/tokens.css repeats the minimums as the columns' min-width", () => {
  const css = readFileSync(fileURLToPath(new URL("../src/styles/tokens.css", import.meta.url)), "utf8");
  for (const [kind, min] of Object.entries(COLUMN_MIN)) {
    assert.match(css, new RegExp(`--col-min-${kind}:\\s*calc\\(${min} \\* var\\(--px\\)\\)`), `--col-min-${kind} is ${min}`);
  }
});

test("a divider drag keeps both columns at or above their minimum shares", () => {
  // 1280 px, shares 0.3 / 0.4 over a 0.7 pair; minimums 220 px and 360 px of 1280 / 1.0 (1 share = 1280 px).
  const min: [number, number] = [220 / 1280, 360 / 1280];
  const [a, b] = dragShares([0.3, 0.4], -0.2, min);
  assert.ok(Math.abs(a - 220 / 1280) < 1e-9, "dragged left past the minimum, the first stops there");
  assert.ok(Math.abs(a + b - 0.7) < 1e-9, "the pair keeps its total");
  const [c] = dragShares([0.3, 0.4], 0.5, min);
  assert.ok(Math.abs(c - (0.7 - 360 / 1280)) < 1e-9, "dragged right, the second stops at its minimum");
});

test("a column held at its minimum starts a drag from there, so dragging back answers at once", () => {
  const min: [number, number] = [220 / 1280, 0];
  // Saved share 0.1 is shown as the minimum (0.171875): a small drag right moves from the shown width.
  const [first] = dragShares([0.1, 0.6], 0.01, min);
  assert.ok(Math.abs(first - (220 / 1280 + 0.01)) < 1e-9);
  const [still] = dragShares([0.1, 0.6], -0.05, min);
  assert.ok(Math.abs(still - 220 / 1280) < 1e-9, "pushing further left moves nothing");
});

test("the floor of a share applies without minimums, and impossible minimums fall back to a split", () => {
  assert.deepEqual(dragShares([0.3, 0.3], -1, [0, 0]), [0.08, 0.52]);
  const [a, b] = dragShares([0.2, 0.2], 0.1, [0.3, 0.3]);
  assert.ok(Math.abs(a - 0.2) < 1e-9 && Math.abs(b - 0.2) < 1e-9);
});

test("a width of 0 is no reading: the last known width stays", () => {
  assert.equal(knownWidth(0, 700), 700);
  assert.equal(knownWidth(540, 700), 540);
  assert.ok(Number.isNaN(knownWidth(0, Number.NaN)), "none yet: unknown, shown whole");
  assert.equal(isNarrow(knownWidth(0, Number.NaN)), false);
});
