// Delegated tasks: each subagent's model, status, cost, activity, and report. A finished
// subagent keeps its conversation, so its menu can give it a follow-up ("Continue with…").

import { MessageSquarePlus } from "lucide-react";
import { useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { RowMenu } from "../components/RowMenu";
import { ToolState } from "../components/ToolState";
import { formatCost, type SessionState, type TaskState } from "../sessions";

export function AgentsPanel({
  session,
  onResume,
}: {
  session: SessionState | null;
  /** Gives a finished subagent a follow-up (`session.agent_resume`). */
  onResume?: (taskId: string, text: string) => Promise<unknown>;
}) {
  const tasks = session ? Object.values(session.tasks) : [];
  if (tasks.length === 0) {
    return <p className="empty">Subagents appear here when the agent delegates work.</p>;
  }
  const archived = session?.summary.archived ?? false;
  return (
    <ul className="tasks">
      {tasks.map((task) => (
        <TaskRow key={task.taskId} task={task} onResume={archived ? undefined : onResume} />
      ))}
    </ul>
  );
}

function TaskRow({ task, onResume }: { task: TaskState; onResume?: (taskId: string, text: string) => Promise<unknown> }) {
  const [composing, setComposing] = useState(false);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const finished = task.status !== "running";

  const submit = () => {
    const text = draft.trim();
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

  return (
    <li className="task-row">
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
      <details open={task.status === "running" || composing}>
        <summary>
          <span className={`badge ${task.status}`}>{task.status}</span> <strong>{task.taskId}</strong>{" "}
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
      {composing && (
        <form
          className="task-follow-up"
          onSubmit={(event) => {
            event.preventDefault();
            submit();
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
                submit();
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
