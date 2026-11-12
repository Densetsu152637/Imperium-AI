// Which budgets cover a session, the indicator, alert notices, and budget stops (src/budgets.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { Budget, BudgetStatus, ModelRow } from "@imperium/protocol";

import { budgetNotice, budgetStop, covers, describeUsage, formatShare, indicator, modelSpec } from "../src/budgets.ts";

function budget(change: Partial<Budget>): Budget {
  return {
    id: "b",
    name: "",
    scope: "global",
    target: "",
    window: "weekly",
    tokens: 1000,
    cost_usd: null,
    thresholds: null,
    action: "warn",
    ...change,
  };
}

function status(change: Partial<Budget>, share: number, alerted: number[]): BudgetStatus {
  return {
    budget: budget(change),
    label: change.id ?? "b",
    window_start: "2026-11-09",
    window_end: "2026-11-16",
    tokens_used: Math.round(share * 1000),
    cost_usd_used: 0,
    share,
    alerted,
    exceeded: share >= 1,
  };
}

test("budgets cover sessions by environment, endpoint, and model", () => {
  assert.ok(covers(budget({}), null, null));
  assert.ok(covers(budget({ scope: "environment", target: "e1" }), "e1", null));
  assert.ok(!covers(budget({ scope: "environment", target: "e1" }), "e2", "a:b"));
  assert.ok(covers(budget({ scope: "endpoint", target: "router" }), null, "router:qwen/qwen3"));
  assert.ok(!covers(budget({ scope: "endpoint", target: "router" }), null, null));
  assert.ok(covers(budget({ scope: "model", target: "router:big" }), "e1", "router:big"));
  assert.ok(!covers(budget({ scope: "model", target: "router:big" }), "e1", "router:small"));
  // Calls through the local API are not a session's, so no session indicator shows them.
  assert.ok(!covers(budget({ scope: "source", target: "api" }), "e1", "router:big"));
});

test("catalog ids resolve to specs", () => {
  const models = [{ id: "big", spec: "router:big" } as ModelRow];
  assert.equal(modelSpec("big", models), "router:big");
  assert.equal(modelSpec("llamacpp:q", models), "llamacpp:q");
  assert.equal(modelSpec("unknown", models), null);
  assert.equal(modelSpec(null, models), null);
});

test("the indicator shows the budget furthest along past a threshold", () => {
  const statuses = [
    status({ id: "quiet" }, 0.3, []),
    status({ id: "near", scope: "environment", target: "e1" }, 0.85, [50, 80]),
    status({ id: "other", scope: "environment", target: "e2" }, 1.2, [50, 80, 100]),
  ];
  assert.equal(indicator(statuses.slice(0, 1), "e1", null), null);
  const near = indicator(statuses, "e1", null);
  assert.equal(near?.level, "near");
  assert.equal(near?.status.budget.id, "near");
  const over = indicator(statuses, "e2", null);
  assert.equal(over?.level, "over");
  assert.equal(over?.status.budget.id, "other");
  assert.equal(formatShare(0.996), "99%");
});

test("alerts become budget notices for the session that crossed them", () => {
  const notice = budgetNotice({
    budget_id: "b",
    label: "Work, weekly",
    threshold: 100,
    share: 1.02,
    action: "block",
    window_end: "2026-11-16",
    message: "Work, weekly is used up.",
    session_id: "s1",
  });
  assert.equal(notice.kind, "budget");
  assert.equal(notice.sessionId, "s1");
  assert.match(notice.title, /used up/);
  const early = budgetNotice({ ...{ budget_id: "b", label: "L", threshold: 50, share: 0.5, action: "warn", window_end: "", message: "", session_id: null } });
  assert.match(early.title, /50%/);
});

test("only -32008 errors with a budget are budget stops", () => {
  const data = { budget_id: "b", label: "L", action: "ask", window_end: "2026-11-16", message: "Start anyway?" };
  assert.deepEqual(budgetStop({ code: -32008, data }), data);
  assert.equal(budgetStop({ code: -32006, data }), null);
  assert.equal(budgetStop({ code: -32008 }), null);
  assert.equal(budgetStop(new Error("x")), null);
});

test("usage lines name tokens and cost", () => {
  const both = status({ cost_usd: 10 }, 0.5, [50]);
  assert.equal(describeUsage({ ...both, cost_usd_used: 3.1 }, String), "500 / 1000 tokens · $3.10 / $10.00");
});
