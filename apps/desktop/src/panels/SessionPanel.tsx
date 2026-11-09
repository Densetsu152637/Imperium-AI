// The main session panel: a header with title and status, environment (with the model picker),
// usage-limit controls, and the folded context and outputs sections; then the conversation and
// the composer.

import { ArrowUp, CornerDownRight, ListPlus } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type {
  ModelRow,
  PermissionDecision,
  Plan,
  QueuedMessage,
  ResumePolicy,
  SendMode,
  SessionControlResult,
} from "@imperium/protocol";

import { ProjectIcon, projectName, type ProjectIcons } from "../projectIcons";
import {
  ROOT,
  describeUse,
  formatCost,
  formatCount,
  formatTokens,
  sessionTitle,
  totals,
  type Entry,
  type SessionState,
} from "../sessions";
import { ToolState } from "../components/ToolState";
import type { DaemonClient } from "../daemon/client";
import { ContextBreakdownView } from "./ContextBreakdown";
import { ModelSuggestionCard } from "./ModelSuggestionCard";
import { StatusBadge } from "./StatusBadge";
import { useSettings } from "../settings";

type Props = {
  session: SessionState | null;
  icons: ProjectIcons;
  /** Sends a message: `queue` starts a turn, or waits while one runs; `steer` joins the running turn. */
  onSend: (text: string, mode: SendMode) => void;
  /** Edits a queued message, or removes it when `text` is null. */
  onChangeQueued: (id: string, text: string | null) => void;
  onRespond: (requestId: number, decision: PermissionDecision) => void;
  /** Resolves once the daemon accepted the policy. */
  onSetResumePolicy: (policy: ResumePolicy) => Promise<void>;
  onResumeNow: () => void;
  /** The model table (`models.list`), offered in the model picker. */
  models: ModelRow[];
  /** Switches the session's model; resolves with the daemon's answer, or null when it failed. */
  onSetModel: (model: string) => Promise<SessionControlResult | null>;
  /** For answering model download suggestions and following their downloads. */
  client?: DaemonClient | null;
  /** Resumes an archived session. */
  onUnarchive?: () => void;
};

export function SessionPanel({
  session,
  icons,
  onSend,
  onChangeQueued,
  onRespond,
  onSetResumePolicy,
  onResumeNow,
  models,
  onSetModel,
  client = null,
  onUnarchive,
}: Props) {
  const settings = useSettings();
  const [draft, setDraft] = useState("");
  const [rendered, setRendered] = useState(settings.renderMarkdown);
  useEffect(() => setRendered(settings.renderMarkdown), [settings.renderMarkdown]);
  const end = useRef<HTMLDivElement>(null);
  const entryCount = session?.entries.length ?? 0;
  const lastLength = session?.entries.at(-1) && "text" in session.entries.at(-1)! ? (session.entries.at(-1) as { text: string }).text.length : 0;

  // Block body: newer browsers return a promise from scrollIntoView, which an effect must not return.
  useEffect(() => {
    if (settings.autoScroll) end.current?.scrollIntoView({ block: "end" });
  }, [entryCount, lastLength, settings.autoScroll]);

  if (!session) {
    return (
      <div className="session-empty">
        <p className="empty">Open a project folder and start a session to begin.</p>
      </div>
    );
  }
  const sum = totals(session.calls);
  const rootContext = session.breakdowns[ROOT];
  const outputs = session.entries.filter((entry): entry is Extract<Entry, { kind: "tool" }> => entry.kind === "tool");
  // While the agent works the composer stays open: Enter queues the message for after the turn,
  // and Steer (Ctrl+Shift+Enter) adds it to the running turn.
  const send = (mode: SendMode = "queue") => {
    const text = draft.trim();
    if (!text) return;
    onSend(text, mode);
    setDraft("");
  };

  return (
    <div className="session">
      <header className="session-top">
        <div className="session-head">
          <ProjectIcon choice={icons.iconFor(session.summary.project_dir)} />
          <h3 className="session-title" title={sessionTitle(session)}>
            {sessionTitle(session)}
          </h3>
          <StatusBadge session={session} />
          {session.status === "paused" && (
            <button className="resume-now primary" onClick={onResumeNow}>
              Resume now
            </button>
          )}
        </div>

        <div className="session-env">
          <span title={session.summary.project_dir}>{projectName(session.summary.project_dir)}</span>
          <ModelPicker
            key={session.summary.session_id}
            current={session.summary.model}
            models={models}
            busy={session.busy}
            onSwitch={onSetModel}
          />
          <span title="Isolation tier">{session.summary.isolation}</span>
          <span title="Session cost">{formatCost(sum.costUsd)}</span>
          <label className="toggle">
            <input type="checkbox" checked={rendered} onChange={(event) => setRendered(event.target.checked)} />
            Markdown
          </label>
        </div>

        {/* Settings and statistics fold away so the conversation keeps the room. */}
        <div className="session-tools">
          <ResumeControls
            key={session.summary.session_id}
            policy={session.summary.resume_policy}
            onSave={onSetResumePolicy}
          />
          <details className="session-details">
            <summary>
              Context and outputs{" "}
              <span className="muted">
                · {rootContext ? describeUse(rootContext) : `${formatCount(sum.prompt)} prompt tokens`} ·{" "}
                {session.calls.length} model calls
              </span>
            </summary>
            <div className="session-sections">
              <section className="context-window-section">
                <h3>Context window</h3>
                <ContextBreakdownView session={session} />
              </section>
              <section>
                <h3>Session totals</h3>
                <dl>
                  <dt>Prompt tokens</dt>
                  <dd>{formatCount(sum.prompt)}</dd>
                  <dt>Cached</dt>
                  <dd>{formatCount(sum.cached)}</dd>
                  <dt>Completion</dt>
                  <dd>{formatCount(sum.completion)}</dd>
                  <dt>Model calls</dt>
                  <dd>{session.calls.length}</dd>
                </dl>
              </section>
              <section>
                <h3>Outputs</h3>
                {outputs.length === 0 ? (
                  <p className="muted">No tool output yet.</p>
                ) : (
                  <ul className="outputs">
                    {outputs.slice(-8).map((tool, index) => (
                      <li key={index} className={tool.ok === false ? "failed" : undefined} title={tool.output}>
                        {tool.summary}
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            </div>
          </details>
        </div>
      </header>

      {session.plan && <PlanView plan={session.plan} />}

      <div className="conversation" aria-live="polite">
        {session.entries.map((entry, index) => (
          entry.kind === "suggestion" ? (
            <ModelSuggestionCard
              key={index}
              entry={entry}
              sessionId={session.summary.session_id}
              client={client}
              onSetModel={onSetModel}
            />
          ) : (
            <EntryView key={index} entry={entry} rendered={rendered} onRespond={onRespond} />
          )
        ))}
        {session.busy && <p className="working">Working…</p>}
        <div ref={end} />
      </div>

      {session.summary.archived && (
        <div className="archived-note" role="status">
          <span>This session is archived: it is not running and keeps its history. Unarchive it to continue.</span>
          {onUnarchive && (
            <button className="primary" onClick={onUnarchive}>
              Unarchive
            </button>
          )}
        </div>
      )}

      {session.summary.model === null && !session.summary.archived && (
        <p className="needs-model" role="status">
          This session has no model yet. Pick one in the model list above (or set a default under Settings → Models);
          {session.status === "needs_model"
            ? " your messages are waiting and run as soon as it has one."
            : " messages you send wait until then."}
        </p>
      )}

      {session.queue.length > 0 && <QueueView queue={session.queue} onChange={onChangeQueued} />}

      <form
        className="composer"
        hidden={session.summary.archived}
        onSubmit={(event) => {
          event.preventDefault();
          send();
        }}
      >
        <textarea
          value={draft}
          placeholder={composerHint(settings.sendKey === "enter", session.busy)}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            const modified = event.ctrlKey || event.metaKey;
            if (modified && event.shiftKey) {
              event.preventDefault();
              send("steer");
              return;
            }
            const sends = settings.sendKey === "enter" ? !event.shiftKey && !modified : modified;
            if (sends) {
              event.preventDefault();
              send();
            }
          }}
          rows={1}
        />
        {session.busy && (
          <button
            type="button"
            className="steer"
            disabled={!draft.trim()}
            onClick={() => send("steer")}
            title="Add this message to the running turn; the agent reads it after its current step (Ctrl+Shift+Enter)"
          >
            <CornerDownRight aria-hidden="true" /> Steer
          </button>
        )}
        <button
          type="submit"
          className="send"
          disabled={!draft.trim()}
          aria-label={session.busy ? "Queue" : "Send"}
          title={session.busy ? "Queue for after this turn (Enter)" : "Send (Enter)"}
        >
          {session.busy ? <ListPlus aria-hidden="true" /> : <ArrowUp aria-hidden="true" />}
        </button>
      </form>
    </div>
  );
}

function EntryView({
  entry,
  rendered,
  onRespond,
}: {
  entry: Entry;
  rendered: boolean;
  onRespond: Props["onRespond"];
}) {
  switch (entry.kind) {
    case "user":
      return <div className="message user prose">{entry.text}</div>;
    case "steer":
      return (
        <div className="message user steer prose" title="Steering: joined the running turn">
          <span className="steer-label">Steering</span>
          {entry.text}
        </div>
      );
    case "assistant":
      return (
        <div className="message assistant prose">
          {rendered ? <ReactMarkdown remarkPlugins={[remarkGfm]}>{entry.text}</ReactMarkdown> : <pre>{entry.text}</pre>}
        </div>
      );
    case "tool":
      return (
        <details className={`tool ${entry.ok === false ? "failed" : ""}`}>
          <summary>
            <span className="tool-state">
              <ToolState ok={entry.ok} />
            </span>{" "}
            {entry.summary}
          </summary>
          {entry.output && <pre>{entry.output}</pre>}
        </details>
      );
    case "task":
      return (
        <div className="task-line">
          → <strong>{entry.taskId}</strong> on {entry.model}: {entry.brief.split("\n")[0]}
        </div>
      );
    case "permission":
      return (
        <div className="permission" role="group" aria-label="Permission request">
          <span>
            {entry.agentId !== "root" && <span className="badge running">{entry.agentId}</span>} Allow{" "}
            <strong>{entry.summary}</strong>?
          </span>
          {entry.answered ? (
            <span className="muted">Answered</span>
          ) : (
            <span className="buttons">
              <button className="primary" onClick={() => onRespond(entry.requestId, "allow")}>
                Allow
              </button>
              <button onClick={() => onRespond(entry.requestId, "allow_for_session")}>Always this session</button>
              <button className="danger" onClick={() => onRespond(entry.requestId, "deny")}>
                Deny
              </button>
            </span>
          )}
        </div>
      );
    case "turn":
      return (
        <p className="turn-summary">
          {formatCount(entry.usage.prompt_tokens)} in ({formatCount(entry.usage.cached_tokens)} cached) ·{" "}
          {formatCount(entry.usage.completion_tokens)} out · {formatCost(entry.costUsd)} · {entry.steps} model calls
        </p>
      );
    case "error":
      return <p className="error">Turn failed: {entry.message}</p>;
    case "notice":
      return <p className="notice">{entry.message}</p>;
    case "suggestion":
      return null;
  }
}

/** The composer's placeholder: what Enter does now, and how to steer while a turn runs. */
function composerHint(enterSends: boolean, busy: boolean): string {
  const key = enterSends ? "Enter" : "Ctrl+Enter";
  const newline = enterSends ? "Shift+Enter" : "Enter";
  return busy
    ? `${key} queues for after this turn · Ctrl+Shift+Enter steers the running turn`
    : `Message the agent (${key} to send, ${newline} for a new line)`;
}

/** Messages waiting for the agent, above the composer; each can be edited or removed until delivered. */
function QueueView({ queue, onChange }: { queue: QueuedMessage[]; onChange: Props["onChangeQueued"] }) {
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);
  const save = () => {
    if (!editing) return;
    const text = editing.text.trim();
    if (text) onChange(editing.id, text);
    setEditing(null);
  };
  return (
    <ol className="queue" aria-label="Queued messages">
      {queue.map((message) => (
        <li key={message.id} className={message.steer ? "steer" : undefined}>
          <span className="queue-kind">{message.steer ? "Steering" : "Queued"}</span>
          {editing?.id === message.id ? (
            <input
              aria-label="Edit queued message"
              autoFocus
              value={editing.text}
              onChange={(event) => setEditing({ id: message.id, text: event.target.value })}
              onKeyDown={(event) => {
                if (event.key === "Enter") save();
                if (event.key === "Escape") setEditing(null);
              }}
              onBlur={save}
            />
          ) : (
            <span className="queue-text" title={message.text}>
              {message.text}
            </span>
          )}
          <span className="buttons">
            {editing?.id !== message.id && (
              <button onClick={() => setEditing({ id: message.id, text: message.text })}>Edit</button>
            )}
            <button className="danger" onClick={() => onChange(message.id, null)} aria-label="Remove queued message">
              Remove
            </button>
          </span>
        </li>
      ))}
    </ol>
  );
}

/**
 * The session's model, switchable to any row of the model table between turns. A smaller
 * context window makes the daemon summarise older turns first; the result is noted beside it.
 */
function ModelPicker({
  current,
  models,
  busy,
  onSwitch,
}: {
  /** `null` while the session has no model yet. */
  current: string | null;
  models: ModelRow[];
  busy: boolean;
  onSwitch: Props["onSetModel"];
}) {
  const [switching, setSwitching] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (models.length === 0) return <span title="Model">{current ?? "no model"}</span>;
  const listed = current === null || models.some((row) => row.spec === current);
  const switchTo = (spec: string) => {
    if (spec === current) return;
    setSwitching(true);
    setNote(null);
    onSwitch(spec)
      .then((result) => {
        const compaction = result?.context?.compaction;
        if (compaction) {
          setNote(
            `compacted ${formatTokens(compaction.before_tokens)} → ${formatTokens(compaction.after_tokens)} tokens`,
          );
        }
      })
      .finally(() => setSwitching(false));
  };
  return (
    <span className="model-picker">
      <select
        aria-label="Model"
        title={
          busy
            ? "Switch models when the turn finishes"
            : "Switch the session's model; older turns are summarised if its context window is smaller"
        }
        value={current ?? ""}
        disabled={busy || switching}
        onChange={(event) => switchTo(event.target.value)}
      >
        {current === null && (
          <option value="" disabled>
            Pick a model…
          </option>
        )}
        {!listed && current !== null && <option value={current}>{current}</option>}
        {models.map((row) => (
          <option key={row.id} value={row.spec} disabled={!row.endpoint_configured}>
            {row.id === row.spec ? row.spec : `${row.id} · ${row.spec}`}
          </option>
        ))}
      </select>
      {switching && <span className="muted">Switching…</span>}
      {note && !switching && <span className="model-note">{note}</span>}
    </span>
  );
}

const RESUME_MODES: Record<ResumePolicy["mode"], string> = {
  wait_for_reset: "Wait for the provider's reset",
  at_time: "Resume at a set time",
  retry_every: "Retry every N minutes",
  stop: "Stop and tell me",
};

/** One line saying what the session does when usage runs out. */
function describePolicy(policy: ResumePolicy): string {
  switch (policy.mode) {
    case "wait_for_reset":
      return "wait for reset";
    case "at_time":
      return `resume at ${policy.at_time ?? "—"}`;
    case "retry_every":
      return `retry every ${policy.retry_every_minutes} min`;
    case "stop":
      return "stop";
  }
}

/** What the session does when a provider says usage has run out; edited as a draft, then saved. */
function ResumeControls({ policy, onSave }: { policy: ResumePolicy; onSave: Props["onSetResumePolicy"] }) {
  const [draft, setDraft] = useState(policy);
  const [saving, setSaving] = useState(false);
  // A policy changed elsewhere (another window, the daemon) replaces an unedited draft.
  const [saved, setSaved] = useState(policy);
  if (policy !== saved) {
    setSaved(policy);
    setDraft(policy);
  }
  const dirty = JSON.stringify(draft) !== JSON.stringify(policy);
  const valid = draft.mode !== "at_time" || /^\d{2}:\d{2}$/.test(draft.at_time ?? "");
  const set = (change: Partial<ResumePolicy>) => setDraft((current) => ({ ...current, ...change }));
  const retries = draft.mode !== "stop";

  return (
    <details className="usage-limits">
      <summary>
        Usage limits <span className="muted">· {describePolicy(policy)}</span>
      </summary>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!dirty || !valid) return;
          setSaving(true);
          onSave(draft).finally(() => setSaving(false));
        }}
      >
        <label>
          When usage runs out:
          <select value={draft.mode} onChange={(event) => set({ mode: event.target.value as ResumePolicy["mode"] })}>
            {Object.entries(RESUME_MODES).map(([mode, label]) => (
              <option key={mode} value={mode}>
                {label}
              </option>
            ))}
          </select>
        </label>
        {draft.mode === "at_time" && (
          <label>
            At
            <input
              type="time"
              required
              value={draft.at_time ?? ""}
              onChange={(event) => set({ at_time: event.target.value || null })}
            />
          </label>
        )}
        {retries && (
          <label title={draft.mode === "retry_every" ? undefined : "How often to retry when no reset time is known"}>
            {draft.mode === "retry_every" ? "Every" : "Retry every"}
            <input
              type="number"
              min={1}
              max={1440}
              value={draft.retry_every_minutes}
              onChange={(event) => set({ retry_every_minutes: Math.max(1, Math.round(Number(event.target.value) || 1)) })}
            />
            min
          </label>
        )}
        {retries && (
          <label>
            Give up after
            <input
              type="number"
              min={1}
              max={168}
              value={draft.max_wait_hours}
              onChange={(event) => set({ max_wait_hours: Math.max(1, Math.round(Number(event.target.value) || 1)) })}
            />
            h
          </label>
        )}
        <span className="usage-actions">
          {dirty && (
            <button type="button" onClick={() => setDraft(policy)}>
              Revert
            </button>
          )}
          <button type="submit" className="primary" disabled={!dirty || !valid || saving}>
            {saving ? "Saving…" : "Save"}
          </button>
        </span>
      </form>
    </details>
  );
}

const STEP_MARK: Record<Plan["steps"][number]["status"], string> = {
  pending: "○",
  in_progress: "▸",
  done: "✓",
  blocked: "!",
  skipped: "–",
};

function PlanView({ plan }: { plan: Plan }) {
  const done = plan.steps.filter((step) => step.status === "done").length;
  return (
    <details className="plan" open>
      <summary>
        Plan{plan.goal ? `: ${plan.goal}` : ""}{" "}
        <span className="muted">
          {done}/{plan.steps.length}
        </span>
      </summary>
      <ol>
        {plan.steps.map((step, index) => (
          <li key={index} className={`step ${step.status}`}>
            <span className="mark">{STEP_MARK[step.status]}</span> {step.title}
          </li>
        ))}
      </ol>
    </details>
  );
}
