// The main session panel: title and status, environment header, usage-limit controls,
// conversation, composer, and the context, outputs, and cost sections underneath.

import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { PermissionDecision, Plan, ResumePolicy } from "@imperium/protocol";

import { ProjectIcon, projectName, type ProjectIcons } from "../projectIcons";
import { formatCost, formatCount, sessionTitle, totals, type Entry, type SessionState } from "../sessions";
import { StatusBadge } from "./StatusBadge";

type Props = {
  session: SessionState | null;
  icons: ProjectIcons;
  onSend: (text: string) => void;
  onRespond: (requestId: number, decision: PermissionDecision) => void;
  /** Resolves once the daemon accepted the policy. */
  onSetResumePolicy: (policy: ResumePolicy) => Promise<void>;
  onResumeNow: () => void;
};

export function SessionPanel({ session, icons, onSend, onRespond, onSetResumePolicy, onResumeNow }: Props) {
  const [draft, setDraft] = useState("");
  const [rendered, setRendered] = useState(true);
  const end = useRef<HTMLDivElement>(null);
  const entryCount = session?.entries.length ?? 0;
  const lastLength = session?.entries.at(-1) && "text" in session.entries.at(-1)! ? (session.entries.at(-1) as { text: string }).text.length : 0;

  // Block body: newer browsers return a promise from scrollIntoView, which an effect must not return.
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" });
  }, [entryCount, lastLength]);

  if (!session) {
    return <p className="empty">Open a project folder and start a session to begin.</p>;
  }
  const sum = totals(session.calls);
  const outputs = session.entries.filter((entry): entry is Extract<Entry, { kind: "tool" }> => entry.kind === "tool");
  const send = () => {
    const text = draft.trim();
    if (!text) return;
    onSend(text);
    setDraft("");
  };

  return (
    <div className="session">
      <div className="session-head">
        <ProjectIcon choice={icons.iconFor(session.summary.project_dir)} />
        <h3 className="session-title" title={sessionTitle(session)}>
          {sessionTitle(session)}
        </h3>
        <StatusBadge session={session} />
        {session.status === "paused" && (
          <button className="resume-now" onClick={onResumeNow}>
            Resume now
          </button>
        )}
      </div>

      <div className="session-env">
        <span title={session.summary.project_dir}>{projectName(session.summary.project_dir)}</span>
        <span title="Model">{session.summary.model}</span>
        <span title="Isolation tier">{session.summary.isolation}</span>
        <span title="Session cost">{formatCost(sum.costUsd)}</span>
        <label className="toggle">
          <input type="checkbox" checked={rendered} onChange={(event) => setRendered(event.target.checked)} />
          Markdown
        </label>
      </div>

      <ResumeControls
        key={session.summary.session_id}
        policy={session.summary.resume_policy}
        onSave={onSetResumePolicy}
      />

      {session.plan && <PlanView plan={session.plan} />}

      <div className="conversation" aria-live="polite">
        {session.entries.map((entry, index) => (
          <EntryView key={index} entry={entry} rendered={rendered} onRespond={onRespond} />
        ))}
        {session.busy && <p className="working">Working…</p>}
        <div ref={end} />
      </div>

      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          send();
        }}
      >
        <textarea
          value={draft}
          placeholder="Message the agent (Enter to send, Shift+Enter for a new line)"
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              send();
            }
          }}
          rows={3}
        />
        <button type="submit" disabled={!draft.trim()}>
          Send
        </button>
      </form>

      <div className="session-sections">
        <section>
          <h3>Context</h3>
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
            <span className="tool-state">{entry.ok === null ? "…" : entry.ok ? "✓" : "✗"}</span> {entry.summary}
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
              <button onClick={() => onRespond(entry.requestId, "allow")}>Allow</button>
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
  }
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
          <button type="submit" disabled={!dirty || !valid || saving}>
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
