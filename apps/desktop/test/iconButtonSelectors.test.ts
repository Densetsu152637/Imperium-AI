// An IconButton sits inside the Tooltip's `display: contents` wrapper, so it is never a direct child of
// the element that holds it: a `> .message-action` rule stops matching and the button loses its place
// and its hover reveal. The session stylesheet must reach it with descendant selectors.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

test("session.css reaches the branch button with descendant selectors, not a child combinator", () => {
  const css = readFileSync(new URL("../src/styles/session.css", import.meta.url), "utf8");
  assert.ok(css.includes(".message-action"), "the branch button rules are gone");
  assert.doesNotMatch(css, />\s*\.message-action/);
});
