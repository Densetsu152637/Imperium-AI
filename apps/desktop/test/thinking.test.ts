// The collapsed thinking block (src/sessions/thinking.ts): its wording, how fragments gather into
// blocks, and how a block that never got its `thinking_finished` is closed. Run with `pnpm test`
// (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { SessionEvent } from "@imperium/protocol";

import {
  closeThinking,
  formatThoughtDuration,
  foldThinking,
  thinkingView,
  type ThinkingEntry,
} from "../src/sessions/thinking.ts";
import { sessionNotice } from "../src/shared/attention.ts";
import type { Entry } from "../src/sessions/sessions.ts";

/** Applies events in order; plain entries (a reply, a notice) stand in for what the other events of
 * the stream add between thinking runs. */
const feed = (steps: (SessionEvent | Entry)[], start: Entry[] = []): Entry[] =>
  steps.reduce<Entry[]>((entries, step) => ("type" in step ? foldThinking(entries, step) : [...entries, step]), start);

const blocks = (entries: Entry[]): ThinkingEntry[] =>
  entries.filter((entry): entry is Entry & ThinkingEntry => entry.kind === "thinking");

const delta = (text: string, at_ms?: number): SessionEvent => ({ type: "thinking_delta", text, ...(at_ms === undefined ? {} : { at_ms }) });

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
  const live = thinkingView(null, false, false);
  assert.deepEqual([live.label, live.accessibleName, live.live], ["Thinking…", "Thinking", true]);
  const done = thinkingView(10_000, true, false);
  assert.deepEqual([done.label, done.accessibleName, done.live], ["Thought for 10 seconds", "Thought for 10 seconds", false]);
  assert.equal(thinkingView(60_000, true, false).accessibleName, "Thought for 1 minute");
});

test("a block cut off with no known time reads plainly Thought, and is no longer live", () => {
  const cut = thinkingView(null, true, false);
  assert.deepEqual([cut.label, cut.accessibleName, cut.live], ["Thought", "Thought", false]);
});

test("collapsed is the default: the text is in the page only when the block is open", () => {
  assert.equal(thinkingView(null, false, false).showText, false);
  assert.equal(thinkingView(4000, true, false).showText, false);
  assert.equal(thinkingView(null, true, false).showText, false);
  assert.equal(thinkingView(null, false, true).showText, true);
  assert.equal(thinkingView(4000, true, true).showText, true);
  // The label never carries the reasoning, open or closed.
  assert.ok(!thinkingView(4000, true, true).label.includes("secret"));
});

test("reasoning fragments gather into one live block that finishes with its duration", () => {
  const live = feed([{ kind: "user", text: "go" }, delta("let me "), delta("think")]);
  assert.equal(live.length, 2);
  const block = live[1] as ThinkingEntry;
  assert.deepEqual([block.kind, block.text, block.durationMs, block.ended], ["thinking", "let me think", null, false]);

  const done = feed([delta("a"), { type: "thinking_finished", duration_ms: 2500 }, { kind: "assistant", text: "The answer." }]);
  assert.deepEqual(done.map((entry) => entry.kind), ["thinking", "assistant"]);
  assert.deepEqual([blocks(done)[0].durationMs, blocks(done)[0].ended], [2500, true]);
});

test("each run of thinking is its own block, around other entries too", () => {
  const entries = feed([
    delta("first"),
    { type: "thinking_finished", duration_ms: 1000 },
    { kind: "notice", message: "a tool ran" },
    delta("second"),
    { type: "thinking_finished", duration_ms: 3000 },
    // A run straight after a finished one starts a new block, not a longer one.
    delta("third"),
  ]);
  assert.deepEqual(
    blocks(entries).map((block) => [block.text, block.durationMs]),
    [
      ["first", 1000],
      ["second", 3000],
      ["third", null],
    ],
  );
});

test("a fragment joins the open block wherever it sits, not only at the end of the transcript", () => {
  const entries = feed([delta("one "), { kind: "notice", message: "in between" }, delta("two")]);
  assert.equal(blocks(entries).length, 1);
  assert.equal(blocks(entries)[0].text, "one two");
  // And a finish closes that same block.
  const closed = feed([{ type: "thinking_finished", duration_ms: 7000 }], entries);
  assert.deepEqual([blocks(closed)[0].durationMs, blocks(closed)[0].ended], [7000, true]);
});

test("a stray finish with no open block changes nothing", () => {
  const entries = feed([{ kind: "user", text: "go" }, { type: "thinking_finished", duration_ms: 5 }]);
  assert.equal(entries.length, 1);
});

test("a block starts from its first fragment's own timestamp, never this window's clock", () => {
  const [block] = blocks(feed([delta("a", 1_000), delta("b", 4_000)]));
  assert.deepEqual([block.startedAt, block.lastAt], [1_000, 4_000]);
  const [untimed] = blocks(feed([delta("a")]));
  assert.deepEqual([untimed.startedAt, untimed.lastAt], [null, null]);
});

test("a block still open when its turn ends closes with the span of its own timestamps", () => {
  const open = feed([delta("a", 10_000), delta("b", 22_000)]);
  const closed = closeThinking(open);
  assert.deepEqual([blocks(closed)[0].ended, blocks(closed)[0].durationMs], [true, 12_000]);
  const view = thinkingView(blocks(closed)[0].durationMs, true, false);
  assert.equal(view.label, "Thought for 12 seconds");
});

test("a cut-off block with no timestamps closes as a plain Thought", () => {
  const closed = closeThinking(feed([delta("a")]));
  assert.deepEqual([blocks(closed)[0].ended, blocks(closed)[0].durationMs], [true, null]);
  assert.equal(thinkingView(null, true, false).label, "Thought");
});

test("closing leaves finished blocks and other entries alone, and returns the same array when none is open", () => {
  const done = feed([{ kind: "user", text: "go" }, delta("a", 1), { type: "thinking_finished", duration_ms: 9000 }]);
  assert.equal(closeThinking(done), done);
  const mixed = feed([delta("b", 5_000)], done);
  const closed = closeThinking(mixed);
  assert.deepEqual(blocks(closed).map((block) => block.durationMs), [9000, 0]);
  assert.equal(closed[0], mixed[0]);
});

test("a thinking event raises no notification", () => {
  for (const event of [delta("secret"), { type: "thinking_finished", duration_ms: 1 }] as SessionEvent[]) {
    assert.equal(sessionNotice("s1", "Title", "root", event), null);
  }
});
