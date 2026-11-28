// A model's reasoning in the transcript: one collapsed "Thinking" block per run of thinking, which
// shows only a label and, while it runs, the time so far. The text is behind a click. Pure, so the
// rules are tested without a window (test/thinking.test.ts). The same wording is used by
// `imperium chat` (crates/imperium-cli/src/thinking.rs): keep the two in step.

import type { SessionEvent } from "@imperium/protocol";

import type { Entry } from "./sessions";

/** One run of the model's reasoning; `durationMs` is null while it goes on, `startedAt` is when this
 * window saw it begin (the clock of the live time shown). */
export type ThinkingEntry = { kind: "thinking"; text: string; durationMs: number | null; startedAt: number };

/** Applies a `thinking_delta` or `thinking_finished` event to a transcript: fragments gather into
 * the open block (a new one starts after a finished block), and a finish closes it. Returns a new
 * array; other events leave `entries` as they are. */
export function foldThinking(entries: readonly Entry[], event: SessionEvent, now: number): Entry[] {
  const next = [...entries];
  const open = next.findLastIndex((entry) => entry.kind === "thinking" && entry.durationMs === null);
  if (event.type === "thinking_delta") {
    // Only the block at the end of the transcript grows: anything after an open block means the
    // model went on to something else, so this fragment starts a new run.
    const last = next.at(-1);
    if (open >= 0 && last?.kind === "thinking" && last.durationMs === null) {
      next[open] = { ...last, text: last.text + event.text };
    } else {
      next.push({ kind: "thinking", text: event.text, durationMs: null, startedAt: now });
    }
  } else if (event.type === "thinking_finished" && open >= 0) {
    const block = next[open];
    if (block.kind === "thinking") next[open] = { ...block, durationMs: event.duration_ms };
  }
  return next;
}

/** A duration in words: "less than a second", "1 second", "10 seconds", "1 minute", "2 minutes 5 seconds". */
export function formatThoughtDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  if (total < 1) return "less than a second";
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  const unit = (count: number, word: string) => `${count} ${word}${count === 1 ? "" : "s"}`;
  if (minutes === 0) return unit(seconds, "second");
  return seconds === 0 ? unit(minutes, "minute") : `${unit(minutes, "minute")} ${unit(seconds, "second")}`;
}

/** What a thinking block shows and announces; `durationMs` is null while the model still thinks. */
export type ThinkingView = {
  /** The visible label. */
  label: string;
  /** What a screen reader announces for the button: "Thinking" or "Thought for N seconds". */
  accessibleName: string;
  /** Whether the model is still thinking (the block shows a moving indicator and the time so far). */
  live: boolean;
  /** Whether the reasoning text is in the page: only once the block is open. */
  showText: boolean;
};

export function thinkingView(durationMs: number | null, open: boolean): ThinkingView {
  if (durationMs === null) return { label: "Thinking…", accessibleName: "Thinking", live: true, showText: open };
  const label = `Thought for ${formatThoughtDuration(durationMs)}`;
  return { label, accessibleName: label, live: false, showText: open };
}
