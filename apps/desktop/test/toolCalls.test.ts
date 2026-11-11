// Tool-call rows in the transcript (src/toolCalls.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { clipText, formatToolDuration, prettyArguments } from "../src/toolCalls.ts";

test("durations read as ms, seconds, or minutes", () => {
  assert.equal(formatToolDuration(340), "340 ms");
  assert.equal(formatToolDuration(2_400), "2.4 s");
  assert.equal(formatToolDuration(185_000), "3m 05s");
});

test("arguments are indented when they parse and kept as is when cut short", () => {
  assert.equal(prettyArguments('{"path":"a.txt"}'), '{\n  "path": "a.txt"\n}');
  assert.equal(prettyArguments('{"path":"a.t'), '{"path":"a.t');
  assert.equal(prettyArguments("{}"), "");
  assert.equal(prettyArguments("  "), "");
});

test("long text folds by characters or lines; short text shows whole", () => {
  assert.equal(clipText("short"), null);
  assert.equal(clipText("abcdef", 4), "abcd");
  assert.equal(clipText("1\n2\n3\n4", 100, 2), "1\n2");
  assert.equal(clipText("1\n2", 100, 2), null);
});
