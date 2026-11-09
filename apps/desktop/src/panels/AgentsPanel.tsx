// Delegated tasks: each subagent's model, status, cost, activity, and report.

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

import { ToolState } from "../components/ToolState";
import { formatCost, type SessionState } from "../sessions";

export function AgentsPanel({ session }: { session: SessionState | null }) {
  const tasks = session ? Object.values(session.tasks) : [];
  if (tasks.length === 0) {
    return <p className="empty">Subagents appear here when the agent delegates work.</p>;
  }
  return (
    <ul className="tasks">
      {tasks.map((task) => (
        <li key={task.taskId}>
          <details open={task.status === "running"}>
            <summary>
              <span className={`badge ${task.status}`}>{task.status}</span> <strong>{task.taskId}</strong>{" "}
              <span className="muted">
                {task.model} · {formatCost(task.costUsd)}
              </span>
              <span className="brief">{task.brief}</span>
            </summary>
            <ul className="task-activity">
              {task.entries
                .filter((entry) => entry.kind === "tool")
                .map((entry, index) => (
                  <li key={index} className={entry.ok === false ? "failed" : undefined}>
                    <ToolState ok={entry.ok} /> {entry.summary}
                  </li>
                ))}
            </ul>
            {task.report && (
              <div className="task-report prose">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{task.report}</ReactMarkdown>
              </div>
            )}
          </details>
        </li>
      ))}
    </ul>
  );
}
