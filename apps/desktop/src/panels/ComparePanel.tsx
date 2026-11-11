// Comparing models (roadmap item 7). `CompareForm` starts a comparison from a session: pick two to
// four models, write the message; the daemon branches the session once per model and sends each the
// same message. `ComparePanel` then shows the replies side by side, one column per model with its
// tokens, cost and time, and a "Keep this one" button that keeps a branch and archives the others.

import { ArrowLeft, Check } from "lucide-react";
import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { LocalModelRow, ModelRow } from "@imperium/protocol";

import {
  COMPARE_MAX,
  COMPARE_MIN,
  compareProblem,
  standouts,
  togglePicked,
  type BranchRun,
} from "../branches";
import { ModelName } from "../components/ModelName";
import { formatCost, formatTokens } from "../sessions";
import { formatElapsed } from "../agentView";

/** The models a comparison can use: enabled table rows, then local models found in the model folders. */
function choices(models: ModelRow[], local: LocalModelRow[]): { spec: string; label: string }[] {
  const rows = models.filter((row) => row.enabled && row.endpoint_configured).map((row) => ({ spec: row.id, label: row.alias ?? row.id }));
  const known = new Set(models.map((row) => row.spec));
  const found = local
    .map((row) => ({ spec: `${row.runtime}:${row.id}`, label: row.id }))
    .filter((row) => !known.has(row.spec) && !rows.some((r) => r.spec === row.spec));
  return [...rows, ...found];
}

export function CompareForm({
  models,
  localModels,
  onStart,
  onClose,
}: {
  models: ModelRow[];
  localModels: LocalModelRow[];
  /** Starts the comparison; rejects with the daemon's reason. */
  onStart: (models: string[], message: string) => Promise<void>;
  onClose: () => void;
}) {
  const [picked, setPicked] = useState<string[]>([]);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const available = choices(models, localModels);
  const problem = compareProblem(picked, message);
  const start = () => {
    if (problem || busy) return;
    setBusy(true);
    setError(null);
    onStart(picked, message.trim())
      .then(onClose)
      .catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))
      .finally(() => setBusy(false));
  };
  return (
    <form
      className="compare-form"
      onSubmit={(event) => {
        event.preventDefault();
        start();
      }}
    >
      <h3>Compare models</h3>
      <p className="muted">
        Sends one message to a copy of this conversation on each model ({COMPARE_MIN} to {COMPARE_MAX}), then shows the replies
        side by side.
      </p>
      {available.length === 0 ? (
        <p className="muted">No models are set up yet.</p>
      ) : (
        <ul className="compare-models" aria-label="Models to compare">
          {available.map((choice) => {
            const on = picked.includes(choice.spec);
            return (
              <li key={choice.spec}>
                <label className={on ? "on" : undefined}>
                  <input
                    type="checkbox"
                    checked={on}
                    disabled={!on && picked.length >= COMPARE_MAX}
                    onChange={() => setPicked((current) => togglePicked(current, choice.spec))}
                  />
                  <ModelName name={choice.spec} />
                </label>
              </li>
            );
          })}
        </ul>
      )}
      <textarea
        aria-label="Message for every model"
        rows={3}
        placeholder="The message to send to every model"
        value={message}
        onChange={(event) => setMessage(event.target.value)}
      />
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <div className="compare-form-actions">
        <button type="submit" className="primary" disabled={!!problem || busy} title={problem ?? undefined}>
          {busy ? "Starting…" : picked.length > 0 ? `Compare ${picked.length} models` : "Compare"}
        </button>
        <button type="button" className="ghost" onClick={onClose}>
          Cancel
        </button>
      </div>
    </form>
  );
}

export type CompareColumn = BranchRun & { title: string };

export function ComparePanel({
  message,
  columns,
  open,
  onKeep,
  onOpen,
  onBack,
}: {
  /** What every model was sent. */
  message: string;
  columns: CompareColumn[];
  /** Whether more than one branch is left, so one can still be kept. */
  open: boolean;
  /** Keeps a branch and archives the others (`session.compare_pick`). */
  onKeep: (sessionId: string) => void;
  /** Opens a branch as an ordinary session. */
  onOpen: (sessionId: string) => void;
  onBack: () => void;
}) {
  const best = standouts(columns);
  return (
    <div className="compare">
      <header className="compare-head">
        <button type="button" className="ghost back" onClick={onBack}>
          <ArrowLeft aria-hidden="true" /> Back to the session
        </button>
        <p className="compare-message" title={message}>
          {message}
        </p>
      </header>
      <div className="compare-columns" style={{ gridTemplateColumns: `repeat(${Math.max(columns.length, 1)}, minmax(0, 1fr))` }}>
        {columns.map((column) => (
          <section key={column.sessionId} className={`compare-column${column.archived ? " archived" : ""}`} aria-label={column.title}>
            <header>
              <h3>{column.model ? <ModelName name={column.model} /> : "No model"}</h3>
              <dl className="compare-stats">
                <div title="Tokens read and written">
                  <dt>Tokens</dt>
                  <dd>
                    {column.promptTokens === null
                      ? "—"
                      : `${formatTokens(column.promptTokens)} in / ${formatTokens(column.completionTokens ?? 0)} out`}
                  </dd>
                </div>
                <div title="What the turn cost">
                  <dt>Cost</dt>
                  <dd className={best.cheapest === column.sessionId ? "standout" : undefined}>{formatCost(column.costUsd)}</dd>
                </div>
                <div title="Time spent in model calls">
                  <dt>Time</dt>
                  <dd className={best.fastest === column.sessionId ? "standout" : undefined}>
                    {column.durationMs === null ? "—" : formatElapsed(column.durationMs)}
                  </dd>
                </div>
              </dl>
            </header>
            <div className="compare-reply prose">
              {column.state === "failed" ? (
                <p className="error">{column.error ?? "The turn failed."}</p>
              ) : column.text ? (
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{column.text}</ReactMarkdown>
              ) : (
                <p className="muted">{column.state === "done" ? "No reply." : "Working…"}</p>
              )}
            </div>
            <footer>
              {column.archived ? (
                <span className="muted">Archived</span>
              ) : (
                <>
                  <button
                    type="button"
                    className="primary"
                    disabled={!open || column.state === "running" || column.state === "waiting"}
                    onClick={() => onKeep(column.sessionId)}
                    title="Keep this branch and archive the others"
                  >
                    <Check aria-hidden="true" /> Keep this one
                  </button>
                  <button type="button" className="ghost" onClick={() => onOpen(column.sessionId)}>
                    Open
                  </button>
                </>
              )}
            </footer>
          </section>
        ))}
      </div>
    </div>
  );
}
