// The Environments and Sessions panels on the left. An environment is a named set of folders;
// its sessions' tools may use every folder, and the first is the working directory.

import { useEffect, useRef, useState } from "react";

import type { EnvironmentInfo } from "@imperium/protocol";

import {
  IconPicker,
  ProjectIcon,
  environmentIcon,
  projectName,
  sameFolder,
  type ProjectIconChoice,
} from "../projectIcons";
import { formatCost, sessionTitle, totals, type SessionState } from "../sessions";
import { StatusBadge } from "./StatusBadge";

/** The environment a session works in: the one it names, else the one whose first folder it uses. */
export function environmentOf(session: SessionState, environments: EnvironmentInfo[]): EnvironmentInfo | null {
  const id = session.summary.environment_id;
  if (id) return environments.find((env) => env.id === id) ?? null;
  return environments.find((env) => env.folders[0] && sameFolder(env.folders[0], session.summary.project_dir)) ?? null;
}

export type EnvironmentActions = {
  onCreate: (name: string) => Promise<boolean>;
  onRename: (id: string, name: string) => void;
  onDelete: (id: string) => void;
  onAddFolder: (id: string) => void;
  onRemoveFolder: (id: string, folder: string) => void;
  onSetIcon: (id: string, choice: ProjectIconChoice | null) => void;
};

export function EnvironmentsPanel({
  environments,
  selected,
  creating,
  connected,
  onSelect,
  onCancelCreate,
  actions,
}: {
  environments: EnvironmentInfo[];
  selected: string | null;
  /** Whether the "New environment" form is open. */
  creating: boolean;
  connected: boolean;
  onSelect: (id: string) => void;
  onCancelCreate: () => void;
  actions: EnvironmentActions;
}) {
  const [editingIcon, setEditingIcon] = useState<string | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  return (
    <div className="environments">
      {creating && <NameForm label="New environment" submit="Create" onSubmit={actions.onCreate} onCancel={onCancelCreate} />}
      {environments.length === 0 && !creating ? (
        <p className="empty">
          {connected ? "No environments yet. Use New environment, then add folders to it." : "No environments known yet."}
        </p>
      ) : (
        <ul className="list" aria-label="Environments">
          {environments.map((env) => {
            const choice = environmentIcon(env);
            const isSelected = env.id === selected;
            return (
              <li key={env.id}>
                <div className="row">
                  <button
                    className="icon-button"
                    aria-label={`Change icon for ${env.name}`}
                    aria-expanded={editingIcon === env.id}
                    title="Change icon"
                    disabled={!connected}
                    onClick={() => setEditingIcon((open) => (open === env.id ? null : env.id))}
                  >
                    <ProjectIcon choice={choice} />
                  </button>
                  <button
                    className={`row-main${isSelected ? " selected" : ""}`}
                    aria-current={isSelected ? "true" : undefined}
                    onClick={() => onSelect(env.id)}
                  >
                    <span className="name">{env.name}</span>
                    <span className="detail">
                      {env.folders.length === 0
                        ? "no folders"
                        : env.folders.length === 1
                          ? env.folders[0]
                          : `${projectName(env.folders[0])} + ${env.folders.length - 1} more`}
                    </span>
                  </button>
                </div>
                {editingIcon === env.id && (
                  <IconPicker
                    name={env.name}
                    choice={choice}
                    onChange={(next) => actions.onSetIcon(env.id, next)}
                    onReset={() => actions.onSetIcon(env.id, null)}
                    onClose={() => setEditingIcon(null)}
                  />
                )}
                {isSelected && (
                  <div className="environment-detail">
                    {renaming === env.id ? (
                      <NameForm
                        label={`Rename ${env.name}`}
                        submit="Rename"
                        initial={env.name}
                        onSubmit={async (name) => {
                          actions.onRename(env.id, name);
                          setRenaming(null);
                          return true;
                        }}
                        onCancel={() => setRenaming(null)}
                      />
                    ) : null}
                    <ul className="folders" aria-label={`Folders of ${env.name}`}>
                      {env.folders.map((folder, index) => (
                        <li key={folder} className="folder">
                          <span className="folder-path" title={folder}>
                            {folder}
                            {index === 0 && env.folders.length > 1 && <span className="muted"> · working directory</span>}
                          </span>
                          <button
                            className="ghost"
                            aria-label={`Remove ${folder} from ${env.name}`}
                            title="Remove folder (the folder itself is untouched)"
                            disabled={!connected}
                            onClick={() => actions.onRemoveFolder(env.id, folder)}
                          >
                            ×
                          </button>
                        </li>
                      ))}
                      {env.folders.length === 0 && <li className="muted">Add a folder to start sessions here.</li>}
                    </ul>
                    <div className="environment-actions">
                      <button onClick={() => actions.onAddFolder(env.id)} disabled={!connected}>
                        Add folder
                      </button>
                      <button onClick={() => setRenaming(env.id)} disabled={!connected}>
                        Rename
                      </button>
                      {deleting === env.id ? (
                        <>
                          <button
                            className="danger"
                            onClick={() => {
                              setDeleting(null);
                              actions.onDelete(env.id);
                            }}
                          >
                            Delete {env.name}
                          </button>
                          <button onClick={() => setDeleting(null)}>Keep</button>
                        </>
                      ) : (
                        <button onClick={() => setDeleting(env.id)} disabled={!connected}>
                          Delete
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}

/** A one-field form for a name; Escape cancels. */
function NameForm({
  label,
  submit,
  initial = "",
  onSubmit,
  onCancel,
}: {
  label: string;
  submit: string;
  initial?: string;
  /** Resolves to whether the form is done (it stays open on failure). */
  onSubmit: (name: string) => Promise<boolean>;
  onCancel: () => void;
}) {
  const [name, setName] = useState(initial);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.focus(), []);
  return (
    <form
      className="name-form"
      aria-label={label}
      onSubmit={async (event) => {
        event.preventDefault();
        if (!name.trim() || busy) return;
        setBusy(true);
        const done = await onSubmit(name.trim());
        setBusy(false);
        if (done) setName("");
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onCancel();
        }
      }}
    >
      <input ref={input} aria-label={label} placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} />
      <button type="submit" className="primary" disabled={!name.trim() || busy}>
        {submit}
      </button>
      <button type="button" onClick={onCancel}>
        Cancel
      </button>
    </form>
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
  environments,
  scope,
  selected,
  error,
  onSelect,
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
  onSelect: (sessionId: string) => void;
  onShowEnvironment: (id: string) => void;
  onShowAll: () => void;
}) {
  const shown = sortSessions(
    scope ? sessions.filter((s) => environmentOf(s, environments)?.id === scope.id) : sessions,
  );
  return (
    <div className="sessions">
      {error && (
        <p className="error session-error" role="alert">
          {error}
        </p>
      )}
      {scope && (
        <div className="sessions-scope">
          <button className="back" onClick={onShowAll}>
            <span aria-hidden="true">←</span> All sessions
          </button>
          <span className="scope-name" title={scope.folders.join("\n")}>
            <ProjectIcon choice={environmentIcon(scope)} size="small" />
            <span>{scope.name}</span>
          </span>
        </div>
      )}
      {shown.length === 0 ? (
        <p className="empty">{scope ? "No sessions in this environment." : "No sessions yet."}</p>
      ) : (
        <ul className="list" aria-label={scope ? `Sessions in ${scope.name}` : "All sessions"}>
          {shown.map((session) => {
            const id = session.summary.session_id;
            const env = environmentOf(session, environments);
            const place = env?.name ?? projectName(session.summary.project_dir);
            return (
              <li key={id}>
                <div className="row">
                  <button
                    className="icon-button"
                    aria-label={`Show sessions in ${place}`}
                    title={`Show sessions in ${place}`}
                    disabled={!env}
                    onClick={() => env && onShowEnvironment(env.id)}
                  >
                    <ProjectIcon
                      choice={environmentIcon(env ?? { id: session.summary.project_dir, icon: null, color: null })}
                    />
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
                        {!scope && `${place} · `}
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
