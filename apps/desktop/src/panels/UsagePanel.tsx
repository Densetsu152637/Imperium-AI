// Usage: every budget's current window against its limit, a bar chart of daily usage (for all
// usage, or the scope of the budget picked), and the environments, endpoints, and models that
// used the most. The daemon records each model call (`budgets.usage`) and announces new
// standings with `budgets.changed`; budgets are managed in Settings → Budgets.

import { Gauge } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { BudgetStatus, BudgetsResult, UsageAmount, UsageReport } from "@imperium/protocol";

import { describeUsage, formatShare, formatUsd } from "../budgets";
import type { DaemonClient } from "../daemon/client";
import { formatTokens } from "../sessions";

const DAYS = 30;
const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));

type Measure = "tokens" | "cost";

export function UsagePanel({ client }: { client: DaemonClient | null }) {
  const [budgets, setBudgets] = useState<BudgetsResult | null>(null);
  const [report, setReport] = useState<UsageReport | null>(null);
  const [scope, setScope] = useState<string | null>(null);
  const [measure, setMeasure] = useState<Measure>("tokens");
  const [error, setError] = useState<string | null>(null);
  const scopeRef = useRef(scope);
  scopeRef.current = scope;

  useEffect(() => {
    if (!client) return;
    let live = true;
    let timer: number | null = null;
    const load = () => {
      const params = scopeRef.current ? { days: DAYS, budget_id: scopeRef.current } : { days: DAYS };
      client
        .call<UsageReport>("budgets.usage", params)
        .then((next) => {
          if (!live) return;
          setReport(next);
          setError(null);
        })
        .catch((reason: unknown) => live && setError(message(reason)));
    };
    client
      .call<BudgetsResult>("budgets.get", {})
      .then((next) => live && setBudgets(next))
      .catch((reason: unknown) => live && setError(message(reason)));
    load();
    // Usage changes with every model call: refresh the chart at most every few seconds.
    const stop = client.onNotification((notification) => {
      if (notification.method !== "budgets.changed") return;
      setBudgets(notification.params as BudgetsResult);
      timer ??= window.setTimeout(() => {
        timer = null;
        load();
      }, 3000);
    });
    return () => {
      live = false;
      stop();
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [client, scope]);

  if (!client) return <p className="empty">Usage appears once imperium-core is connected.</p>;
  if (!report) return <p className="empty">{error ?? "Reading usage…"}</p>;
  const statuses = budgets?.status ?? [];
  const picked = statuses.find((status) => status.budget.id === scope) ?? null;
  const value = (amount: UsageAmount) => (measure === "cost" ? amount.cost_usd : amount.tokens);
  const format = (n: number) => (measure === "cost" ? formatUsd(n) : formatTokens(n));
  const total = report.days.reduce((sum, day) => sum + value(day), 0);

  return (
    <div className="resources usage">
      {error && <p className="warning">{error}</p>}
      <section className="resource-section" aria-label="Budgets">
        <h3>
          <Gauge aria-hidden={true} /> Budgets
        </h3>
        {statuses.length === 0 ? (
          <p className="empty">No budgets. Add them in Settings → Budgets.</p>
        ) : (
          <ul className="budget-list">
            {statuses.map((status) => (
              <BudgetRow
                key={status.budget.id}
                status={status}
                selected={status.budget.id === scope}
                onSelect={() => setScope((current) => (current === status.budget.id ? null : status.budget.id))}
              />
            ))}
          </ul>
        )}
      </section>

      <section className="resource-section" aria-label="History">
        <h3>
          {picked ? picked.label : "All usage"}, last {DAYS} days
          <span className="usage-measure" role="group" aria-label="Measure">
            {(["tokens", "cost"] as Measure[]).map((option) => (
              <button
                key={option}
                type="button"
                className="chip"
                aria-pressed={measure === option}
                onClick={() => setMeasure(option)}
              >
                {option === "tokens" ? "Tokens" : "Cost"}
              </button>
            ))}
          </span>
        </h3>
        <UsageChart days={report.days} value={value} format={format} />
        <p className="muted usage-total">
          {format(total)} in {DAYS} days
          {measure === "cost" && " (models with prices; local models count tokens only unless priced)"}
        </p>
      </section>

      <TopList title="Top environments" amounts={report.environments} value={value} format={format} />
      <TopList title="Top endpoints" amounts={report.endpoints} value={value} format={format} />
      <TopList title="Top models" amounts={report.models} value={value} format={format} />
    </div>
  );
}

function BudgetRow({ status, selected, onSelect }: { status: BudgetStatus; selected: boolean; onSelect: () => void }) {
  const percent = Math.min(status.share * 100, 100);
  const level = status.exceeded ? "high" : status.alerted.length > 0 ? "busy" : undefined;
  const action =
    status.budget.action === "block" ? "blocks when used up" : status.budget.action === "ask" ? "asks when used up" : "warns only";
  return (
    <li>
      <button
        type="button"
        className="ghost budget-row"
        aria-pressed={selected}
        onClick={onSelect}
        title={`Show the history of ${status.label}; ${action}`}
      >
        <span className="budget-row-label">{status.label}</span>
        <span
          className="meter-bar"
          role="meter"
          aria-label={status.label}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(percent)}
          data-level={level}
        >
          <span style={{ width: `${Math.max(percent, 1)}%` }} />
        </span>
        <span className="meter-value">{formatShare(status.share)}</span>
        <span className="budget-row-detail muted">
          {describeUsage(status, formatTokens)} · resets {status.window_end} · {action}
        </span>
      </button>
    </li>
  );
}

/** Daily bars, oldest at the left; the tallest day sets the scale. */
function UsageChart({
  days,
  value,
  format,
}: {
  days: UsageAmount[];
  value: (amount: UsageAmount) => number;
  format: (n: number) => string;
}) {
  const highest = Math.max(...days.map(value), 0);
  if (highest === 0) return <p className="empty">Nothing used in these days.</p>;
  const width = 100 / days.length;
  return (
    <svg className="usage-chart" viewBox="0 0 100 40" preserveAspectRatio="none" role="img" aria-label="Usage per day">
      {days.map((day, index) => {
        const height = (value(day) / highest) * 38;
        return (
          <rect key={day.key} x={index * width + width * 0.1} y={40 - height} width={width * 0.8} height={height}>
            <title>{`${day.key}: ${format(value(day))}, ${day.calls} calls`}</title>
          </rect>
        );
      })}
    </svg>
  );
}

function TopList({
  title,
  amounts,
  value,
  format,
}: {
  title: string;
  amounts: UsageAmount[];
  value: (amount: UsageAmount) => number;
  format: (n: number) => string;
}) {
  const top = [...amounts].sort((a, b) => value(b) - value(a)).slice(0, 5);
  return (
    <section className="resource-section" aria-label={title}>
      <h3>{title}</h3>
      {top.length === 0 ? (
        <p className="empty">Nothing yet.</p>
      ) : (
        <table>
          <tbody>
            {top.map((amount) => (
              <tr key={amount.key}>
                <td title={amount.key}>{amount.label}</td>
                <td>{format(value(amount))}</td>
                <td className="muted">{amount.calls} calls</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
