// What a docked panel's ⋯ header menu offers (src/layout/panelMenu.ts). Run with `pnpm test`
// (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { panelMenuEntries } from "../src/layout/panelMenu.ts";

test("the panel's own actions come first, then fold-all, detach and close", () => {
  assert.deepEqual(panelMenuEntries({ own: 2, column: { allCollapsed: false }, canDetach: true, canClose: true }), [
    { kind: "panel", index: 0 },
    { kind: "panel", index: 1 },
    { kind: "fold-all", label: "Collapse all in this column" },
    { kind: "detach", label: "Open in its own window" },
    { kind: "close", label: "Close panel" },
  ]);
});

test("fold-all reads as expand when the whole column is folded, and only on a stack's first panel", () => {
  assert.deepEqual(panelMenuEntries({ own: 0, column: { allCollapsed: true }, canDetach: false, canClose: false }), [
    { kind: "fold-all", label: "Expand all in this column" },
  ]);
  assert.deepEqual(panelMenuEntries({ own: 0, canDetach: true, canClose: false }), [{ kind: "detach", label: "Open in its own window" }]);
});

test("a panel that cannot be folded, detached or closed and adds nothing has no menu", () => {
  assert.deepEqual(panelMenuEntries({ own: 0, canDetach: false, canClose: false }), []);
});
