// The review pane's wording and counting (src/review.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { ReviewFile, SessionCheckpoint } from "@imperium/protocol";

import { checkpointLabel, filesIn, lineCounts, lineKind, repoName } from "../src/review.ts";

const turn: SessionCheckpoint = {
  number: 1,
  at_unix_ms: 0,
  kind: "turn",
  prompt: "Fix the parser\nand its tests",
  repos: [],
  conversation: true,
};

test("checkpoints are named by their number and the message they came before", () => {
  assert.equal(checkpointLabel(turn, true), "#1 · session start · before “Fix the parser”");
  assert.equal(checkpointLabel({ ...turn, number: 3 }, false), "#3 · before “Fix the parser”");
  assert.equal(checkpointLabel({ ...turn, number: 4, kind: "rewind", prompt: "" }, false), "#4 · before a rewind");
  assert.ok(checkpointLabel({ ...turn, prompt: "x".repeat(80) }, false).endsWith("…”"));
});

const file: ReviewFile = {
  repo: "C:\\work\\app",
  path: "src/a.ts",
  status: "modified",
  binary: false,
  hunks: [
    { id: "1", header: "@@ -1,3 +1,3 @@", old_start: 1, old_lines: 3, new_start: 1, new_lines: 3, lines: [" a", "-b", "+B", "+C", " d"] },
  ],
};

test("diff lines are told apart by their sign and counted", () => {
  assert.equal(lineKind("+x"), "add");
  assert.equal(lineKind("-x"), "del");
  assert.equal(lineKind(" x"), "ctx");
  assert.deepEqual(lineCounts(file), { added: 2, removed: 1 });
});

test("repositories are named by their folder", () => {
  assert.equal(repoName("C:\\work\\app"), "app");
  assert.equal(repoName("/home/me/app/"), "app");
  assert.deepEqual(filesIn([file], "C:\\work\\app"), [file]);
  assert.deepEqual(filesIn([file], "/other"), []);
});
