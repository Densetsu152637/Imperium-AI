// What each agent's model read on its latest call: one bar from 0 to the configured maximum in
// three regions (the request by category, the pre-allocated buffer, what is unallocated), and a
// table listing every region and category in words, so nothing depends on colour alone.

import { useState } from "react";

import type { TokenCountMethod } from "@imperium/protocol";

import { Select } from "../components/Select";

import {
  ROOT,
  breakdownUsed,
  formatCount,
  formatTokens,
  type ContextBreakdown as Breakdown,
  type SessionState,
} from "../sessions/sessions";
import { contextRegions, type MaxSource } from "../sessions/contextRegions";
import { ModelName } from "../components/ModelName";

const METHODS: Record<TokenCountMethod, string> = {
  exact: "Exact, counted with the model's tokenizer",
  calibrated: "Calibrated to the provider's count",
  estimated: "Estimated; the provider reported no usage",
};

const MAX_NOTES: Record<MaxSource, string> = {
  configured: "",
  window: " The model table sets no context_window, so the maximum is the context window the model reports.",
  unknown: " The maximum is unknown (set context_window in the model table), so the bar spans what is allocated.",
};

/** Root first, then subagents in task order (t1, t2, …, t10). */
function agentOrder(a: string, b: string): number {
  if (a === ROOT || b === ROOT) return a === ROOT ? (b === ROOT ? 0 : -1) : 1;
  return a.localeCompare(b, undefined, { numeric: true });
}

function agentLabel(session: SessionState, agentId: string): string {
  if (agentId === ROOT) return "Root agent";
  const title = session.tasks[agentId]?.title ?? "";
  return title ? `${title.length > 48 ? `${title.slice(0, 47)}…` : title} (${agentId})` : agentId;
}

/** A share of the maximum (or, when it is unknown, of what is allocated) as a percentage. */
const percent = (tokens: number, of: number): string => (of > 0 ? `${((tokens * 100) / of).toFixed(1)}%` : "—");

export function ContextBreakdownView({ session }: { session: SessionState }) {
  const agents = Object.keys(session.breakdowns).sort(agentOrder);
  const [chosen, setChosen] = useState(ROOT);
  const agentId = session.breakdowns[chosen] ? chosen : agents[0];
  if (!agentId) return <p className="muted">No model call yet.</p>;
  const breakdown = session.breakdowns[agentId];

  return (
    <div className="context-breakdown">
      {agents.length > 1 && (
        <label className="context-agent">
          Agent
          <Select
            value={agentId}
            onChange={setChosen}
            aria-label="Agent"
            options={agents.map((id) => ({ value: id, label: agentLabel(session, id) }))}
          />
        </label>
      )}
      <BreakdownDetail breakdown={breakdown} />
    </div>
  );
}

function BreakdownDetail({ breakdown }: { breakdown: Breakdown }) {
  const regions = contextRegions({
    used: breakdownUsed(breakdown),
    configuredWindow: breakdown.configuredWindow,
    contextWindow: breakdown.contextWindow,
    reservedOutput: breakdown.reservedOutput,
    allocatedContext: breakdown.allocatedContext,
  });
  const { used, buffer, unallocated, allocated, max, overflow } = regions;
  const scale = Math.max(max, 1);
  const known = regions.maxSource !== "unknown";
  // A negative overhead (text the template dropped) has no width; the table still lists it.
  const drawn = breakdown.categories.filter((category) => category.tokens > 0);
  const summary = `${formatTokens(used)} used / ${formatTokens(allocated)} allocated${
    known ? ` / ${formatTokens(max)} max` : ""
  } tokens`;
  const bufferWhy = [
    breakdown.reservedOutput ? `${formatCount(breakdown.reservedOutput)} reserved for the reply` : null,
    breakdown.allocatedContext ? `${formatCount(breakdown.allocatedContext)} held by the local server` : null,
  ]
    .filter(Boolean)
    .join("; ");

  return (
    <>
      <p className="context-use">
        <strong>{summary}</strong>
        {known && (
          <span className="muted">
            {" · "}
            {percent(used, max)} used, {percent(allocated, max)} allocated
          </span>
        )}
        <span className="muted">
          {" · "}
          <ModelName name={breakdown.model} />
        </span>
      </p>
      <div
        className="context-bar"
        role="img"
        aria-label={`${summary}; ${formatCount(buffer)} tokens pre-allocated buffer, ${formatCount(unallocated)} unallocated; the table below lists each region`}
      >
        {drawn.map((category) => (
          <span
            key={category.key}
            className={`context-segment ctx-${category.key}`}
            style={{ flexGrow: Math.min(category.tokens, scale) }}
            title={`${category.label}: ${formatCount(category.tokens)} tokens`}
          />
        ))}
        {buffer > 0 && (
          <span
            className="context-segment ctx-buffer"
            style={{ flexGrow: buffer }}
            title={`Pre-allocated buffer: ${formatCount(buffer)} tokens`}
          />
        )}
        {unallocated > 0 && (
          <span
            className="context-segment ctx-free"
            style={{ flexGrow: unallocated }}
            title={`Unallocated: ${formatCount(unallocated)} tokens`}
          />
        )}
      </div>
      <table className="table compact context-table">
        <thead>
          <tr>
            <th scope="col">Region and category</th>
            <th scope="col" className="num">Tokens</th>
            <th scope="col" className="num">{known ? "Of maximum" : "Of allocated"}</th>
          </tr>
        </thead>
        <tbody>
          {breakdown.categories.map((category) => (
            <tr key={category.key}>
              <th scope="row">
                <span className={`context-swatch ctx-${category.key}`} aria-hidden="true" />
                {category.label}
              </th>
              <td className="num">{formatCount(category.tokens)}</td>
              <td className="num">{percent(category.tokens, scale)}</td>
            </tr>
          ))}
        </tbody>
        <tfoot>
          <tr>
            <th scope="row">Used (in the request)</th>
            <td className="num">{formatCount(used)}</td>
            <td className="num">{percent(used, scale)}</td>
          </tr>
          <tr>
            <th scope="row">
              <span className="context-swatch ctx-buffer" aria-hidden="true" />
              Pre-allocated buffer
            </th>
            <td className="num">{formatCount(buffer)}</td>
            <td className="num">{percent(buffer, scale)}</td>
          </tr>
          <tr>
            <th scope="row">Allocated (used + buffer)</th>
            <td className="num">{formatCount(allocated)}</td>
            <td className="num">{percent(allocated, scale)}</td>
          </tr>
          {known && (
            <tr>
              <th scope="row">
                <span className="context-swatch ctx-free" aria-hidden="true" />
                Unallocated
              </th>
              <td className="num">{formatCount(unallocated)}</td>
              <td className="num">{percent(unallocated, scale)}</td>
            </tr>
          )}
          <tr>
            <th scope="row">{known ? "Maximum" : "Maximum (unknown)"}</th>
            <td className="num">{known ? formatCount(max) : "—"}</td>
            <td className="num">{known ? percent(max, scale) : "—"}</td>
          </tr>
        </tfoot>
      </table>
      <p className="context-method muted">
        {METHODS[breakdown.method]}. {formatCount(breakdown.cachedTokens)} prompt tokens served from cache.
        {bufferWhy && ` The buffer is ${bufferWhy}.`}
        {overflow > 0 && ` The request is ${formatCount(overflow)} tokens over the maximum.`}
        {MAX_NOTES[regions.maxSource]}
      </p>
    </>
  );
}
