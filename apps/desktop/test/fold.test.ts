// Which columns the window shows (src/layout/fold.ts): minimum widths, folding from the right,
// and one column at a time below 600 px. Run with `pnpm test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { classifyColumns, COLUMN_MIN, foldColumns, isNarrow, NARROW_BELOW, type FoldColumn } from "../src/layout/fold.ts";

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

test("styles/shell.css repeats the minimums as the columns' min-width", () => {
  const css = readFileSync(fileURLToPath(new URL("../src/styles/shell.css", import.meta.url)), "utf8");
  for (const [kind, min] of Object.entries(COLUMN_MIN)) {
    assert.match(css, new RegExp(`--col-min-${kind}:\\s*calc\\(${min} \\* var\\(--px\\)\\)`), `--col-min-${kind} is ${min}`);
  }
});
