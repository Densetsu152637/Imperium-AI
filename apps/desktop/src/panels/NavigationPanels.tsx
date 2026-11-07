// The Projects (environments) and Sessions panels on the left.

import { useState } from "react";

import { IconPicker, ProjectIcon, normalizeProject, projectName, type ProjectIcons } from "../projectIcons";
import { formatCost, sessionTitle, totals, type SessionState } from "../sessions";
import { StatusBadge } from "./StatusBadge";

export function ProjectsPanel({
  projects,
  selected,
  icons,
  onSelect,
}: {
  projects: string[];
  selected: string | null;
  icons: ProjectIcons;
  onSelect: (project: string) => void;
}) {
  const [editing, setEditing] = useState<string | null>(null);
  if (projects.length === 0) return <p className="empty">No projects yet. Use Open folder to add one.</p>;
  return (
    <ul className="list" aria-label="Projects">
      {projects.map((project) => {
        const name = projectName(project);
        const choice = icons.iconFor(project);
        return (
          <li key={project}>
            <div className="row">
              <button
                className="icon-button"
                aria-label={`Change icon for ${name}`}
                aria-expanded={editing === project}
                title="Change icon"
                onClick={() => setEditing((open) => (open === project ? null : project))}
              >
                <ProjectIcon choice={choice} />
              </button>
              <button
                className={`row-main${project === selected ? " selected" : ""}`}
                aria-current={project === selected ? "true" : undefined}
                onClick={() => onSelect(project)}
              >
                <span className="name">{name}</span>
                <span className="detail">{project}</span>
              </button>
            </div>
            {editing === project && (
              <IconPicker
                name={name}
                choice={choice}
                onChange={(next) => icons.setIcon(project, next)}
                onReset={() => icons.setIcon(project, null)}
                onClose={() => setEditing(null)}
              />
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** Sessions needing the user come first, merged ones last, and otherwise the most recent first. */
function rank(session: SessionState): number {
  if (session.status === "needs_attention") return 0;
  if (session.status === "merged") return 2;
  return 1;
}

export function sortSessions(sessions: SessionState[]): SessionState[] {
  return [...sessions].sort((a, b) => rank(a) - rank(b) || b.updatedAt - a.updatedAt);
}

export function SessionsPanel({
  sessions,
  project,
  selected,
  icons,
  onSelect,
  onShowProject,
  onShowAll,
}: {
  /** Every session; filtered to `project` when one is given. */
  sessions: SessionState[];
  project: string | null;
  selected: string | null;
  icons: ProjectIcons;
  onSelect: (sessionId: string) => void;
  onShowProject: (project: string) => void;
  onShowAll: () => void;
}) {
  const shown = sortSessions(project ? sessions.filter((s) => normalizeProject(s.summary.project_dir) === project) : sessions);
  return (
    <div className="sessions">
      {project && (
        <div className="sessions-scope">
          <button className="back" onClick={onShowAll}>
            <span aria-hidden="true">←</span> All sessions
          </button>
          <span className="scope-name" title={project}>
            <ProjectIcon choice={icons.iconFor(project)} size="small" />
            <span>{projectName(project)}</span>
          </span>
        </div>
      )}
      {shown.length === 0 ? (
        <p className="empty">{project ? "No sessions in this project." : "No sessions yet."}</p>
      ) : (
        <ul className="list" aria-label={project ? `Sessions in ${projectName(project)}` : "All sessions"}>
          {shown.map((session) => {
            const id = session.summary.session_id;
            const dir = session.summary.project_dir;
            return (
              <li key={id}>
                <div className="row">
                  <button
                    className="icon-button"
                    aria-label={`Show sessions in ${projectName(dir)}`}
                    title={`Show sessions in ${projectName(dir)}`}
                    onClick={() => onShowProject(dir)}
                  >
                    <ProjectIcon choice={icons.iconFor(dir)} />
                  </button>
                  <button
                    className={`row-main${id === selected ? " selected" : ""}`}
                    aria-current={id === selected ? "true" : undefined}
                    onClick={() => onSelect(id)}
                  >
                    <span className="name">{sessionTitle(session)}</span>
                    <span className="row-meta">
                      <StatusBadge session={session} />
                      <span className="detail">
                        {!project && `${projectName(dir)} · `}
                        {session.summary.model} · {formatCost(totals(session.calls).costUsd)}
                      </span>
                    </span>
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
