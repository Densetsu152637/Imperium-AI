// Delegated tasks as a compact list: one line per subagent with its status, title, and how long
// it has run (its task id, model, and cost on hover). Choosing one opens its log in the main
// session panel (AgentView), where it can be messaged and steered. A subagent paused by a
// steering message to the root offers Resume and Cancel in its menu too.

import { CircleX, PanelTop, Play } from "lucide-react";
import { useEffect, useState } from "react";

import { RESUME_TEXT, formatElapsed, taskElapsed } from "../agentView";
import { RowMenu, type RowMenuItem } from "../components/RowMenu";
import { modelName, useModelNames } from "../modelNames";
import { formatCost, type SessionState, type TaskState } from "../sessions";
import { isStopped, shownTaskStatus } from "../stop";


type Actions = {
  /** Opens the subagent's log in the main panel. */
  onOpen?: (taskId: string) => void;
  /** Gives a finished or paused subagent a follow-up (`session.agent_resume`). */
  onResume?: (taskId: string, text: string) => Promise<unknown>;
  /** Ends a paused subagent (`session.agent_cancel`). */
  onCancel?: (taskId: string) => Promise<unknown>;
};

/** The current time, every second while `ticking`: running subagents' elapsed times count up. */
export function useNow(ticking: boolean): number {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!ticking) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [ticking]);
  return now;
}

export function AgentsPanel({
  session,
  openAgent = null,
  onOpen,
  onResume,
  onCancel,
}: {
  session: SessionState | null;
  /** The subagent whose log the main panel shows, if any. */
  openAgent?: string | null;
} & Actions) {
  const tasks = session ? Object.values(session.tasks) : [];
  const now = useNow(tasks.some((task) => task.status === "running"));
  if (tasks.length === 0) {
    return <p className="empty">Subagents appear here when the agent delegates work.</p>;
  }
  const archived = session?.summary.archived ?? false;
  // While the session is stopped its running subagents are halted: shown as paused.
  const stopped = session ? isStopped(session) : false;
  return (
    <ul className="list agent-list" aria-label="Subagents">
      {tasks.map((task) => (
        <AgentRow
          key={task.taskId}
          task={task}
          now={now}
          stopped={stopped}
          open={task.taskId === openAgent}
          onOpen={onOpen}
          onResume={archived ? undefined : onResume}
          onCancel={archived ? undefined : onCancel}
        />
      ))}
    </ul>
  );
}

function AgentRow({
  task,
  now,
  stopped,
  open,
  onOpen,
  onResume,
  onCancel,
}: { task: TaskState; now: number; stopped: boolean; open: boolean } & Actions) {
  const aliases = useModelNames();
  const status = shownTaskStatus(task.status, stopped);
  const elapsed = taskElapsed(task, now);
  const paused = task.status === "paused";
  const hover = [
    `${task.title} (${task.taskId})`,
    `${status} · ${modelName(aliases, task.model)} · ${formatCost(task.costUsd)}`,
    stopped && task.status === "running" ? "Halted while the session is stopped" : "",
  ]
    .filter(Boolean)
    .join("\n");
  const items: RowMenuItem[] = [
    { label: "Open log", icon: PanelTop, onSelect: () => onOpen?.(task.taskId), disabled: !onOpen },
  ];
  if (paused && onResume) {
    items.push({ label: "Resume", icon: Play, onSelect: () => void onResume(task.taskId, RESUME_TEXT).catch(() => {}) });
  }
  if (paused && onCancel) {
    items.push({ label: "Cancel", icon: CircleX, danger: true, onSelect: () => void onCancel(task.taskId).catch(() => {}) });
  }
  return (
    <li>
      <div className="row nav-row agent-row">
        <button
          type="button"
          className={`row-main agent-row-main${open ? " selected" : ""}`}
          aria-current={open ? "true" : undefined}
          title={hover}
          onClick={() => onOpen?.(task.taskId)}
          disabled={!onOpen}
        >
          <span className={`status-dot task-${status}`} role="img" aria-label={status} />
          <span className="name">{task.title}</span>
          {elapsed !== null && <span className="agent-elapsed">{formatElapsed(elapsed)}</span>}
        </button>
        <RowMenu label={`More for ${task.title}`} items={items} />
      </div>
    </li>
  );
}
