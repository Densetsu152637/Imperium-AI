// What each agent's model read on its latest call: a bar coloured by category against the context
// window, and a table listing every category in words, so nothing depends on colour alone.

import { useState } from "react";

import type { TokenCountMethod } from "@imperium/protocol";

import { Select } from "../components/Select";

import {
  ROOT,
  breakdownUsed,
  describeUse,
  formatCount,
  type ContextBreakdown as Breakdown,
  type SessionState,
} from "../sessions";
import { ModelName } from "../components/ModelName";

const METHODS: Record<TokenCountMethod, string> = {
  exact: "Exact, counted with the model's tokenizer",
  calibrated: "Calibrated to the provider's count",
  estimated: "Estimated; the provider reported no usage",
};

/** Root first, then subagents in task order (t1, t2, …, t10). */
function agentOrder(a: string, b: string): number {
  if (a === ROOT || b === ROOT) return a === ROOT ? (b === ROOT ? 0 : -1) : 1;
  return a.localeCompare(b, undefined, { numeric: true });
}

function agentLabel(session: SessionState, agentId: string): string {
  if (agentId === ROOT) return "Root agent";
  const brief = session.tasks[agentId]?.brief.split("\n")[0] ?? "";
  return brief ? `${agentId}: ${brief.length > 48 ? `${brief.slice(0, 47)}…` : brief}` : agentId;
}

/** A share of the window (or, when it is unknown, of the request) as a percentage. */
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
  const used = breakdownUsed(breakdown);
  const limit = breakdown.contextWindow;
  // Without a known window the bar and shares describe the request itself.
  const scale = limit ?? Math.max(used, 1);
  const free = limit === null ? null : Math.max(limit - used, 0);
  // A negative overhead (text the template dropped) has no width; the table still lists it.
  const drawn = breakdown.categories.filter((category) => category.tokens > 0);

  return (
    <>
      <p className="context-use">
        <strong>{describeUse(breakdown)}</strong>
        {limit !== null && <span className="muted"> · {percent(used, limit)} of the window</span>}
        <span className="muted">
          {" · "}
          <ModelName name={breakdown.model} />
        </span>
      </p>
      <div
        className="context-bar"
        role="img"
        aria-label={`${describeUse(breakdown)}; the table below lists each category`}
      >
        {drawn.map((category) => (
          <span
            key={category.key}
            className={`context-segment ctx-${category.key}`}
            style={{ flexGrow: Math.min(category.tokens, scale) }}
            title={`${category.label}: ${formatCount(category.tokens)} tokens`}
          />
        ))}
        {free !== null && free > 0 && (
          <span
            className="context-segment ctx-free"
            style={{ flexGrow: free }}
            title={`Free space: ${formatCount(free)} tokens`}
          />
        )}
      </div>
      <table className="context-table">
        <thead>
          <tr>
            <th scope="col">Category</th>
            <th scope="col">Tokens</th>
            <th scope="col">{limit === null ? "Of request" : "Of window"}</th>
          </tr>
        </thead>
        <tbody>
          {breakdown.categories.map((category) => (
            <tr key={category.key}>
              <th scope="row">
                <span className={`context-swatch ctx-${category.key}`} aria-hidden="true" />
                {category.label}
              </th>
              <td>{formatCount(category.tokens)}</td>
              <td>{percent(category.tokens, scale)}</td>
            </tr>
          ))}
          {free !== null && (
            <tr>
              <th scope="row">
                <span className="context-swatch ctx-free" aria-hidden="true" />
                Free space
              </th>
              <td>{formatCount(free)}</td>
              <td>{percent(free, scale)}</td>
            </tr>
          )}
        </tbody>
        <tfoot>
          <tr>
            <th scope="row">In the request</th>
            <td>{formatCount(used)}</td>
            <td>{percent(used, scale)}</td>
          </tr>
        </tfoot>
      </table>
      <p className="context-method muted">
        {METHODS[breakdown.method]}. {formatCount(breakdown.cachedTokens)} prompt tokens served from cache.
        {limit === null && " The context window is unknown; set context_window in the model table."}
      </p>
    </>
  );
}
