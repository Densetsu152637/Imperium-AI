// The collapsed thinking block (src/sessions/thinking.ts) and the transcript entries it is built
// from (src/sessions/sessions.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { SessionEvent } from "@imperium/protocol";

import { formatThoughtDuration, foldThinking, thinkingView, type ThinkingEntry } from "../src/sessions/thinking.ts";
import { sessionNotice } from "../src/shared/attention.ts";
import type { Entry } from "../src/sessions/sessions.ts";

/** Applies events in order; `other` entries (a reply, a tool call) stand in for what the other
 * events of the stream add between thinking runs. */
const feed = (steps: (SessionEvent | Entry)[]): Entry[] =>
  steps.reduce<Entry[]>(
    (entries, step) => ("type" in step ? foldThinking(entries, step, 0) : [...entries, step]),
    [],
  );

test("a duration is worded in seconds under a minute, with the singular for one", () => {
  assert.equal(formatThoughtDuration(0), "less than a second");
  assert.equal(formatThoughtDuration(999), "less than a second");
  assert.equal(formatThoughtDuration(1000), "1 second");
  assert.equal(formatThoughtDuration(1999), "1 second");
  assert.equal(formatThoughtDuration(2000), "2 seconds");
  assert.equal(formatThoughtDuration(10_000), "10 seconds");
  assert.equal(formatThoughtDuration(59_999), "59 seconds");
});

test("from a minute it is whole minutes plus any seconds", () => {
  assert.equal(formatThoughtDuration(60_000), "1 minute");
  assert.equal(formatThoughtDuration(61_000), "1 minute 1 second");
  assert.equal(formatThoughtDuration(120_000), "2 minutes");
  assert.equal(formatThoughtDuration(125_000), "2 minutes 5 seconds");
  assert.equal(formatThoughtDuration(3_600_000), "60 minutes");
  assert.equal(formatThoughtDuration(-5), "less than a second");
});

test("a live block says Thinking and a finished one says how long it thought", () => {
  const live = thinkingView(null, false);
  assert.deepEqual([live.label, live.accessibleName, live.live], ["Thinking…", "Thinking", true]);
  const done = thinkingView(10_000, false);
  assert.deepEqual([done.label, done.accessibleName, done.live], ["Thought for 10 seconds", "Thought for 10 seconds", false]);
  assert.equal(thinkingView(60_000, false).accessibleName, "Thought for 1 minute");
});

test("collapsed is the default: the text is in the page only when the block is open", () => {
  assert.equal(thinkingView(null, false).showText, false);
  assert.equal(thinkingView(4000, false).showText, false);
  assert.equal(thinkingView(null, true).showText, true);
  assert.equal(thinkingView(4000, true).showText, true);
  // The label never carries the reasoning, open or closed.
  assert.ok(!thinkingView(4000, true).label.includes("secret"));
});

test("reasoning fragments gather into one live block that finishes with its duration", () => {
  const live = feed([
    { kind: "user", text: "go" },
    { type: "thinking_delta", text: "let me " },
    { type: "thinking_delta", text: "think" },
  ]);
  assert.equal(live.length, 2);
  const block = live[1] as ThinkingEntry;
  assert.deepEqual([block.kind, block.text, block.durationMs], ["thinking", "let me think", null]);

  const done = feed([
    { type: "thinking_delta", text: "a" },
    { type: "thinking_finished", duration_ms: 2500 },
    { kind: "assistant", text: "The answer." },
  ]);
  assert.deepEqual(done.map((entry) => entry.kind), ["thinking", "assistant"]);
  assert.equal((done[0] as ThinkingEntry).durationMs, 2500);
});

test("each run of thinking is its own block, around other entries too", () => {
  const entries = feed([
    { type: "thinking_delta", text: "first" },
    { type: "thinking_finished", duration_ms: 1000 },
    { kind: "notice", message: "a tool ran" },
    { type: "thinking_delta", text: "second" },
    { type: "thinking_finished", duration_ms: 3000 },
    // A run straight after a finished one starts a new block, not a longer one.
    { type: "thinking_delta", text: "third" },
  ]);
  const blocks = entries.filter((entry): entry is Entry & ThinkingEntry => entry.kind === "thinking");
  assert.deepEqual(
    blocks.map((block) => [block.text, block.durationMs]),
    [
      ["first", 1000],
      ["second", 3000],
      ["third", null],
    ],
  );
});

test("a stray finish with no open block changes nothing", () => {
  const entries = feed([{ kind: "user", text: "go" }, { type: "thinking_finished", duration_ms: 5 }]);
  assert.equal(entries.length, 1);
});

test("a thinking event raises no notification", () => {
  for (const event of [
    { type: "thinking_delta", text: "secret" },
    { type: "thinking_finished", duration_ms: 1 },
  ] as SessionEvent[]) {
    assert.equal(sessionNotice("s1", "Title", "root", event), null);
  }
});
