// The main session panel: a header with title and status, environment, usage-limit controls,
// and the folded context and outputs sections; then the conversation and the composer.

import { useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type { PermissionDecision, Plan, Question, ResumePolicy } from "@imperium/protocol";

import { ProjectIcon, projectName, type ProjectIcons } from "../projectIcons";
import { ROOT, describeUse, formatCost, formatCount, sessionTitle, totals, type Entry, type SessionState } from "../sessions";
import { ContextBreakdownView } from "./ContextBreakdown";
import { StatusBadge } from "./StatusBadge";

type Props = {
  session: SessionState | null;
  icons: ProjectIcons;
  onSend: (text: string) => void;
  onRespond: (requestId: number, decision: PermissionDecision) => void;
  /** One answer per question, in order. */
  onAnswer: (requestId: number, answers: string[]) => void;
  /** Resolves once the daemon accepted the policy. */
  onSetResumePolicy: (policy: ResumePolicy) => Promise<void>;
  onResumeNow: () => void;
};

export function SessionPanel({ session, icons, onSend, onRespond, onAnswer, onSetResumePolicy, onResumeNow }: Props) {
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
    return (
      <div className="session-empty">
        <p className="empty">Open a project folder and start a session to begin.</p>
      </div>
    );
  }
  const sum = totals(session.calls);
  const rootContext = session.breakdowns[ROOT];
  const outputs = session.entries.filter((entry): entry is Extract<Entry, { kind: "tool" }> => entry.kind === "tool");
  const send = () => {
    const text = draft.trim();
    if (!text) return;
    onSend(text);
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
          <span title="Model">{session.summary.model}</span>
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
          <EntryView key={index} entry={entry} rendered={rendered} onRespond={onRespond} onAnswer={onAnswer} />
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
          rows={1}
        />
        <button type="submit" className="send" disabled={!draft.trim()} aria-label="Send" title="Send (Enter)">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 19V5M5 12l7-7 7 7" />
          </svg>
        </button>
      </form>
    </div>
  );
}

/** A batch of questions from the agent; each starts at its recommended answer. */
function QuestionsView({
  entry,
  onAnswer,
}: {
  entry: Extract<Entry, { kind: "questions" }>;
  onAnswer: Props["onAnswer"];
}) {
  const [answers, setAnswers] = useState<string[]>(() => entry.questions.map((question: Question) => question.recommended));
  const inputId = `question-${entry.requestId}`;
  const setAnswer = (index: number, value: string) =>
    setAnswers((all) => all.map((answer, at) => (at === index ? value : answer)));
  return (
    <form
      className="questions"
      aria-label="Questions from the agent"
      onSubmit={(event) => {
        event.preventDefault();
        if (!entry.answered) onAnswer(entry.requestId, answers);
      }}
    >
      {entry.questions.map((question: Question, index: number) => (
        <div key={index} className="question">
          <label htmlFor={`${inputId}-${index}`}>{question.question}</label>
          {question.options.length > 0 && (
            <span className="buttons">
              {question.options.map((option) => (
                <button
                  type="button"
                  key={option}
                  className={answers[index] === option ? "primary" : ""}
                  disabled={entry.answered}
                  onClick={() => setAnswer(index, option)}
                >
                  {option}
                </button>
              ))}
            </span>
          )}
          <input
            id={`${inputId}-${index}`}
            value={answers[index] ?? ""}
            disabled={entry.answered}
            onChange={(event) => setAnswer(index, event.target.value)}
          />
          <span className="muted">Suggested: {question.recommended}</span>
        </div>
      ))}
      {entry.answered ? (
        <span className="muted">Answered</span>
      ) : (
        <button type="submit" className="primary">
          Send answers
        </button>
      )}
    </form>
  );
}

function EntryView({
  entry,
  rendered,
  onRespond,
  onAnswer,
}: {
  entry: Entry;
  rendered: boolean;
  onRespond: Props["onRespond"];
  onAnswer: Props["onAnswer"];
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
    case "questions":
      return <QuestionsView entry={entry} onAnswer={onAnswer} />;
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
            {step.depends_on.length > 0 && <span className="muted"> · after {step.depends_on.join(", ")}</span>}
            {step.owner && <span className="muted"> · {step.owner}</span>}
            {step.acceptance && <div className="muted">Done when: {step.acceptance}</div>}
          </li>
        ))}
      </ol>
      {plan.assumptions.length > 0 && <PlanNotes title="Assumptions" items={plan.assumptions} />}
      {plan.open_questions.length > 0 && <PlanNotes title="Open questions" items={plan.open_questions} />}
    </details>
  );
}

function PlanNotes({ title, items }: { title: string; items: string[] }) {
  return (
    <div className="muted">
      {title}:
      <ul>
        {items.map((item, index) => (
          <li key={index}>{item}</li>
        ))}
      </ul>
    </div>
  );
}
