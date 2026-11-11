// The main session panel: the title and status at the top, then the conversation, the composer,
// and under it a footer with the model selector at the left and the context meter at the right.
// The meter opens the context breakdown, session totals, and outputs above the composer; the
// options button opens Markdown rendering and the usage-limit policy there. Beside the meter, a
// small budget mark shows when a budget over the session (its environment, endpoint, or model, or
// all usage) has passed an alert threshold.

import { CornerDownRight, Gauge, Paperclip, Pencil, Play, SlidersHorizontal, Square, Trash, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import type {
  Attachment,
  BudgetStatus,
  LocalModelRow,
  ModelRow,
  PermissionDecision,
  PermissionRule,
  Plan,
  Question,
  QueuedMessage,
  ResumePolicy,
  RuntimeInfo,
  RuntimeInstallJob,
  SendMode,
  SessionControlResult,
} from "@imperium/protocol";

import { ProjectIcon, projectName, type ProjectIcons } from "../projectIcons";
import { entryForEvent } from "../sessionSearch";
import {
  ROOT,
  breakdownUsed,
  describeUse,
  formatCost,
  formatCount,
  formatResumeTime,
  formatTokens,
  sessionTitle,
  totals,
  type Entry,
  type SessionState,
} from "../sessions";
import { describeUsage, formatShare, indicator, modelSpec } from "../budgets";
import { describeRule } from "../components/PermissionsEditor";
import { Select } from "../components/Select";
import { Switch } from "../components/Switch";
import { ToolCall } from "../components/ToolCall";
import { WaitLine } from "../components/WaitLine";
import { describeWait } from "../waits";
import type { DaemonClient } from "../daemon/client";
import { SentAttachments } from "../components/AttachmentChips";
import { Composer } from "./Composer";
import { ContextBreakdownView } from "./ContextBreakdown";
import { ModelSuggestionCard } from "./ModelSuggestionCard";
import { SessionModelSelect } from "./ModelSelect";
import { StatusBadge } from "./StatusBadge";
import { useSettings } from "../settings";
import { canStop, isStopShortcut, isStopped } from "../stop";
import { editStillValid, queueActions, queueLabel, steeredLabel, type QueueChange } from "../queue";

type Props = {
  session: SessionState;
  icons: ProjectIcons;
  /** Sends a message: `queue` starts a turn, or waits while one runs; `steer` joins the running turn. */
  onSend: (text: string, mode: SendMode, attachments: Attachment[]) => void;
  /** Edits, removes, or steers a queued message (`session.queue`). */
  onChangeQueued: (id: string, change: QueueChange) => void;
  onRespond: (requestId: number, decision: PermissionDecision) => void;
  /** Saves `rule` (an allow rule the daemon suggested) and allows the request. */
  onAlwaysAllow?: (requestId: number, rule: PermissionRule) => void;
  /** One answer per question, in order. */
  onAnswer: (requestId: number, answers: string[]) => void;
  /** Resolves once the daemon accepted the policy. */
  onSetResumePolicy: (policy: ResumePolicy) => Promise<void>;
  onResumeNow: () => void;
  /** Stops all of the session's work (`true`) or resumes it (`false`): `session.control` `stop` / `resume`. */
  onStop: (stop: boolean) => void;
  /** The model table (`models.list`), offered in the model selector. */
  models: ModelRow[];
  /** Local models found in the model folders (`models.local`), offered after the table. */
  localModels: LocalModelRow[];
  /** The llama.cpp runtime (`models.local`), so choosing a llama.cpp model without one warns first. */
  localRuntime?: RuntimeInfo | null;
  /** Opens the Local models panel. */
  onOpenLocalModels?: () => void;
  /** Switches the session's model; resolves with the daemon's answer, rejects with its reason. */
  onSetModel: (model: string) => Promise<SessionControlResult>;
  /** For answering model download suggestions and following their downloads. */
  client?: DaemonClient | null;
  /** Resumes an archived session. */
  onUnarchive?: () => void;
  /** Where every budget stands (`budgets.get`), for the indicator beside the context meter. */
  budgets?: BudgetStatus[];
  /** Scrolls to the entry holding this history event (a session search hit); `nonce` repeats it. */
  focusEvent?: { index: number; nonce: number } | null;
  /** llama.cpp being installed, named in the status line while a model waits to load. */
  runtimeInstall?: RuntimeInstallJob | null;
  /** Answers the status line's budget question: send the waiting message anyway, or drop it. */
  onBudget?: (confirm: boolean) => void;
};

/** What the footer has opened above the composer. */
type Drawer = "context" | "options" | null;

export function SessionPanel({
  session,
  icons,
  onSend,
  onChangeQueued,
  onRespond,
  onAlwaysAllow,
  onAnswer,
  onSetResumePolicy,
  onResumeNow,
  onStop,
  models,
  localModels,
  localRuntime = null,
  onOpenLocalModels,
  onSetModel,
  client = null,
  onUnarchive,
  budgets = [],
  focusEvent = null,
  runtimeInstall = null,
  onBudget,
}: Props) {
  const settings = useSettings();
  const [rendered, setRendered] = useState(settings.renderMarkdown);
  const [drawer, setDrawer] = useState<Drawer>(null);
  const drawerId = useId();
  useEffect(() => setRendered(settings.renderMarkdown), [settings.renderMarkdown]);
  const end = useRef<HTMLDivElement>(null);
  const entryCount = session.entries.length;
  const last = session.entries.at(-1);
  const lastLength = last && "text" in last ? last.text.length : 0;

  // Block body: newer browsers return a promise from scrollIntoView, which an effect must not return.
  useEffect(() => {
    if (settings.autoScroll) end.current?.scrollIntoView({ block: "end" });
  }, [entryCount, lastLength, settings.autoScroll]);

  // A search hit: after any scroll to the end, bring its entry into view and mark it briefly.
  const conversation = useRef<HTMLDivElement>(null);
  const sessionId = session.summary.session_id;
  useEffect(() => {
    if (!focusEvent) return;
    const frame = window.requestAnimationFrame(() => {
      const index = entryForEvent(
        session.entries.map((entry) => entry.event),
        focusEvent.index,
      );
      const target = index === null ? null : conversation.current?.children.item(index);
      if (!(target instanceof HTMLElement)) return;
      target.scrollIntoView({ block: "center" });
      target.classList.add("search-focus");
      window.setTimeout(() => target.classList.remove("search-focus"), 2000);
    });
    return () => window.cancelAnimationFrame(frame);
    // Only a new hit (or another session) scrolls; new entries do not.
  }, [focusEvent?.nonce, sessionId]);

  const sum = totals(session.calls);
  const outputs = session.entries.filter((entry): entry is Extract<Entry, { kind: "tool" }> => entry.kind === "tool");
  const archived = session.summary.archived;
  const toggle = (next: Exclude<Drawer, null>) => setDrawer((open) => (open === next ? null : next));
  const stopped = isStopped(session);
  const runningTasks = Object.values(session.tasks).filter((task) => task.status === "running").length;
  const stoppable = !archived && canStop(session, runningTasks);
  // What the root agent waits on before its first output; the stopped banner says when it is
  // stopped (only a budget question still shows then, since it is about a message not yet sent).
  const described = archived
    ? null
    : describeWait({
        wait: session.waits[ROOT],
        sent: session.sent,
        pause:
          session.pause && !stopped
            ? { cause: session.pause.cause, until: session.pause.untilMs > 0 ? formatResumeTime(session.pause.untilMs) : null }
            : null,
        install: runtimeInstall,
      });
  const wait = stopped && described?.action !== "budget" ? null : described;
  // Ctrl+. (Cmd+. on macOS) stops a running session or resumes a stopped one.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!isStopShortcut(event) || archived) return;
      if (stopped || stoppable) {
        event.preventDefault();
        onStop(!stopped);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stopped, stoppable, archived, onStop]);

  return (
    <div className="session">
      <header className="session-top">
        <div className="session-head">
          <ProjectIcon choice={icons.iconFor(session.summary.project_dir)} />
          <h3 className="session-title" title={sessionTitle(session)}>
            {sessionTitle(session)}
          </h3>
          <StatusBadge session={session} />
          {stoppable && (
            <button
              className="stop-session ghost danger"
              onClick={() => onStop(true)}
              title="Stop: pause the agent and its subagents at their next safe point (Ctrl+.)"
            >
              <Square aria-hidden="true" /> Stop
            </button>
          )}
          {stopped ? (
            <button className="resume-now primary" onClick={() => onStop(false)} title="Resume all work (Ctrl+.)">
              <Play aria-hidden="true" /> Resume
            </button>
          ) : (
            session.status === "paused" && (
              <button className="resume-now primary" onClick={onResumeNow}>
                Resume now
              </button>
            )
          )}
        </div>
      </header>

      {session.plan && <PlanView plan={session.plan} />}

      <div className="conversation" aria-live="polite" ref={conversation}>
        {session.entries.map((entry, index) =>
          entry.kind === "suggestion" ? (
            <ModelSuggestionCard
              key={index}
              entry={entry}
              sessionId={session.summary.session_id}
              client={client}
              onSetModel={(model) => onSetModel(model).catch(() => null)}
            />
          ) : (
            <EntryView
              key={index}
              entry={entry}
              rendered={rendered}
              onRespond={onRespond}
              onAlwaysAllow={onAlwaysAllow}
              onAnswer={onAnswer}
            />
          ),
        )}
        {session.sent && <div className="message user prose pending">{session.sent.text}</div>}
        {wait ? (
          <WaitLine
            view={wait}
            onStop={stoppable || session.sent ? () => onStop(true) : undefined}
            onResume={() => onStop(false)}
            onResumeNow={onResumeNow}
            onBudget={onBudget}
          />
        ) : (
          session.busy && <p className="working">Working…</p>
        )}
        <div ref={end} />
      </div>

      {archived && (
        <div className="archived-note" role="status">
          <span>This session is archived: it is not running and keeps its history. Unarchive it to continue.</span>
          {onUnarchive && (
            <button className="primary" onClick={onUnarchive}>
              Unarchive
            </button>
          )}
        </div>
      )}

      {session.summary.model === null && !archived && (
        <p className="needs-model" role="status">
          This session has no model yet. Pick one with the model selector under the message box (or set a default under
          Settings → Models);
          {session.status === "needs_model"
            ? " your messages are waiting and run as soon as it has one."
            : " messages you send wait until then."}
        </p>
      )}

      {stopped && !archived && (
        <div className="stopped-banner" role="status">
          <span>Stopped. The agent and its subagents are paused where they were; messages you send wait in the queue.</span>
          <button className="primary" onClick={() => onStop(false)}>
            <Play aria-hidden="true" /> Resume
          </button>
        </div>
      )}
      {session.queue.length > 0 && <QueueView queue={session.queue} busy={session.busy} onChange={onChangeQueued} />}

      {drawer && (
        <section
          id={drawerId}
          className="session-drawer"
          aria-label={drawer === "context" ? "Context and outputs" : "Session options"}
        >
          <button
            type="button"
            className="ghost icon-only session-drawer-close"
            onClick={() => setDrawer(null)}
            aria-label="Close"
            title="Close"
          >
            <X aria-hidden="true" />
          </button>
          {drawer === "context" ? (
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
                  <dt>Cost</dt>
                  <dd>{formatCost(sum.costUsd)}</dd>
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
          ) : (
            <div className="session-sections session-options">
              <section>
                <h3>Display</h3>
                <label className="option-row">
                  <Switch checked={rendered} onChange={setRendered} label="Render Markdown" />
                  Render Markdown
                </label>
                <p className="muted" title={session.summary.project_dir}>
                  {projectName(session.summary.project_dir)} · {session.summary.isolation}
                </p>
              </section>
              <section>
                <h3>Usage limits</h3>
                <ResumeControls
                  key={session.summary.session_id}
                  policy={session.summary.resume_policy}
                  onSave={onSetResumePolicy}
                />
              </section>
            </div>
          )}
        </section>
      )}

      <Composer
        sessionId={session.summary.session_id}
        busy={session.busy}
        hidden={archived}
        client={client}
        placeholder={composerHint(settings.sendKey === "enter", session.busy)}
        onSend={onSend}
      />

      <footer className="session-footer" aria-label="Session status">
        <SessionModelSelect
          key={session.summary.session_id}
          current={session.summary.model}
          busy={session.busy}
          disabled={archived}
          models={models}
          local={localModels}
          runtime={localRuntime}
          onOpenLocalModels={onOpenLocalModels}
          onSwitch={onSetModel}
        />
        <span className="session-footer-spacer" />
        {sum.costUsd !== null && (
          <span className="session-cost muted" title="Session cost">
            {formatCost(sum.costUsd)}
          </span>
        )}
        <button
          type="button"
          className="ghost icon-only footer-button"
          onClick={() => toggle("options")}
          aria-label="Session options"
          aria-expanded={drawer === "options"}
          aria-controls={drawer === "options" ? drawerId : undefined}
          title={`Session options: Markdown and usage limits (when usage runs out: ${describePolicy(session.summary.resume_policy)})`}
        >
          <SlidersHorizontal aria-hidden="true" />
        </button>
        <BudgetMark
          budgets={budgets}
          environmentId={session.summary.environment_id ?? null}
          spec={modelSpec(session.summary.model, models)}
        />
        <ContextMeter
          session={session}
          promptTokens={sum.prompt}
          open={drawer === "context"}
          controls={drawer === "context" ? drawerId : undefined}
          onToggle={() => toggle("context")}
        />
      </footer>
    </div>
  );
}

/**
 * How full the root agent's context was on its latest call, as a small bar and `12.3k / 32.8k`;
 * pressing it opens the full breakdown.
 */
function ContextMeter({
  session,
  promptTokens,
  open,
  controls,
  onToggle,
}: {
  session: SessionState;
  promptTokens: number;
  open: boolean;
  controls?: string;
  onToggle: () => void;
}) {
  const breakdown = session.breakdowns[ROOT];
  const used = breakdown ? breakdownUsed(breakdown) : 0;
  const limit = breakdown?.contextWindow ?? null;
  const share = limit ? Math.min(used / limit, 1) : null;
  const text = breakdown
    ? limit
      ? `${formatTokens(used)} / ${formatTokens(limit)}`
      : `${formatTokens(used)} tokens`
    : promptTokens > 0
      ? `${formatTokens(promptTokens)} prompt tokens`
      : "Context";
  const level = share === null ? "" : share >= 0.9 ? " full" : share >= 0.7 ? " high" : "";
  return (
    <button
      type="button"
      className={`ghost context-meter${level}`}
      onClick={onToggle}
      aria-expanded={open}
      aria-controls={controls}
      title={
        breakdown
          ? `Context: ${describeUse(breakdown)}${share !== null ? ` (${Math.round(share * 100)}%)` : ""}. Show the breakdown, totals, and outputs.`
          : "Show the context breakdown, totals, and outputs"
      }
    >
      {share !== null && (
        <span className="context-meter-bar" aria-hidden="true">
          <span style={{ width: `${Math.max(share * 100, 2)}%` }} />
        </span>
      )}
      <span className="context-meter-text">{text}</span>
    </button>
  );
}

/**
 * A small mark when a budget over the session has passed an alert threshold: amber near the
 * limit, red when used up. Its title lists each such budget.
 */
function BudgetMark({
  budgets,
  environmentId,
  spec,
}: {
  budgets: BudgetStatus[];
  environmentId: string | null;
  spec: string | null;
}) {
  const shown = indicator(budgets, environmentId, spec);
  if (!shown) return null;
  const lines = shown.all.map(
    (status) =>
      `${status.label}: ${formatShare(status.share)} (${describeUsage(status, formatTokens)}), resets ${status.window_end}` +
      (status.exceeded && status.budget.action !== "warn"
        ? status.budget.action === "ask"
          ? "; new turns ask first"
          : "; new turns are blocked"
        : ""),
  );
  return (
    <span className={`budget-mark ${shown.level}`} role="status" title={lines.join("\n")}>
      <Gauge aria-hidden="true" />
      <span>{formatShare(shown.status.share)}</span>
      <span className="visually-hidden">of {shown.status.label}</span>
    </span>
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
  onAlwaysAllow,
  onAnswer,
}: {
  entry: Entry;
  rendered: boolean;
  onRespond: Props["onRespond"];
  onAlwaysAllow?: Props["onAlwaysAllow"];
  onAnswer: Props["onAnswer"];
}) {
  switch (entry.kind) {
    case "user":
      return (
        <div className="message user prose">
          {entry.attachments && <SentAttachments attachments={entry.attachments} />}
          {entry.text}
        </div>
      );
    case "steer":
      return (
        <div className="message user steer prose" title="Steered: joined the running turn here">
          <span className="steer-label">{steeredLabel(entry.paused)}</span>
          {entry.attachments && <SentAttachments attachments={entry.attachments} />}
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
      return <ToolCall entry={entry} />;
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
              {entry.suggestedRule && onAlwaysAllow && (
                <button
                  onClick={() => entry.suggestedRule && onAlwaysAllow(entry.requestId, entry.suggestedRule)}
                  title={`Adds an allow rule to this environment's permissions (Settings → Permissions for global rules): ${describeRule(entry.suggestedRule)}`}
                >
                  Always allow {entry.suggestedRule.pattern ? <code>{entry.suggestedRule.pattern}</code> : "this tool"}
                </button>
              )}
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
    case "suggestion":
      return null;
  }
}

/** The composer's placeholder: what Enter does now, and how to steer while a turn runs. */
function composerHint(enterSends: boolean, busy: boolean): string {
  const key = enterSends ? "Enter" : "Ctrl+Enter";
  const newline = enterSends ? "Shift+Enter" : "Enter";
  return busy
    ? `${key} queues for after this turn · Steer a queued message to send it now (or Ctrl+Shift+Enter)`
    : `Message the agent (${key} to send, ${newline} for a new line)`;
}

/**
 * Messages waiting for the agent, above the composer. A queued one can be edited, steered into
 * the running turn, or removed until it is delivered; one that is steering waits for the
 * agent's next safe point and can only be removed.
 */
function QueueView({
  queue,
  busy,
  onChange,
}: {
  queue: QueuedMessage[];
  busy: boolean;
  onChange: Props["onChangeQueued"];
}) {
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);
  // Another window (or the agent) may steer, deliver, or remove the message being edited.
  useEffect(() => {
    if (editing && !editStillValid(queue, editing.id)) setEditing(null);
  }, [queue, editing]);
  const save = () => {
    if (!editing) return;
    const text = editing.text.trim();
    if (text) onChange(editing.id, { kind: "edit", text });
    setEditing(null);
  };
  return (
    <ol className="queue" aria-label="Queued messages">
      {queue.map((message) => {
        const actions = queueActions(message, busy);
        const isEditing = editing?.id === message.id;
        return (
          <li key={message.id} className={message.steer ? "steer" : undefined}>
            <span className="queue-kind">{queueLabel(message)}</span>
            {isEditing ? (
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
                {(message.attachments?.length ?? 0) > 0 && (
                  <span
                    className="queue-attachments muted"
                    title={message.attachments?.map((attachment) => attachment.name).join("\n")}
                  >
                    <Paperclip aria-hidden="true" /> {message.attachments?.length}{" "}
                  </span>
                )}
                {message.text}
              </span>
            )}
            <span className="buttons">
              {actions.includes("edit") && !isEditing && (
                <button
                  className="ghost"
                  onClick={() => setEditing({ id: message.id, text: message.text })}
                  aria-label="Edit queued message"
                  title="Edit"
                >
                  <Pencil aria-hidden="true" /> Edit
                </button>
              )}
              {actions.includes("steer") && (
                <button
                  className="ghost steer"
                  onClick={() => onChange(message.id, { kind: "steer" })}
                  aria-label="Steer the running turn with this message"
                  title="Steer: the agent reads it after its current step, instead of after the turn"
                >
                  <CornerDownRight aria-hidden="true" /> Steer
                </button>
              )}
              <button
                className="ghost danger"
                onClick={() => onChange(message.id, { kind: "remove" })}
                aria-label="Remove queued message"
                title="Remove"
              >
                <Trash aria-hidden="true" /> Remove
              </button>
            </span>
          </li>
        );
      })}
    </ol>
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
    <div className="usage-limits">
      <form
        onSubmit={(event) => {
          event.preventDefault();
          if (!dirty || !valid) return;
          setSaving(true);
          onSave(draft).finally(() => setSaving(false));
        }}
      >
        <span className="usage-mode">
          When usage runs out:
          <Select
            aria-label="When usage runs out"
            value={draft.mode}
            onChange={(mode) => set({ mode: mode as ResumePolicy["mode"] })}
            options={Object.entries(RESUME_MODES).map(([mode, label]) => ({ value: mode, label }))}
          />
        </span>
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
    </div>
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
