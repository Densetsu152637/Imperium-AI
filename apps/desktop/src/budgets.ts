// Budgets as the desktop shows them: which budgets cover a session, the indicator beside the
// context meter, the notice an alert sends, and the stop a used-up budget puts on a new turn.
// Pure (types only), so the rules are tested without a window (test/budgets.test.ts).

import type { Budget, BudgetAlert, BudgetStatus, BudgetStop, ModelRow } from "@imperium/protocol";

import type { Notice } from "./attention";

/** `-32008`: a used-up budget asks before, or blocks, a new turn (jsonrpc.rs `BUDGET_EXCEEDED`). */
export const BUDGET_EXCEEDED = -32008;

/** A model as a spec (`<endpoint>:<model>`): catalog ids are looked up in the model table. */
export function modelSpec(model: string | null | undefined, models: ModelRow[]): string | null {
  if (!model) return null;
  if (model.includes(":")) return model;
  return models.find((row) => row.id === model)?.spec ?? null;
}

/** Whether a budget covers a session in `environmentId` using `spec`. */
export function covers(budget: Budget, environmentId: string | null, spec: string | null): boolean {
  const target = budget.target.trim();
  switch (budget.scope) {
    case "global":
      return true;
    case "environment":
      return environmentId === target;
    case "endpoint":
      return spec !== null && spec.split(":")[0] === target;
    case "model":
      return spec === target;
    case "source":
      // Calls that come through an entry point (the local API) are not a session's.
      return false;
  }
}

export type Indicator = {
  /** `over`: used up; `near`: past an alert threshold. */
  level: "near" | "over";
  /** The budget furthest along. */
  status: BudgetStatus;
  /** Every budget over a threshold, furthest along first. */
  all: BudgetStatus[];
};

/** The budgets over a session past an alert threshold, or null when none is. */
export function indicator(statuses: BudgetStatus[], environmentId: string | null, spec: string | null): Indicator | null {
  const all = statuses
    .filter((status) => covers(status.budget, environmentId, spec) && (status.exceeded || status.alerted.length > 0))
    .sort((a, b) => b.share - a.share);
  if (all.length === 0) return null;
  return { level: all[0].exceeded ? "over" : "near", status: all[0], all };
}

/** `42%`, rounded down so 99.6% is not shown as used up. */
export function formatShare(share: number): string {
  return `${Math.floor(share * 100)}%`;
}

/** `$1.23`, or `<$0.01` for tiny amounts. */
export function formatUsd(amount: number): string {
  if (amount > 0 && amount < 0.01) return "<$0.01";
  return `$${amount.toFixed(2)}`;
}

/** The notice an alert sends through `notify`. */
export function budgetNotice(alert: BudgetAlert): Notice {
  const title = alert.share >= 1 ? `Budget used up: ${alert.label}` : `Budget at ${alert.threshold}%: ${alert.label}`;
  return { kind: "budget", title, body: alert.message, sessionId: alert.session_id };
}

/** The budget stop a failed `session.send` carries, or null for any other failure. */
export function budgetStop(error: unknown): BudgetStop | null {
  if (typeof error !== "object" || error === null) return null;
  const { code, data } = error as { code?: unknown; data?: unknown };
  if (code !== BUDGET_EXCEEDED || typeof data !== "object" || data === null) return null;
  const stop = data as Partial<BudgetStop>;
  return typeof stop.budget_id === "string" && (stop.action === "ask" || stop.action === "block") ? (stop as BudgetStop) : null;
}

/** "Tokens 1.2M / 5M · $3.10 / $10" for a budget's limits. */
export function describeUsage(status: BudgetStatus, tokens: (n: number) => string): string {
  const parts: string[] = [];
  if (status.budget.tokens !== null) parts.push(`${tokens(status.tokens_used)} / ${tokens(status.budget.tokens)} tokens`);
  if (status.budget.cost_usd !== null) parts.push(`${formatUsd(status.cost_usd_used)} / ${formatUsd(status.budget.cost_usd)}`);
  return parts.join(" · ");
}
