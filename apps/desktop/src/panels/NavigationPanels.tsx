// The Environments and Sessions panels on the left. An environment is a named set of folders;
// its sessions' tools may use every folder, and the first is the working directory. Each row
// is an icon and a title with a "more" menu; environments are created and edited in the
// environment window (EnvironmentWindow.tsx). Pinned environments and sessions come first.

import { ArrowLeft, FolderOpen, Pencil, Pin, PinOff, Trash } from "lucide-react";

import type { EnvironmentInfo, SessionStatus } from "@imperium/protocol";

import { RowMenu } from "../components/RowMenu";
import { Select } from "../components/Select";
import { ProjectIcon, environmentIcon, projectName, sameFolder } from "../projectIcons";
import { formatCost, sessionTitle, totals, type SessionState } from "../sessions";
import { StatusBadge } from "./StatusBadge";

/** The environment a session works in: the one it names, else the one whose first folder it uses. */
export function environmentOf(session: SessionState, environments: EnvironmentInfo[]): EnvironmentInfo | null {
  const id = session.summary.environment_id;
  if (id) return environments.find((env) => env.id === id) ?? null;
  return environments.find((env) => env.folders[0] && sameFolder(env.folders[0], session.summary.project_dir)) ?? null;
}

/** Pinned environments first, otherwise in the daemon's order (creation). */
export function sortEnvironments(environments: EnvironmentInfo[]): EnvironmentInfo[] {
  return environments
    .map((env, index) => ({ env, index }))
    .sort((a, b) => Number(!!b.env.pinned) - Number(!!a.env.pinned) || a.index - b.index)
    .map(({ env }) => env);
}

export type EnvironmentActions = {
  onEdit: (id: string) => void;
  onSetPinned: (env: EnvironmentInfo, pinned: boolean) => void;
  onDelete: (id: string) => void;
};

export function EnvironmentsPanel({
  environments,
  selected,
  connected,
  onSelect,
  actions,
}: {
  environments: EnvironmentInfo[];
  selected: string | null;
  connected: boolean;
  onSelect: (id: string) => void;
  actions: EnvironmentActions;
}) {
  if (environments.length === 0) {
    return (
      <div className="environments">
        <p className="empty">
          {connected ? "No environments yet. Use New environment to make one." : "No environments known yet."}
        </p>
      </div>
    );
  }
  return (
    <div className="environments">
      <ul className="list" aria-label="Environments">
        {sortEnvironments(environments).map((env) => {
          const isSelected = env.id === selected;
          return (
            <li key={env.id}>
              <div className="row nav-row">
                <button
                  className={`row-main nav-row-main${isSelected ? " selected" : ""}`}
                  aria-current={isSelected ? "true" : undefined}
                  title={env.folders.length ? env.folders.join("\n") : "No folders yet"}
                  onClick={() => onSelect(env.id)}
                  onDoubleClick={() => connected && actions.onEdit(env.id)}
                >
                  <ProjectIcon choice={environmentIcon(env)} />
                  <span className="name">{env.name}</span>
                  {env.pinned && <Pin className="pin-mark" aria-label="Pinned" />}
                </button>
                <RowMenu
                  label={`More for ${env.name}`}
                  items={[
                    { label: "Edit", icon: Pencil, onSelect: () => actions.onEdit(env.id), disabled: !connected },
                    env.pinned
                      ? { label: "Unpin", icon: PinOff, onSelect: () => actions.onSetPinned(env, false), disabled: !connected }
                      : { label: "Pin", icon: Pin, onSelect: () => actions.onSetPinned(env, true), disabled: !connected },
                    { label: "Delete", icon: Trash, danger: true, onSelect: () => actions.onDelete(env.id), disabled: !connected },
                  ]}
                />
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

export type SessionSort = "recent" | "environment" | "status";

const SORT_OPTIONS: { value: SessionSort; label: string }[] = [
  { value: "recent", label: "Recent" },
  { value: "environment", label: "By environment" },
  { value: "status", label: "By status" },
];

/** Status groups, in the order they are shown. */
const STATUS_GROUPS: { label: string; statuses: SessionStatus[] }[] = [
  { label: "Waiting for approval", statuses: ["needs_attention"] },
  { label: "Active", statuses: ["working", "watching"] },
  { label: "Paused", statuses: ["paused"] },
  { label: "Idle", statuses: ["idle"] },
  { label: "Merged", statuses: ["merged"] },
];

/** An environment's persistent agent first, then pinned sessions, then the most recently active. */
export function sortSessions(sessions: SessionState[]): SessionState[] {
  return [...sessions].sort(
    (a, b) =>
      Number(!!b.summary.persistent) - Number(!!a.summary.persistent) ||
      Number(!!b.summary.pinned) - Number(!!a.summary.pinned) ||
      b.updatedAt - a.updatedAt,
  );
}

type Group = { key: string; label: string | null; env?: EnvironmentInfo | null; sessions: SessionState[] };

/** The sessions as headed groups (one unheaded group for Recent). */
export function groupSessions(sessions: SessionState[], environments: EnvironmentInfo[], sort: SessionSort): Group[] {
  const sorted = sortSessions(sessions);
  if (sort === "status") {
    return STATUS_GROUPS.map(({ label, statuses }) => ({
      key: label,
      label,
      sessions: sorted.filter((s) => statuses.includes(s.status)),
    })).filter((group) => group.sessions.length > 0);
  }
  if (sort === "environment") {
    const groups: Group[] = sortEnvironments(environments).map((env) => ({
      key: env.id,
      label: env.name,
      env,
      sessions: sorted.filter((s) => environmentOf(s, environments)?.id === env.id),
    }));
    groups.push({
      key: "none",
      label: "No environment",
      env: null,
      sessions: sorted.filter((s) => !environmentOf(s, environments)),
    });
    return groups.filter((group) => group.sessions.length > 0);
  }
  return [{ key: "all", label: null, sessions: sorted }];
}

export function SessionsPanel({
  sessions,
  environments,
  scope,
  selected,
  error,
  sort,
  onSort,
  onSelect,
  onSetPinned,
  onShowEnvironment,
  onShowAll,
}: {
  /** Every session; filtered to `scope` when one is given. */
  sessions: SessionState[];
  environments: EnvironmentInfo[];
  scope: EnvironmentInfo | null;
  selected: string | null;
  /** Why the last attempt to start a session failed. */
  error: string | null;
  sort: SessionSort;
  onSort: (sort: SessionSort) => void;
  onSelect: (sessionId: string) => void;
  onSetPinned: (sessionId: string, pinned: boolean) => void;
  onShowEnvironment: (id: string) => void;
  onShowAll: () => void;
}) {
  const inScope = scope ? sessions.filter((s) => environmentOf(s, environments)?.id === scope.id) : sessions;
  // Within one environment, grouping by environment would be a single group.
  const groups = groupSessions(inScope, environments, scope && sort === "environment" ? "recent" : sort);

  const row = (session: SessionState) => {
    const id = session.summary.session_id;
    const env = environmentOf(session, environments);
    const place = env?.name ?? projectName(session.summary.project_dir);
    const pinned = !!session.summary.pinned;
    const title = session.summary.persistent ? `${place} agent` : sessionTitle(session);
    return (
      <li key={id}>
        <div className="row nav-row">
          <button
            className={`row-main nav-row-main${id === selected ? " selected" : ""}`}
            aria-current={id === selected ? "true" : undefined}
            onClick={() => onSelect(id)}
          >
            <ProjectIcon choice={environmentIcon(env ?? { id: session.summary.project_dir, icon: null, color: null })} />
            <span className="nav-row-text">
              <span className="name">
                {title}
                {pinned && <Pin className="pin-mark" aria-label="Pinned" />}
              </span>
              <span className="row-meta">
                {session.summary.persistent && (
                  <span className="badge persistent" title="Always available; keeps its conversation across restarts">
                    Persistent
                  </span>
                )}
                {(session.summary.worktrees?.length ?? 0) > 0 && (
                  <span
                    className="badge worktree"
                    title={session.summary.worktrees.map((tree) => `${tree.path} (${tree.branch})`).join("\n")}
                  >
                    Worktree
                  </span>
                )}
                <StatusBadge session={session} />
                <span className="detail">
                  {!scope && sort !== "environment" && `${place} · `}
                  {session.summary.model ?? "No model"} · {formatCost(totals(session.calls).costUsd)}
                </span>
              </span>
            </span>
          </button>
          <RowMenu
            label={`More for ${title}`}
            items={[
              pinned
                ? { label: "Unpin", icon: PinOff, onSelect: () => onSetPinned(id, false) }
                : { label: "Pin", icon: Pin, onSelect: () => onSetPinned(id, true) },
              {
                label: `Show ${place}`,
                icon: FolderOpen,
                onSelect: () => env && onShowEnvironment(env.id),
                disabled: !env || scope?.id === env.id,
              },
            ]}
          />
        </div>
      </li>
    );
  };

  return (
    <div className="sessions">
      {error && (
        <p className="error session-error" role="alert">
          {error}
        </p>
      )}
      <div className="sessions-toolbar">
        {scope ? (
          <div className="sessions-scope">
            <button className="back" onClick={onShowAll}>
              <ArrowLeft aria-hidden="true" /> All sessions
            </button>
            <span className="scope-name" title={scope.folders.join("\n")}>
              <ProjectIcon choice={environmentIcon(scope)} size="small" />
              <span>{scope.name}</span>
            </span>
          </div>
        ) : (
          <span className="spacer" />
        )}
        <Select
          aria-label="Sort sessions"
          className="sessions-sort"
          value={sort}
          onChange={(value) => onSort(value as SessionSort)}
          options={SORT_OPTIONS}
        />
      </div>
      {inScope.length === 0 ? (
        <p className="empty">{scope ? "No sessions in this environment." : "No sessions yet."}</p>
      ) : (
        groups.map((group) => (
          <section key={group.key} className="session-group" aria-label={group.label ?? undefined}>
            {group.label && (
              <h3 className="session-group-label">
                {group.env && <ProjectIcon choice={environmentIcon(group.env)} size="small" />}
                <span>{group.label}</span>
                <span className="session-group-count">{group.sessions.length}</span>
              </h3>
            )}
            <ul
              className="list"
              aria-label={group.label ?? (scope ? `Sessions in ${scope.name}` : "All sessions")}
            >
              {group.sessions.map(row)}
            </ul>
          </section>
        ))
      )}
    </div>
  );
}
