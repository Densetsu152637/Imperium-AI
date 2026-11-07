// The Projects (environments) and Sessions panels on the left.

import { formatCost, totals, type SessionState } from "../sessions";

export function ProjectsPanel({
  projects,
  selected,
  onSelect,
}: {
  projects: string[];
  selected: string | null;
  onSelect: (project: string) => void;
}) {
  if (projects.length === 0) return <p className="empty">No projects yet. Use Open folder to add one.</p>;
  return (
    <ul className="list" role="listbox" aria-label="Projects">
      {projects.map((project) => (
        <li key={project} role="option" aria-selected={project === selected}>
          <button className={project === selected ? "selected" : undefined} onClick={() => onSelect(project)}>
            <span className="name">{project.split(/[\\/]/).filter(Boolean).at(-1) ?? project}</span>
            <span className="detail">{project}</span>
          </button>
        </li>
      ))}
    </ul>
  );
}

export function SessionsPanel({
  sessions,
  selected,
  onSelect,
}: {
  sessions: SessionState[];
  selected: string | null;
  onSelect: (sessionId: string) => void;
}) {
  if (sessions.length === 0) return <p className="empty">No sessions in this project.</p>;
  return (
    <ul className="list" role="listbox" aria-label="Sessions">
      {sessions.map((session) => {
        const id = session.summary.session_id;
        const pending = session.entries.some((entry) => entry.kind === "permission" && !entry.answered);
        return (
          <li key={id} role="option" aria-selected={id === selected}>
            <button className={id === selected ? "selected" : undefined} onClick={() => onSelect(id)}>
              <span className="name">
                {session.entries.find((entry) => entry.kind === "user")?.text.slice(0, 60) ?? `Session ${id.slice(0, 6)}`}
              </span>
              <span className="detail">
                {pending ? "needs approval · " : session.busy ? "working · " : ""}
                {session.summary.model} · {formatCost(totals(session.calls).costUsd)}
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}
