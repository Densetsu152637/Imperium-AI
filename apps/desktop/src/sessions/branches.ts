// Branched sessions and model comparisons (roadmap item 7; docs/07-desktop-ui.md): where a
// branch sits in the session list, what each branch of a comparison said and cost, and the
// rules for starting a comparison. Pure, so they are tested without a window (test/branches.test.ts).

import type { SessionSummary, TokenUsage } from "@imperium/protocol";

/** Fewest and most models a comparison takes (the daemon's limits). */
export const COMPARE_MIN = 2;
export const COMPARE_MAX = 4;

type Listed = { summary: SessionSummary };

/** A session in the list and how far down its source's branches it sits. */
export type Nested<T> = { session: T; depth: number };

/**
 * Orders sessions so each branch follows the session it was branched from, one level deeper
 * (branches of branches go deeper still). A branch whose source is not in the list (another
 * group, archived, deleted) stands on its own at depth 0. Otherwise the order is kept.
 */
export function nestBranches<T extends Listed>(sessions: readonly T[]): Nested<T>[] {
  const ids = new Set(sessions.map((s) => s.summary.session_id));
  const parentOf = (s: T): string | null => {
    const from = s.summary.branched_from?.session_id;
    return from && from !== s.summary.session_id && ids.has(from) ? from : null;
  };
  const children = new Map<string, T[]>();
  for (const session of sessions) {
    const parent = parentOf(session);
    if (parent) children.set(parent, [...(children.get(parent) ?? []), session]);
  }
  const out: Nested<T>[] = [];
  const seen = new Set<string>();
  const visit = (session: T, depth: number) => {
    const id = session.summary.session_id;
    if (seen.has(id)) return;
    seen.add(id);
    out.push({ session, depth });
    for (const child of children.get(id) ?? []) visit(child, depth + 1);
  };
  for (const session of sessions) if (!parentOf(session)) visit(session, 0);
  // Branches that only point at each other (a cycle) are still listed.
  for (const session of sessions) visit(session, 0);
  return out;
}

/** The part of an entry the comparison reads (a subset of the transcript's entries). */
export type RunEntry = {
  kind: string;
  text?: string;
  usage?: TokenUsage;
  costUsd?: number | null;
  message?: string | null;
};

/** The part of a session the comparison reads. */
export type RunSource = {
  summary: SessionSummary;
  entries: readonly RunEntry[];
  busy: boolean;
  calls: readonly { durationMs: number; costUsd: number | null; usage: TokenUsage }[];
  /** Index in `calls` where the latest turn began. */
  turnCallsFrom: number;
};

/** One branch of a comparison as the side-by-side view shows it. */
export type BranchRun = {
  sessionId: string;
  model: string | null;
  archived: boolean;
  /** `waiting`: the message has not started a turn; `done` and `failed` end the turn. */
  state: "waiting" | "running" | "done" | "failed";
  /** What the model said in reply to the comparison's message. */
  text: string;
  /** Why it failed, when it did. */
  error: string | null;
  promptTokens: number | null;
  completionTokens: number | null;
  costUsd: number | null;
  /** Model time of the turn, summed over its calls; null before any call finished. */
  durationMs: number | null;
};

/**
 * What a branch said and cost in its latest turn. With the comparison's `message`, a branch
 * whose latest user message is not that one yet (it is still starting) is `waiting`, not done
 * with an earlier turn.
 */
export function branchRun(source: RunSource, message?: string): BranchRun {
  const lastUser = source.entries.map((e) => e.kind).lastIndexOf("user");
  const sentYet = message === undefined || source.entries[lastUser]?.text === message;
  const turn = source.entries.slice(lastUser + 1);
  const text = turn
    .filter((e) => e.kind === "assistant")
    .map((e) => e.text ?? "")
    .join("\n\n");
  const error = [...turn].reverse().find((e) => e.kind === "error")?.message ?? null;
  const ended = sentYet && turn.some((e) => e.kind === "turn");
  const calls = source.calls.slice(source.turnCallsFrom);
  const priced = calls.filter((call) => call.costUsd !== null);
  const state: BranchRun["state"] = source.busy
    ? "running"
    : ended
      ? "done"
      : error !== null
        ? "failed"
        : lastUser >= 0 && sentYet
          ? "running"
          : "waiting";
  return {
    sessionId: source.summary.session_id,
    model: source.summary.model,
    archived: source.summary.archived,
    state,
    text: sentYet ? text : "",
    error: sentYet ? error : null,
    promptTokens: calls.length ? calls.reduce((sum, call) => sum + call.usage.prompt_tokens, 0) : null,
    completionTokens: calls.length ? calls.reduce((sum, call) => sum + call.usage.completion_tokens, 0) : null,
    costUsd: priced.length ? priced.reduce((sum, call) => sum + (call.costUsd ?? 0), 0) : null,
    durationMs: calls.length ? calls.reduce((sum, call) => sum + call.durationMs, 0) : null,
  };
}

/** The sessions of one comparison, in the order they were started (their ids are random, so by creation). */
export function comparisonMembers<T extends Listed>(sessions: readonly T[], comparisonId: string): T[] {
  return sessions
    .filter((s) => s.summary.comparison_id === comparisonId)
    .sort(
      (a, b) =>
        (a.summary.created_at ?? 0) - (b.summary.created_at ?? 0) ||
        a.summary.session_id.localeCompare(b.summary.session_id),
    );
}

/** Whether the comparison can still be settled: more than one of its branches is not archived. */
export function comparisonOpen<T extends Listed>(members: readonly T[]): boolean {
  return members.filter((s) => !s.summary.archived).length > 1;
}

/** The branches that finished, marked where one is quickest or cheapest (ties mark none). */
export function standouts(runs: readonly BranchRun[]): { fastest: string | null; cheapest: string | null } {
  const done = runs.filter((run) => run.state === "done");
  const best = (value: (run: BranchRun) => number | null): string | null => {
    const known = done.flatMap((run) => {
      const v = value(run);
      return v === null ? [] : [{ id: run.sessionId, v }];
    });
    if (known.length < 2) return null;
    const lowest = Math.min(...known.map((k) => k.v));
    const winners = known.filter((k) => k.v === lowest);
    return winners.length === 1 ? winners[0].id : null;
  };
  return { fastest: best((run) => run.durationMs), cheapest: best((run) => run.costUsd) };
}

/** Why a comparison cannot be started with these models and message; null when it can. */
export function compareProblem(models: readonly string[], message: string): string | null {
  if (models.length < COMPARE_MIN) return `Pick at least ${COMPARE_MIN} models.`;
  if (models.length > COMPARE_MAX) return `Pick at most ${COMPARE_MAX} models.`;
  if (new Set(models).size !== models.length) return "Pick each model once.";
  if (!message.trim()) return "Write the message to send to each model.";
  return null;
}

/** Adds or removes `model` from the picked ones, keeping the order picked and the limit. */
export function togglePicked(picked: readonly string[], model: string): string[] {
  if (picked.includes(model)) return picked.filter((m) => m !== model);
  return picked.length >= COMPARE_MAX ? [...picked] : [...picked, model];
}

/** A branch's place in words, for tooltips: `Branched from <title>`. */
export function branchNote(summary: SessionSummary, titleOf: (sessionId: string) => string | null): string | null {
  const from = summary.branched_from;
  if (!from) return null;
  const title = titleOf(from.session_id);
  return summary.comparison_id
    ? `One of the models compared from ${title ?? "another session"}`
    : `Branched from ${title ?? "another session"}`;
}
