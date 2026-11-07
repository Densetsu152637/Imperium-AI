// Per-call telemetry: tokens, time to first token, throughput, and cost for every model call.

import { formatCost, formatCount, totals, type SessionState } from "../sessions";

export function TelemetryPanel({ session }: { session: SessionState | null }) {
  if (!session || session.calls.length === 0) {
    return <p className="empty">Model calls appear here with tokens, speed, and cost.</p>;
  }
  const sum = totals(session.calls);
  const speeds = session.calls.flatMap((call) => (call.tokensPerSec === null ? [] : [call.tokensPerSec]));
  const average = speeds.length ? speeds.reduce((a, b) => a + b, 0) / speeds.length : null;
  return (
    <div className="telemetry">
      <dl className="stat-row">
        <div>
          <dt>Cost</dt>
          <dd>{formatCost(sum.costUsd)}</dd>
        </div>
        <div>
          <dt>Tokens</dt>
          <dd>{formatCount(sum.prompt + sum.completion)}</dd>
        </div>
        <div>
          <dt>Avg tok/s</dt>
          <dd>{average === null ? "—" : average.toFixed(1)}</dd>
        </div>
      </dl>
      <table>
        <thead>
          <tr>
            <th>Model</th>
            <th>In</th>
            <th>Out</th>
            <th>TTFT</th>
            <th>tok/s</th>
            <th>Cost</th>
          </tr>
        </thead>
        <tbody>
          {[...session.calls].reverse().map((call) => (
            <tr key={call.at + call.durationMs}>
              <td title={call.model}>{call.model.split(":").slice(1).join(":") || call.model}</td>
              <td>{formatCount(call.usage.prompt_tokens)}</td>
              <td>{formatCount(call.usage.completion_tokens)}</td>
              <td>{call.ttftMs === null ? "—" : `${call.ttftMs} ms`}</td>
              <td>{call.tokensPerSec === null ? "—" : call.tokensPerSec.toFixed(1)}</td>
              <td>{formatCost(call.costUsd)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
