// Tool-call rows in the transcript (src/toolCalls.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { ToolEntry } from "../src/sessions.ts";
import {
  clipText,
  describeToolRun,
  formatToolDuration,
  groupToolRuns,
  itemForEntry,
  prettyArguments,
  toolRunState,
} from "../src/toolCalls.ts";

function call(tool: string, ok: boolean | null = true, summary = ""): ToolEntry {
  return { kind: "tool", callId: `${tool}-${summary}`, tool, summary, arguments: "", ok, output: "", durationMs: null };
}

const say = (text: string) => ({ kind: "assistant" as const, text });

test("consecutive calls fold into one item; a lone call and other entries stay as they are", () => {
  const entries = [say("a"), call("read_file"), call("run_command"), say("b"), call("read_file"), say("c")];
  const items = groupToolRuns(entries);
  assert.deepEqual(
    items.map((item) => [item.kind, item.index, item.kind === "tools" ? item.calls.length : item.entry.kind]),
    [
      ["entry", 0, "assistant"],
      ["tools", 1, 2],
      ["entry", 3, "assistant"],
      ["entry", 4, "tool"],
      ["entry", 5, "assistant"],
    ],
  );
  assert.deepEqual(groupToolRuns([]), []);
  const trailing = groupToolRuns([say("a"), call("a"), call("b"), call("c")]);
  assert.equal(trailing.length, 2);
  assert.equal(trailing[1].kind === "tools" && trailing[1].calls.length, 3);
});

test("an entry folded into a group maps to the group's item", () => {
  const items = groupToolRuns([say("a"), call("x"), call("y"), call("z"), say("b")]);
  assert.equal(itemForEntry(items, 0), 0);
  assert.equal(itemForEntry(items, 1), 1);
  assert.equal(itemForEntry(items, 3), 1);
  assert.equal(itemForEntry(items, 4), 2);
  assert.equal(itemForEntry(items, 5), null);
});

test("a group's line counts calls by kind in the order each first ran", () => {
  const calls = ["read_file", "run_command", "read_file", "web_search", "read_file", "run_command"].map((tool) => call(tool));
  assert.equal(describeToolRun(calls), "Read 3 files, ran 2 commands, searched 1 time");
  assert.equal(describeToolRun([call("write_file"), call("edit_file")]), "Edited 2 files");
  assert.equal(describeToolRun([call("read_file"), call("spawn_agent")]), "Read 1 file, used 1 other tool");
  assert.equal(describeToolRun([call("spawn_agent"), call(""), call("toString")]), "Used 3 tools");
});

test("a group runs while any call runs, and failed when any call failed", () => {
  const running = toolRunState([call("a", true), call("b", null, "first"), call("c", null, "second")]);
  assert.equal(running.ok, null);
  assert.equal(running.current?.summary, "second");
  assert.deepEqual(toolRunState([call("a", true), call("b", false)]), { ok: false, current: null });
  assert.deepEqual(toolRunState([call("a", true), call("b", true)]), { ok: true, current: null });
});

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
