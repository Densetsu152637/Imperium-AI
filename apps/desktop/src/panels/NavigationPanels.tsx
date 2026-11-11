// The Environments and Sessions panels on the left. An environment is a named set of folders;
// its sessions' tools may use every folder, and the first is the working directory. Each row
// is an icon and a title with a "more" menu; environments are created and edited in the
// environment window (EnvironmentWindow.tsx). A session row is one line: a status dot, its title,
// and how long ago it was active, with the rest in its tooltip; double-click the title, or use
// the "more" menu, to rename it. Pinned environments and sessions come first;
// archived sessions are kept apart in a collapsed Archived group at the bottom. A search box above
// the sessions searches what was said and done in them (SessionSearch.tsx).

import { Archive, ArchiveRestore, ArrowLeft, FolderOpen, Pencil, Pin, PinOff, Trash } from "lucide-react";
import { useState, type KeyboardEvent } from "react";

import type { EnvironmentInfo, SessionStatus } from "@imperium/protocol";

import { RowMenu } from "../components/RowMenu";
import { Select } from "../components/Select";
import { ProjectIcon, environmentIcon, projectName, sameFolder } from "../projectIcons";
import type { SearchClient } from "../sessionSearch";
import { formatAgo } from "../ago";
import { formatCost, sessionTitle, totals, type SessionState } from "../sessions";
import { SessionSearch } from "./SessionSearch";
import { statusText } from "./StatusBadge";

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
  sort,
  onSort,
  onSelect,
  onSetPinned,
  onSetArchived,
  onRename,
  onShowEnvironment,
  onShowAll,
  client = null,
  onOpenHit,
}: {
  /** Every session; filtered to `scope` when one is given. */
  sessions: SessionState[];
  environments: EnvironmentInfo[];
  scope: EnvironmentInfo | null;
  selected: string | null;
  sort: SessionSort;
  onSort: (sort: SessionSort) => void;
  onSelect: (sessionId: string) => void;
  onSetPinned: (sessionId: string, pinned: boolean) => void;
  /** Archives a session (`true`) or resumes an archived one. */
  onSetArchived: (sessionId: string, archived: boolean) => void;
  /** Renames a session; an empty title lets it be titled automatically again. */
  onRename: (sessionId: string, title: string) => void;
  onShowEnvironment: (id: string) => void;
  onShowAll: () => void;
  /** Searches sessions; no search box without one. */
  client?: SearchClient | null;
  /** Opens a session at a search hit's history event. */
  onOpenHit?: (sessionId: string, eventIndex: number) => void;
}) {
  const [searchActive, setSearching] = useState(false);
  // The session whose title is being edited in place.
  const [renaming, setRenaming] = useState<string | null>(null);
  // While a query is entered, its results stand in for the list.
  const searching = searchActive && !!client && !!onOpenHit;
  const inScope = scope ? sessions.filter((s) => environmentOf(s, environments)?.id === scope.id) : sessions;
  const active = inScope.filter((s) => !s.summary.archived);
  const archived = sortSessions(inScope.filter((s) => s.summary.archived)).sort(
    (a, b) => (b.summary.archived_at ?? 0) - (a.summary.archived_at ?? 0),
  );
  // Within one environment, grouping by environment would be a single group.
  const groups = groupSessions(active, environments, scope && sort === "environment" ? "recent" : sort);

  const row = (session: SessionState) => {
    const id = session.summary.session_id;
    const env = environmentOf(session, environments);
    const place = env?.name ?? projectName(session.summary.project_dir);
    const pinned = !!session.summary.pinned;
    const persistent = !!session.summary.persistent;
    const title = persistent ? `${place} agent` : sessionTitle(session);
    const status = statusText(session);
    // `updatedAt` is a time once the session has had an event here, else only an ordering key.
    const at = session.summary.archived
      ? session.summary.archived_at
      : Math.max(session.updatedAt, session.summary.created_at ?? 0);
    // Everything but the title and time is on hover.
    const hover = [
      title,
      status,
      place,
      `${session.summary.model ?? "No model"} · ${formatCost(totals(session.calls).costUsd)}`,
      persistent ? "Persistent: always available, keeps its conversation across restarts" : null,
      ...(session.summary.worktrees ?? []).map((tree) => `Worktree ${tree.path} (${tree.branch})`),
    ]
      .filter(Boolean)
      .join("\n");
    // Enter and leaving the field keep the new title; Escape keeps the old one.
    const finishRename = (input: HTMLInputElement) => {
      setRenaming(null);
      const value = input.value.trim();
      if (input.dataset.cancel !== "true" && value !== title) onRename(id, value);
    };
    const onRenameKey = (event: KeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        event.currentTarget.dataset.cancel = "true";
      }
      if (event.key === "Enter" || event.key === "Escape") event.currentTarget.blur();
    };
    return (
      <li key={id}>
        <div className="row nav-row session-row">
          {renaming === id ? (
            <input
              className="session-rename"
              aria-label="Session title"
              defaultValue={title}
              autoFocus
              onFocus={(event) => event.currentTarget.select()}
              onKeyDown={onRenameKey}
              onBlur={(event) => finishRename(event.currentTarget)}
            />
          ) : (
            <button
              className={`row-main session-row-main${id === selected ? " selected" : ""}`}
              aria-current={id === selected ? "true" : undefined}
              title={hover}
              onClick={() => onSelect(id)}
              onDoubleClick={() => !persistent && setRenaming(id)}
            >
              <span className={`status-dot ${session.status}`} role="img" aria-label={status} />
              <span className="name">{title}</span>
              {pinned && <Pin className="pin-mark" aria-label="Pinned" />}
              <span className="session-ago">{formatAgo(at)}</span>
            </button>
          )}
          <RowMenu
            label={`More for ${title}`}
            items={[
              { label: "Rename", icon: Pencil, onSelect: () => setRenaming(id), disabled: persistent },
              ...(session.summary.archived
                ? [{ label: "Unarchive", icon: ArchiveRestore, onSelect: () => onSetArchived(id, false) }]
                : [
                    pinned
                      ? { label: "Unpin", icon: PinOff, onSelect: () => onSetPinned(id, false) }
                      : { label: "Pin", icon: Pin, onSelect: () => onSetPinned(id, true) },
                    {
                      label: "Archive",
                      icon: Archive,
                      onSelect: () => onSetArchived(id, true),
                      // An environment's agent runs while the daemon does.
                      disabled: persistent,
                    },
                  ]),
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
      {client && onOpenHit && (
        <SessionSearch
          client={client}
          environmentId={scope?.id ?? null}
          onOpen={onOpenHit}
          onActiveChange={setSearching}
        />
      )}
      {searching ? null : active.length === 0 ? (
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
      {!searching && archived.length > 0 && (
        <details className="session-group archived-sessions">
          <summary className="session-group-label">
            <Archive aria-hidden="true" />
            <span>Archived</span>
            <span className="session-group-count">{archived.length}</span>
          </summary>
          <ul className="list" aria-label="Archived sessions">
            {archived.map(row)}
          </ul>
        </details>
      )}
    </div>
  );
}
