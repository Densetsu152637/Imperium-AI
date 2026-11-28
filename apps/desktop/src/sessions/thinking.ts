// A model's reasoning in the transcript: one collapsed "Thinking" block per run of thinking, which
// shows only a label and, while it runs, the time so far. The text is behind a click. Pure, so the
// rules are tested without a window (test/thinking.test.ts). The same wording is used by
// `imperium chat` (crates/imperium-cli/src/thinking.rs): keep the two in step.

import type { SessionEvent } from "@imperium/protocol";

import type { Entry } from "./sessions";

/**
 * One run of the model's reasoning. It is live while `ended` is false. `durationMs` is the daemon's
 * own figure (`thinking_finished`) or, for a run cut off before that (the daemon died, the turn
 * failed or paused), the span between its first and last fragment; null when neither is known.
 * `startedAt` and `lastAt` are the fragments' own timestamps (`at_ms`), null when the event had
 * none, so a replayed session never takes them from this window's clock.
 */
export type ThinkingEntry = {
  kind: "thinking";
  text: string;
  durationMs: number | null;
  ended: boolean;
  startedAt: number | null;
  lastAt: number | null;
};

const isOpen = (entry: Entry): boolean => entry.kind === "thinking" && !entry.ended;

/** Applies a `thinking_delta` or `thinking_finished` event to a transcript: a fragment joins the
 * open block wherever it sits (a new block starts when none is open), and a finish closes it.
 * Returns a new array; other events leave `entries` as they are. */
export function foldThinking(entries: readonly Entry[], event: SessionEvent): Entry[] {
  const next = [...entries];
  const open = next.findLastIndex(isOpen);
  const block = open >= 0 ? next[open] : null;
  if (event.type === "thinking_delta") {
    const at = event.at_ms ?? null;
    if (block && block.kind === "thinking") {
      next[open] = { ...block, text: block.text + event.text, lastAt: at ?? block.lastAt };
    } else {
      next.push({ kind: "thinking", text: event.text, durationMs: null, ended: false, startedAt: at, lastAt: at });
    }
  } else if (event.type === "thinking_finished" && block && block.kind === "thinking") {
    next[open] = { ...block, durationMs: event.duration_ms, ended: true };
  }
  return next;
}

/** Closes every block still open when its turn ends, fails or pauses, or a new turn starts (a
 * `thinking_finished` that never came, after a daemon crash): its duration is the span of its own
 * timestamps, or unknown. Returns `entries` itself when none is open. */
export function closeThinking(entries: Entry[]): Entry[] {
  if (!entries.some(isOpen)) return entries;
  return entries.map((entry) =>
    entry.kind === "thinking" && !entry.ended
      ? {
          ...entry,
          ended: true,
          durationMs:
            entry.startedAt !== null && entry.lastAt !== null ? Math.max(0, entry.lastAt - entry.startedAt) : null,
        }
      : entry,
  );
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

/** A block first drawn this long after its run began was replayed (the window was opened or the
 * session reopened mid-thought); one drawn sooner is seen live. */
export const REPLAY_AFTER_MS = 5000;

/** The time so far shown on a live block. A block seen live is timed from `seenAt`, when this window
 * first drew it, so a daemon clock that differs from this machine's changes nothing; one that was
 * already running long before it was drawn (a replay) is timed from its own `startedAt`. Never
 * negative. */
export function liveElapsedMs(startedAt: number | null, seenAt: number, now: number): number {
  const replayed = startedAt !== null && seenAt - startedAt > REPLAY_AFTER_MS;
  return Math.max(0, now - (replayed ? startedAt : seenAt));
}

/** What a thinking block shows and announces. */
export type ThinkingView = {
  /** The visible label. */
  label: string;
  /** What a screen reader announces for the button: "Thinking", "Thought for N seconds" or "Thought". */
  accessibleName: string;
  /** Whether the model is still thinking (the block shows a moving indicator and the time so far). */
  live: boolean;
  /** Whether the reasoning text is in the page: only once the block is open. */
  showText: boolean;
};

/** `durationMs` is null while the model thinks, or after a cut-off run whose time is unknown
 * (`ended`), which reads plainly "Thought". */
export function thinkingView(durationMs: number | null, ended: boolean, open: boolean): ThinkingView {
  if (durationMs !== null) {
    const label = `Thought for ${formatThoughtDuration(durationMs)}`;
    return { label, accessibleName: label, live: false, showText: open };
  }
  if (ended) return { label: "Thought", accessibleName: "Thought", live: false, showText: open };
  return { label: "Thinking…", accessibleName: "Thinking", live: true, showText: open };
}
