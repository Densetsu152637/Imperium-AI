// Live telemetry: one row per model in use right now, drawn by the Telemetry panel as a Markdown
// table. Pure, so the rules and formatting are tested without a window (test/telemetry.test.ts).
//
// A model is active while an agent using it works: a session's root agent during a turn (from
// `turn_started` to `turn_completed` / `turn_failed`, unless the session is stopped) or a
// subagent whose task is running. Its tokens, cost, and speed cover that work only: the root
// agent's calls since its turn started and each running task's calls since it was spawned or
// resumed. Rows of several agents on the same model add up. A row the window saw go idle stays
// for `LINGER_MS` with its last values, then drops off; history replayed when a session is
// opened never shows idle rows.

import type { ModelRow } from "@imperium/protocol";

import type { ModelCall, SessionState } from "./sessions";

/** How long a model stays listed after its work ends. */
export const LINGER_MS = 60_000;

export type TelemetryRow = {
  /** The model spec the daemon reports (`endpoint:model`). */
  model: string;
  label: string;
  /** Generation speed of the model's most recent call; null before one finished. */
  tokensPerSec: number | null;
  tokensIn: number;
  tokensOut: number;
  /** Null when no call reported a cost. */
  costUsd: number | null;
  local: boolean;
};

/** The agents' model calls this table covers, with the models they run now. */
type Work = { models: string[]; calls: ModelCall[] };

/** Runtimes the daemon starts on this machine; their specs are `<runtime>:<model>`. */
const LOCAL_RUNTIMES = ["llamacpp", "vllm"];

const findRow = (spec: string, models: ModelRow[]): ModelRow | undefined =>
  models.find((row) => row.spec === spec) ?? models.find((row) => row.id === spec);

/**
 * The name a model goes by in telemetry: its id in the model table, or the spec without its
 * endpoint. The one place to switch to a model's alias.
 */
export function modelLabel(spec: string, models: ModelRow[]): string {
  const row = findRow(spec, models);
  if (row) return row.id;
  const colon = spec.indexOf(":");
  return colon > 0 && colon < spec.length - 1 ? spec.slice(colon + 1) : spec;
}

/** Whether a model runs on this machine: a local runtime's spec or a table row marked local. */
export function isLocalModel(spec: string, models: ModelRow[]): boolean {
  const row = findRow(spec, models);
  if (row) return row.location === "local";
  return LOCAL_RUNTIMES.includes(spec.slice(0, spec.indexOf(":")));
}

/** The work running in one session now: the root agent's turn and each running subagent. */
function activeWork(session: SessionState): Work[] {
  // Stopped (as stop.ts's isStopped, which node's test runner cannot import from here): nothing runs.
  if (session.status === "paused" && session.statusDetail === "stopped") return [];
  const work: Work[] = [];
  const callsSince = (from: number, agentId: string) => session.calls.slice(from).filter((call) => call.agentId === agentId);
  if (session.busy) {
    work.push({
      models: session.summary.model ? [session.summary.model] : [],
      calls: callsSince(session.turnCallsFrom ?? 0, "root"),
    });
  }
  for (const task of Object.values(session.tasks)) {
    if (task.status === "running") work.push({ models: task.model ? [task.model] : [], calls: callsSince(task.callsFrom ?? 0, task.taskId) });
  }
  return work;
}

/** One row per model the sessions use now, in the order they first appear. */
export function liveTelemetry(sessions: SessionState[], models: ModelRow[]): TelemetryRow[] {
  const rows = new Map<string, TelemetryRow>();
  const latestAt = new Map<string, number>();
  const row = (model: string) => {
    let found = rows.get(model);
    if (!found) {
      found = {
        model,
        label: modelLabel(model, models),
        tokensPerSec: null,
        tokensIn: 0,
        tokensOut: 0,
        costUsd: null,
        local: isLocalModel(model, models),
      };
      rows.set(model, found);
    }
    return found;
  };
  for (const work of sessions.flatMap(activeWork)) {
    for (const call of work.calls) {
      const found = row(call.model);
      found.tokensIn += call.usage.prompt_tokens;
      found.tokensOut += call.usage.completion_tokens;
      if (call.costUsd !== null) found.costUsd = (found.costUsd ?? 0) + call.costUsd;
      if (call.tokensPerSec !== null && call.at >= (latestAt.get(call.model) ?? -Infinity)) {
        found.tokensPerSec = call.tokensPerSec;
        latestAt.set(call.model, call.at);
      }
    }
    for (const model of work.models) row(model);
  }
  return [...rows.values()];
}

/** Rows that went idle while this window watched, and when. */
export type Lingering = Map<string, { row: TelemetryRow; idleSince: number }>;

/**
 * The rows to show: the active ones, then those that went idle less than `LINGER_MS` ago with
 * their last values. `previous` is the result of the last call; the new `lingering` replaces it.
 */
export function withLingering(
  active: TelemetryRow[],
  previous: { rows: TelemetryRow[]; lingering: Lingering },
  now: number,
): { rows: TelemetryRow[]; lingering: Lingering } {
  const live = new Set(active.map((row) => row.model));
  const lingering: Lingering = new Map();
  for (const [model, entry] of previous.lingering) {
    if (!live.has(model) && now - entry.idleSince < LINGER_MS) lingering.set(model, entry);
  }
  for (const row of previous.rows) {
    // Only rows that were live last time start lingering; expired ones stay gone.
    if (!live.has(row.model) && !previous.lingering.has(row.model)) lingering.set(row.model, { row, idleSince: now });
  }
  return { rows: [...active, ...[...lingering.values()].map((entry) => entry.row)], lingering };
}

/** Tokens per second with one decimal, or an em dash before a speed was measured. */
export const formatSpeed = (tokensPerSec: number | null): string =>
  tokensPerSec === null || !Number.isFinite(tokensPerSec) ? "—" : tokensPerSec.toFixed(1);

/** A token count: plain below a thousand, else two decimals with `k` or `M` (`54.00k`, `1.00M`). */
export function formatTokenCount(n: number): string {
  const size = Math.abs(n);
  if (size < 1e3) return String(Math.round(n));
  const thousands = (n / 1e3).toFixed(2);
  if (size < 1e6 && Math.abs(Number(thousands)) < 1e3) return `${thousands}k`;
  return `${(n / 1e6).toFixed(2)}M`;
}

/** Dollars with two decimals; an em dash for a local model or when no cost was reported. */
export const formatTelemetryCost = (usd: number | null, local: boolean): string =>
  local || usd === null ? "—" : `$${usd.toFixed(2)}`;

/** Text safe in a GFM table cell: pipes escaped and line breaks flattened. */
const cell = (text: string): string => text.replace(/\\/g, "\\\\").replace(/\|/g, "\\|").replace(/\s*[\r\n]+\s*/g, " ");

/** The rows as a GitHub-flavoured Markdown table. */
export function telemetryTable(rows: TelemetryRow[]): string {
  const lines = ["| Model | tok/s | in | out | cost |", "|---|---|---|---|---|"];
  for (const row of rows) {
    const cells = [
      cell(row.label),
      formatSpeed(row.tokensPerSec),
      formatTokenCount(row.tokensIn),
      formatTokenCount(row.tokensOut),
      formatTelemetryCost(row.costUsd, row.local),
    ];
    lines.push(`| ${cells.join(" | ")} |`);
  }
  return lines.join("\n");
}
