// Settings → Budgets: token and cost budgets for all usage, an environment, an endpoint, or one
// model, over a day, a week, or a month (local time), with the thresholds that alert and what
// happens when one is used up (warn, ask before each new turn, or block new turns). Saved to the
// daemon's config file with `budgets.set`; the Usage panel shows where each stands.

import { Plus, X } from "lucide-react";
import { useEffect, useState } from "react";

import type {
  Budget,
  BudgetAction,
  BudgetScope,
  BudgetSettings,
  BudgetWindow,
  BudgetsResult,
  EnvironmentInfo,
  EnvironmentListResult,
  ModelListResult,
  WeekStart,
} from "@imperium/protocol";

import { describeUsage, formatShare } from "../budgets";
import type { DaemonClient } from "../daemon/client";
import { formatTokens } from "../sessions";
import { Select } from "./Select";
import { Row } from "./SettingsRow";

const SCOPES: { value: BudgetScope; label: string }[] = [
  { value: "global", label: "All usage" },
  { value: "environment", label: "Environment" },
  { value: "endpoint", label: "Endpoint" },
  { value: "model", label: "Model" },
  { value: "source", label: "API calls" },
];

const WINDOWS: { value: BudgetWindow; label: string }[] = [
  { value: "daily", label: "Daily" },
  { value: "weekly", label: "Weekly" },
  { value: "monthly", label: "Monthly" },
];

const ACTIONS: { value: BudgetAction; label: string; description: string }[] = [
  { value: "warn", label: "Warn only", description: "Alert, and let turns go on" },
  { value: "ask", label: "Ask first", description: "Ask before each new turn in this scope" },
  { value: "block", label: "Block", description: "Refuse new turns in this scope until the window resets" },
];

const PERIOD: Record<BudgetWindow, string> = { daily: "day", weekly: "week", monthly: "month" };

const DAYS: WeekStart[] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error));
const capital = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** `50, 80, 100` → [50, 80, 100]; anything that is not a number is dropped. */
export function parseThresholds(text: string): number[] {
  return [
    ...new Set(
      text
        .split(/[\s,]+/)
        .map((part) => Number(part.replace("%", "")))
        .filter((n) => Number.isInteger(n) && n > 0),
    ),
  ].sort((a, b) => a - b);
}

/** A whole number, or null for an empty field. */
function parseLimit(text: string, whole: boolean): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || n <= 0) return null;
  return whole ? Math.round(n) : n;
}

function newBudget(): Budget {
  return {
    id: "",
    name: "",
    scope: "global",
    target: "",
    window: "monthly",
    tokens: null,
    cost_usd: 10,
    thresholds: null,
    action: "warn",
  };
}

export function BudgetsSection({ client }: { client: DaemonClient }) {
  const [saved, setSaved] = useState<BudgetsResult | null>(null);
  const [draft, setDraft] = useState<BudgetSettings | null>(null);
  const [environments, setEnvironments] = useState<EnvironmentInfo[]>([]);
  const [specs, setSpecs] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    client
      .call<BudgetsResult>("budgets.get", {})
      .then((result) => {
        if (!live) return;
        setSaved(result);
        setDraft(result.settings);
      })
      .catch((failure: unknown) => live && setError(message(failure)));
    client
      .call<EnvironmentListResult>("environments.list", {})
      .then((listing) => live && setEnvironments(listing.environments))
      .catch(() => {});
    client
      .call<ModelListResult>("models.list", {})
      .then((table) => live && setSpecs([...new Set(table.models.map((row) => row.spec))].sort()))
      .catch(() => {});
    const stop = client.onNotification((notification) => {
      if (notification.method === "budgets.changed") setSaved(notification.params as BudgetsResult);
    });
    return () => {
      live = false;
      stop();
    };
  }, [client]);

  if (!saved || !draft) return error ? <p className="error">{error}</p> : <p className="muted">Loading…</p>;
  const changed = JSON.stringify(draft) !== JSON.stringify(saved.settings);
  const endpoints = [...new Set(specs.map((spec) => spec.split(":")[0]))].sort();
  const setBudget = (index: number, change: Partial<Budget>) =>
    setDraft({ ...draft, limits: draft.limits.map((budget, i) => (i === index ? { ...budget, ...change } : budget)) });

  const save = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await client.call<BudgetsResult>("budgets.set", draft);
      setSaved(result);
      setDraft(result.settings);
      setNotice("Saved. Budgets apply at once; usage already recorded in the current windows counts.");
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Row
        label="Week starts on"
        description="Weekly budgets reset at midnight (this computer's time) on this day."
      >
        <Select
          aria-label="Week starts on"
          value={draft.week_start}
          options={DAYS.map((day) => ({ value: day, label: capital(day) }))}
          disabled={busy}
          onChange={(day) => setDraft({ ...draft, week_start: day as WeekStart })}
        />
      </Row>
      <Row label="Month resets on day" description="Monthly budgets reset at midnight on this day of the month (1 to 28).">
        <input
          className="field budget-number"
          type="number"
          min={1}
          max={28}
          aria-label="Month resets on day"
          value={draft.month_start_day}
          disabled={busy}
          onChange={(event) =>
            setDraft({ ...draft, month_start_day: Math.min(28, Math.max(1, Math.round(Number(event.target.value) || 1))) })
          }
        />
      </Row>
      <Row
        label="Alert at"
        description="Percentages of a budget that send a notification, once each per window (Settings → Notifications)."
      >
        <ThresholdsField
          value={draft.thresholds}
          disabled={busy}
          ariaLabel="Default alert thresholds"
          onChange={(thresholds) => setDraft({ ...draft, thresholds: thresholds ?? [50, 80, 100] })}
        />
      </Row>

      <section className="environment-section budgets-editor" aria-label="Budgets">
        <div className="environment-section-head">
          <span className="environment-label">Budgets</span>
          <button type="button" disabled={busy} onClick={() => setDraft({ ...draft, limits: [...draft.limits, newBudget()] })}>
            <Plus aria-hidden="true" /> Add budget
          </button>
        </div>
        <p className="muted permissions-hint">
          Tokens are prompt plus completion tokens of every model call, subagents included. Cost counts calls to models with
          prices; local models count tokens only unless you give them a price. With both limits, whichever is further along
          counts. Turns already running always finish.
        </p>
        <datalist id="budget-endpoints">
          {endpoints.map((endpoint) => (
            <option key={endpoint} value={endpoint} />
          ))}
        </datalist>
        <datalist id="budget-models">
          {specs.map((spec) => (
            <option key={spec} value={spec} />
          ))}
        </datalist>
        {draft.limits.length === 0 && <p className="muted">No budgets yet.</p>}
        {draft.limits.map((budget, index) => {
          const status = saved.status.find((s) => s.budget.id === budget.id && budget.id);
          return (
            <div className="budget-card" key={budget.id || `new-${index}`}>
              <div className="budget-card-row">
                <input
                  className="field budget-name"
                  aria-label="Name"
                  placeholder="Name (optional)"
                  value={budget.name}
                  disabled={busy}
                  onChange={(event) => setBudget(index, { name: event.target.value })}
                />
                <button
                  type="button"
                  className="ghost icon-only"
                  aria-label={`Remove budget ${budget.name || index + 1}`}
                  title="Remove this budget"
                  disabled={busy}
                  onClick={() => setDraft({ ...draft, limits: draft.limits.filter((_, i) => i !== index) })}
                >
                  <X />
                </button>
              </div>
              <div className="budget-card-row">
                <Select
                  aria-label="Scope"
                  value={budget.scope}
                  options={SCOPES}
                  disabled={busy}
                  onChange={(scope) =>
                    setBudget(index, {
                      scope: scope as BudgetScope,
                      target: scope === "environment" ? (environments[0]?.id ?? "") : scope === "source" ? "api" : "",
                    })
                  }
                />
                {budget.scope === "environment" && (
                  <Select
                    aria-label="Environment"
                    value={budget.target}
                    placeholder="Choose an environment"
                    options={environments.map((env) => ({ value: env.id, label: env.name }))}
                    disabled={busy}
                    onChange={(target) => setBudget(index, { target })}
                  />
                )}
                {(budget.scope === "endpoint" || budget.scope === "model") && (
                  <input
                    className="field budget-target"
                    aria-label={budget.scope === "endpoint" ? "Endpoint" : "Model"}
                    list={budget.scope === "endpoint" ? "budget-endpoints" : "budget-models"}
                    placeholder={budget.scope === "endpoint" ? "endpoint, e.g. openrouter" : "endpoint:model"}
                    spellCheck={false}
                    value={budget.target}
                    disabled={busy}
                    onChange={(event) => setBudget(index, { target: event.target.value })}
                  />
                )}
                <Select
                  aria-label="Window"
                  value={budget.window}
                  options={WINDOWS}
                  disabled={busy}
                  onChange={(window) => setBudget(index, { window: window as BudgetWindow })}
                />
              </div>
              <div className="budget-card-row">
                <LimitField
                  label="Tokens"
                  placeholder="no token limit"
                  value={budget.tokens}
                  whole
                  disabled={busy}
                  onChange={(tokens) => setBudget(index, { tokens })}
                />
                <LimitField
                  label="US$"
                  placeholder="no cost limit"
                  value={budget.cost_usd}
                  disabled={busy}
                  onChange={(cost_usd) => setBudget(index, { cost_usd })}
                />
              </div>
              <div className="budget-card-row">
                <Select
                  aria-label="When used up"
                  value={budget.action}
                  options={ACTIONS}
                  disabled={busy}
                  onChange={(action) => setBudget(index, { action: action as BudgetAction })}
                />
                <ThresholdsField
                  value={budget.thresholds}
                  defaults={draft.thresholds}
                  disabled={busy}
                  ariaLabel="Alert thresholds for this budget"
                  onChange={(thresholds) => setBudget(index, { thresholds })}
                />
              </div>
              {status && (
                <p className="muted budget-card-status">
                  This {PERIOD[status.budget.window]}: {formatShare(status.share)} ·{" "}
                  {describeUsage(status, formatTokens)} · resets {status.window_end}
                </p>
              )}
            </div>
          );
        })}
      </section>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notice && !changed && <p className="muted">{notice}</p>}
      <div className="permissions-actions">
        <button onClick={() => setDraft(saved.settings)} disabled={busy || !changed}>
          Revert
        </button>
        <button className="primary" onClick={() => void save()} disabled={busy || !changed}>
          Save
        </button>
      </div>
    </>
  );
}

/** A limit typed as text, so it can be cleared; the budget gets null for an empty field. */
function LimitField({
  label,
  placeholder,
  value,
  whole = false,
  disabled,
  onChange,
}: {
  label: string;
  placeholder: string;
  value: number | null;
  whole?: boolean;
  disabled: boolean;
  onChange: (value: number | null) => void;
}) {
  const [text, setText] = useState(value === null ? "" : String(value));
  useEffect(() => {
    if (parseLimit(text, whole) !== value) setText(value === null ? "" : String(value));
    // Only an outside change (revert, save) rewrites the text.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);
  return (
    <label className="budget-limit">
      <span className="muted">{label}</span>
      <input
        className="field"
        inputMode={whole ? "numeric" : "decimal"}
        placeholder={placeholder}
        value={text}
        disabled={disabled}
        onChange={(event) => {
          setText(event.target.value);
          onChange(parseLimit(event.target.value, whole));
        }}
      />
    </label>
  );
}

/** Thresholds as `50, 80, 100`; empty means the defaults (null) where `defaults` is given. */
function ThresholdsField({
  value,
  defaults,
  disabled,
  ariaLabel,
  onChange,
}: {
  value: number[] | null;
  defaults?: number[];
  disabled: boolean;
  ariaLabel: string;
  onChange: (value: number[] | null) => void;
}) {
  const shown = (list: number[] | null) => (list ? list.join(", ") : "");
  const [text, setText] = useState(shown(value));
  useEffect(() => {
    if (JSON.stringify(value) !== JSON.stringify(text.trim() ? parseThresholds(text) : null)) setText(shown(value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);
  return (
    <label className="budget-limit">
      <span className="muted">Alert at %</span>
      <input
        className="field"
        aria-label={ariaLabel}
        placeholder={defaults ? `${defaults.join(", ")} (default)` : "50, 80, 100"}
        value={text}
        disabled={disabled}
        onChange={(event) => {
          setText(event.target.value);
          const parsed = parseThresholds(event.target.value);
          onChange(event.target.value.trim() && parsed.length > 0 ? parsed : defaults ? null : parsed.length > 0 ? parsed : null);
        }}
      />
    </label>
  );
}
