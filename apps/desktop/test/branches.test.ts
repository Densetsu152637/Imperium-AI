// Branched sessions and model comparisons (src/branches.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { SessionSummary } from "@imperium/protocol";

import {
  branchNote,
  branchRun,
  compareProblem,
  comparisonMembers,
  comparisonOpen,
  nestBranches,
  standouts,
  togglePicked,
  type RunSource,
} from "../src/branches.ts";

function summary(id: string, extra: Partial<SessionSummary> = {}): SessionSummary {
  return {
    session_id: id,
    project_dir: "/work",
    environment_id: null,
    folders: ["/work"],
    model: "fake:m",
    isolation: "",
    title: id,
    status: "idle",
    status_detail: null,
    resume_policy: { mode: "stop", at_time: null, retry_every_minutes: 5, max_wait_hours: 12 },
    background: false,
    hold: null,
    persistent: false,
    worktrees: [],
    pinned: false,
    archived: false,
    archived_at: null,
    created_at: null,
    reasoning: false,
    ...extra,
  } as SessionSummary;
}

const usage = (prompt: number, completion: number) => ({ prompt_tokens: prompt, completion_tokens: completion, cached_tokens: 0 });

test("branches are listed under their source, one level deeper each", () => {
  const list = [
    { summary: summary("a") },
    { summary: summary("b") },
    { summary: summary("a1", { branched_from: { session_id: "a", at: 4 } }) },
    { summary: summary("a1x", { branched_from: { session_id: "a1", at: 2 } }) },
    { summary: summary("b1", { branched_from: { session_id: "b", at: 1 } }) },
    { summary: summary("a2", { branched_from: { session_id: "a", at: 6 } }) },
  ];
  assert.deepEqual(
    nestBranches(list).map((n) => [n.session.summary.session_id, n.depth]),
    [
      ["a", 0],
      ["a1", 1],
      ["a1x", 2],
      ["a2", 1],
      ["b", 0],
      ["b1", 1],
    ],
  );
});

test("a branch whose source is not listed stands alone, and cycles do not loop", () => {
  const orphan = nestBranches([{ summary: summary("x", { branched_from: { session_id: "gone", at: 0 } }) }]);
  assert.deepEqual(orphan.map((n) => n.depth), [0]);
  const cycle = nestBranches([
    { summary: summary("p", { branched_from: { session_id: "q", at: 0 } }) },
    { summary: summary("q", { branched_from: { session_id: "p", at: 0 } }) },
  ]);
  assert.equal(cycle.length, 2);
});

function source(entries: RunSource["entries"], extra: Partial<RunSource> = {}): RunSource {
  return { summary: summary("s"), entries, busy: false, calls: [], turnCallsFrom: 0, ...extra };
}

test("a branch run reads the reply to the latest message, with its tokens, cost, and time", () => {
  const run = branchRun(
    source(
      [
        { kind: "user", text: "setup" },
        { kind: "assistant", text: "old" },
        { kind: "turn", usage: usage(1, 1), costUsd: 0.1 },
        { kind: "user", text: "compare me" },
        { kind: "assistant", text: "first" },
        { kind: "tool" },
        { kind: "assistant", text: "second" },
        { kind: "turn", usage: usage(30, 12), costUsd: 0.02 },
      ],
      {
        calls: [
          { durationMs: 500, costUsd: 0.1, usage: usage(1, 1) },
          { durationMs: 700, costUsd: 0.01, usage: usage(10, 4) },
          { durationMs: 300, costUsd: 0.01, usage: usage(20, 8) },
        ],
        turnCallsFrom: 1,
      },
    ),
    "compare me",
  );
  assert.equal(run.state, "done");
  assert.equal(run.text, "first\n\nsecond");
  assert.deepEqual(
    [run.promptTokens, run.completionTokens, run.durationMs, run.costUsd],
    [30, 12, 1000, 0.02],
  );
});

test("a branch that has not taken the message yet is waiting, not done with an earlier turn", () => {
  const entries = [
    { kind: "user", text: "setup" },
    { kind: "assistant", text: "old" },
    { kind: "turn", usage: usage(1, 1), costUsd: null },
  ];
  assert.equal(branchRun(source(entries), "compare me").state, "waiting");
  assert.equal(branchRun(source(entries), "compare me").text, "");
  assert.equal(branchRun(source([...entries, { kind: "user", text: "compare me" }], { busy: true }), "compare me").state, "running");
  const failed = branchRun(
    source([...entries, { kind: "user", text: "compare me" }, { kind: "error", message: "no model" }]),
    "compare me",
  );
  assert.deepEqual([failed.state, failed.error], ["failed", "no model"]);
});

test("the quickest and cheapest finished branches stand out, ties and lone runs do not", () => {
  const run = (id: string, durationMs: number | null, costUsd: number | null, state: "done" | "running" = "done") =>
    ({ sessionId: id, state, durationMs, costUsd }) as never;
  assert.deepEqual(standouts([run("a", 900, 0.5), run("b", 400, 0.5), run("c", 100, 0.9, "running")]), {
    fastest: "b",
    cheapest: null,
  });
  assert.deepEqual(standouts([run("a", 900, 0.5)]), { fastest: null, cheapest: null });
  assert.deepEqual(standouts([run("a", 900, null), run("b", 400, null)]), { fastest: "b", cheapest: null });
});

test("a comparison's members are those with its id, in creation order, and it stays open while two remain", () => {
  const list = [
    { summary: summary("late", { comparison_id: "c1", created_at: 20 }) },
    { summary: summary("other", { comparison_id: "c2", created_at: 5 }) },
    { summary: summary("early", { comparison_id: "c1", created_at: 10 }) },
  ];
  const members = comparisonMembers(list, "c1");
  assert.deepEqual(
    members.map((m) => m.summary.session_id),
    ["early", "late"],
  );
  assert.equal(comparisonOpen(members), true);
  assert.equal(comparisonOpen([members[0], { summary: summary("late", { archived: true }) }]), false);
});

test("a comparison needs two to four different models and a message", () => {
  assert.match(compareProblem(["a"], "go") ?? "", /at least 2/);
  assert.match(compareProblem(["a", "b", "c", "d", "e"], "go") ?? "", /at most 4/);
  assert.match(compareProblem(["a", "a"], "go") ?? "", /each model once/);
  assert.match(compareProblem(["a", "b"], "  ") ?? "", /message/);
  assert.equal(compareProblem(["a", "b"], "go"), null);
});

test("models are picked in order up to the limit, and picking again removes one", () => {
  let picked: string[] = [];
  for (const m of ["a", "b", "c", "d", "e"]) picked = togglePicked(picked, m);
  assert.deepEqual(picked, ["a", "b", "c", "d"]);
  assert.deepEqual(togglePicked(picked, "b"), ["a", "c", "d"]);
});

test("a branch's tooltip names its source and says when it is one of several models", () => {
  const titleOf = (id: string) => (id === "src" ? "Fix the build" : null);
  assert.equal(branchNote(summary("a"), titleOf), null);
  assert.equal(
    branchNote(summary("a", { branched_from: { session_id: "src", at: 3 } }), titleOf),
    "Branched from Fix the build",
  );
  assert.equal(
    branchNote(summary("a", { branched_from: { session_id: "src", at: 3 }, comparison_id: "c" }), titleOf),
    "One of the models compared from Fix the build",
  );
});
