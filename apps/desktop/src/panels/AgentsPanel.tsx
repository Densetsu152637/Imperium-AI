// Delegated tasks: each subagent's model, status, cost, activity, and report. A finished
// subagent keeps its conversation, so its menu can give it a follow-up ("Continue with…"). A
// subagent paused by a steering message to the root shows Resume and Cancel until the root or
// the user decides.

import { CircleX, MessageSquarePlus, Play } from "lucide-react";
import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { RowMenu } from "../components/RowMenu";
import { ToolState } from "../components/ToolState";
import { formatCost, type SessionState, type TaskState } from "../sessions";
import { isStopped, shownTaskStatus } from "../stop";

/** What a paused subagent is told when the user resumes it without a follow-up of their own. */
const RESUME_TEXT = "Continue where you left off.";

type Actions = {
  /** Gives a finished or paused subagent a follow-up (`session.agent_resume`). */
  onResume?: (taskId: string, text: string) => Promise<unknown>;
  /** Ends a paused subagent (`session.agent_cancel`). */
  onCancel?: (taskId: string) => Promise<unknown>;
};

export function AgentsPanel({ session, onResume, onCancel }: { session: SessionState | null } & Actions) {
  const tasks = session ? Object.values(session.tasks) : [];
  if (tasks.length === 0) {
    return <p className="empty">Subagents appear here when the agent delegates work.</p>;
  }
  const archived = session?.summary.archived ?? false;
  // While the session is stopped its running subagents are halted: shown as paused.
  const stopped = session ? isStopped(session) : false;
  return (
    <ul className="tasks">
      {tasks.map((task) => (
        <TaskRow
          key={task.taskId}
          task={task}
          stopped={stopped}
          onResume={archived ? undefined : onResume}
          onCancel={archived ? undefined : onCancel}
        />
      ))}
    </ul>
  );
}

function TaskRow({ task, stopped, onResume, onCancel }: { task: TaskState; stopped: boolean } & Actions) {
  const [composing, setComposing] = useState(false);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const finished = task.status !== "running";
  const paused = task.status === "paused";

  const resume = (text: string) => {
    if (!text || !onResume) return;
    setSending(true);
    onResume(task.taskId, text)
      .then(() => {
        setDraft("");
        setComposing(false);
      })
      .catch(() => {})
      .finally(() => setSending(false));
  };
  const cancel = () => {
    if (!onCancel) return;
    setSending(true);
    onCancel(task.taskId)
      .catch(() => {})
      .finally(() => setSending(false));
  };

  return (
    <li className={paused ? "task-row paused" : "task-row"}>
      {onResume && (
        <div className="task-menu">
          <RowMenu
            label={`More for ${task.taskId}`}
            items={[
              {
                label: "Continue with…",
                icon: MessageSquarePlus,
                disabled: !finished,
                onSelect: () => setComposing(true),
              },
            ]}
          />
        </div>
      )}
      <details open={task.status === "running" || paused || composing}>
        <summary>
          <span
            className={`badge ${shownTaskStatus(task.status, stopped)}`}
            title={stopped && task.status === "running" ? "Halted while the session is stopped" : undefined}
          >
            {shownTaskStatus(task.status, stopped)}
          </span> <strong>{task.taskId}</strong>{" "}
          <span className="muted">
            {task.model} · {formatCost(task.costUsd)}
          </span>
          <span className="brief">{task.brief}</span>
        </summary>
        <ul className="task-activity">
          {task.entries
            .filter((entry) => entry.kind === "tool" || entry.kind === "user")
            .map((entry, index) =>
              entry.kind === "user" ? (
                <li key={index} className="task-turn">
                  {entry.text.split("\n")[0]}
                </li>
              ) : entry.kind === "tool" ? (
                <li key={index} className={entry.ok === false ? "failed" : undefined}>
                  <ToolState ok={entry.ok} /> {entry.summary}
                </li>
              ) : null,
            )}
        </ul>
        {task.report && (
          <div className="task-report prose">
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{task.report}</ReactMarkdown>
          </div>
        )}
      </details>
      {paused && (onResume || onCancel) && !composing && (
        <div className="task-paused-actions" aria-label={`${task.taskId} is paused`}>
          {onResume && (
            <button
              type="button"
              disabled={sending}
              onClick={() => resume(RESUME_TEXT)}
              title="Resume from where it paused, with its saved conversation"
            >
              <Play aria-hidden="true" /> Resume
            </button>
          )}
          {onResume && (
            <button type="button" className="ghost" disabled={sending} onClick={() => setComposing(true)}>
              <MessageSquarePlus aria-hidden="true" /> Re-brief…
            </button>
          )}
          {onCancel && (
            <button type="button" className="ghost danger" disabled={sending} onClick={cancel} title="End it without resuming">
              <CircleX aria-hidden="true" /> Cancel
            </button>
          )}
        </div>
      )}
      {composing && (
        <form
          className="task-follow-up"
          onSubmit={(event) => {
            event.preventDefault();
            resume(draft.trim());
          }}
        >
          <textarea
            autoFocus
            rows={2}
            value={draft}
            aria-label={`Follow-up for ${task.taskId}`}
            placeholder={`A follow-up for ${task.taskId}; it continues with its earlier conversation`}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                event.preventDefault();
                resume(draft.trim());
              } else if (event.key === "Escape") {
                setComposing(false);
              }
            }}
          />
          <div className="task-follow-up-actions">
            <button type="button" className="ghost" onClick={() => setComposing(false)}>
              Cancel
            </button>
            <button type="submit" disabled={!draft.trim() || sending || !finished}>
              Continue
            </button>
          </div>
        </form>
      )}
    </li>
  );
}
