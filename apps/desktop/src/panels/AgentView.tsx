// A subagent's log in the main session panel, in place of the root's transcript: a breadcrumb
// back to the session ("← <session> / <subagent>"), its plan, its transcript drawn exactly as the
// root's (turns, steering, replies, tool calls and groups of them), permission requests it waits
// on, and a composer that messages it directly. While it runs, a message waits in its queue and
// runs as its next turn, and a queued message's Steer button (or Ctrl+Shift+Enter) joins its
// running turn instead, as for the root. A finished or paused subagent takes the message as a
// follow-up that continues its kept conversation.

import { ArrowLeft, CircleX, Play } from "lucide-react";
import { useEffect, useMemo, useState } from "react";

import type { Attachment, PermissionDecision, PermissionRule, RuntimeInstallJob, SendMode } from "@imperium/protocol";

import { RESUME_TEXT, agentComposerHint, formatElapsed, taskElapsed } from "../agentView";
import { JumpToLatest } from "../components/JumpToLatest";
import { ModelName } from "../components/ModelName";
import { ToolGroup } from "../components/ToolGroup";
import { WaitLine } from "../components/WaitLine";
import type { DaemonClient } from "../daemon/client";
import type { QueueChange } from "../queue";
import { agentName, formatCost, sessionTitle, type SessionState, type TaskState } from "../sessions";
import { useSettings } from "../settings";
import { isStopped, shownTaskStatus } from "../stop";
import { groupToolRuns } from "../toolCalls";
import { useFollowScroll } from "../useFollowScroll";
import { describeWait } from "../waits";
import { useNow } from "./AgentsPanel";
import { Composer } from "./Composer";
import { EntryView, PlanView, QueueView } from "./SessionPanel";

type Props = {
  session: SessionState;
  task: TaskState;
  /** Back to the root's transcript. */
  onBack: () => void;
  /** Messages the subagent: queued or steering while it runs, a follow-up once it has finished. */
  onSend: (text: string, mode: SendMode, attachments: Attachment[]) => void;
  /** Edits, removes, or steers a message waiting for the subagent. */
  onChangeQueued: (id: string, change: QueueChange) => void;
  onRespond: (requestId: number, decision: PermissionDecision) => void;
  onAlwaysAllow?: (requestId: number, rule: PermissionRule) => void;
  onAnswer: (requestId: number, answers: string[]) => void;
  /** Resumes a paused subagent (`session.agent_resume`). */
  onResume?: (taskId: string, text: string) => Promise<unknown>;
  /** Ends a paused subagent (`session.agent_cancel`). */
  onCancel?: (taskId: string) => Promise<unknown>;
  /** Stops the session's work, from the subagent's status line. */
  onStop?: () => void;
  client?: DaemonClient | null;
  runtimeInstall?: RuntimeInstallJob | null;
};

export function AgentView({
  session,
  task,
  onBack,
  onSend,
  onChangeQueued,
  onRespond,
  onAlwaysAllow,
  onAnswer,
  onResume,
  onCancel,
  onStop,
  client = null,
  runtimeInstall = null,
}: Props) {
  const settings = useSettings();
  const [busyAction, setBusyAction] = useState(false);
  const items = useMemo(() => groupToolRuns(task.entries), [task.entries]);
  const running = task.status === "running";
  const now = useNow(running);
  const archived = session.summary.archived;
  const stopped = isStopped(session);
  const status = shownTaskStatus(task.status, stopped);
  const elapsed = taskElapsed(task, now);
  const paused = task.status === "paused";
  // Permission requests from this subagent wait in the session's list; they are answered here too.
  const requests = session.entries.filter(
    (entry) => entry.kind === "permission" && entry.agentId === task.taskId && !entry.answered,
  );
  const wait =
    running && !stopped && !archived ? describeWait({ wait: session.waits[task.taskId], install: runtimeInstall }) : null;
  const name = (id: string) => agentName(session, id);

  // Follows the subagent's newest output until the reader scrolls up (roadmap item 20).
  const last = task.entries.at(-1);
  const follow = useFollowScroll({
    count: items.length + requests.length,
    activity: `${task.entries.length}:${last && "text" in last ? last.text.length : 0}:${requests.length}`,
    enabled: settings.autoScroll,
    resetKey: `${session.summary.session_id}/${task.taskId}`,
  });
  const send: Props["onSend"] = (text, mode, attachments) => {
    follow.resume();
    onSend(text, mode, attachments);
  };

  // Escape (outside a text field) goes back to the session.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      const target = event.target as HTMLElement | null;
      if (target && (target.closest("input, textarea, select, [role=menu], [role=dialog]") || target.isContentEditable)) return;
      onBack();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onBack]);

  const act = (action: () => Promise<unknown>) => {
    setBusyAction(true);
    action()
      .catch(() => {})
      .finally(() => setBusyAction(false));
  };

  return (
    <div className="session agent-view">
      <header className="session-top">
        <div className="session-head">
          <nav className="agent-crumbs" aria-label="Breadcrumb">
            <button
              type="button"
              className="ghost agent-back"
              onClick={onBack}
              title="Back to the session (Esc)"
              aria-label={`Back to ${sessionTitle(session)}`}
            >
              <ArrowLeft aria-hidden="true" />
              <span className="agent-crumb-session">{sessionTitle(session)}</span>
            </button>
            <span className="agent-crumb-separator" aria-hidden="true">
              /
            </span>
            <h3 className="session-title" title={`${task.title} (${task.taskId})`} aria-current="page">
              {task.title}
            </h3>
          </nav>
          <span
            className={`badge ${status}`}
            title={stopped && running ? "Halted while the session is stopped" : undefined}
          >
            {status}
          </span>
          {paused && onResume && !archived && (
            <button
              type="button"
              className="primary"
              disabled={busyAction}
              onClick={() => act(() => onResume(task.taskId, RESUME_TEXT))}
              title="Resume from where it paused, with its saved conversation"
            >
              <Play aria-hidden="true" /> Resume
            </button>
          )}
          {paused && onCancel && !archived && (
            <button
              type="button"
              className="ghost danger"
              disabled={busyAction}
              onClick={() => act(() => onCancel(task.taskId))}
              title="End it without resuming"
            >
              <CircleX aria-hidden="true" /> Cancel
            </button>
          )}
        </div>
      </header>

      {task.plan && <PlanView plan={task.plan} />}

      <div className="conversation" aria-live="polite" ref={follow.ref}>
        {items.map((item) =>
          item.kind === "tools" ? (
            <ToolGroup key={item.index} calls={item.calls} />
          ) : (
            <EntryView
              key={item.index}
              entry={item.entry}
              rendered={settings.renderMarkdown}
              onRespond={onRespond}
              onAlwaysAllow={onAlwaysAllow}
              onAnswer={onAnswer}
              agentName={name}
            />
          ),
        )}
        {requests.map((entry, index) => (
          <EntryView
            key={`request-${index}`}
            entry={entry}
            rendered={settings.renderMarkdown}
            onRespond={onRespond}
            onAlwaysAllow={onAlwaysAllow}
            onAnswer={onAnswer}
            agentName={name}
          />
        ))}
        {wait ? (
          <WaitLine view={wait} onStop={onStop} />
        ) : (
          running && !stopped && <p className="working">Working…</p>
        )}
        {task.status === "failed" && task.report && <p className="error">{task.report}</p>}
        {paused && (
          <p className="notice">
            Paused for a steering message to the root agent. Resume it, message it to re-brief it, or cancel it.
          </p>
        )}
      </div>
      <JumpToLatest follow={follow} />

      {task.queue.length > 0 && <QueueView queue={task.queue} busy={running} onChange={onChangeQueued} />}

      <Composer
        sessionId={session.summary.session_id}
        busy={running}
        hidden={archived}
        client={client}
        placeholder={agentComposerHint(task.title, task.status, settings.sendKey === "enter")}
        onSend={send}
      />

      <footer className="session-footer" aria-label="Subagent status">
        <span className="muted agent-footer-id" title={task.brief}>
          {task.taskId} · <ModelName name={task.model} />
        </span>
        <span className="session-footer-spacer" />
        {elapsed !== null && <span className="muted">{formatElapsed(elapsed)}</span>}
        <span className="session-cost muted" title="Subagent cost">
          {formatCost(task.costUsd)}
        </span>
      </footer>
    </div>
  );
}
