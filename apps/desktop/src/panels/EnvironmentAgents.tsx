// The persistent agents of an environment, in its window: list, add, rename or change the role or
// permissions of, and delete. A new environment has none. An agent's permissions only narrow what
// the environment allows: MCP servers and plugins (all, none or named) and a local-files-only lock. Each action is its own daemon call and applies at once;
// the daemon announces the changed environment (`environments.changed`), which redraws the list.

import { Pencil, Plus, Trash, X } from "lucide-react";
import { useState } from "react";

import type { EnvironmentAgent, EnvironmentAgentCreated, EnvironmentInfo, ToolAccess } from "@imperium/protocol";

import {
  type PermissionDraft,
  describePermissions,
  draftOfPermissions,
  nameProblem,
  nameTaken,
  notStartedNotice,
  permissionsChanged,
  permissionsOfDraft,
} from "../agents";
import type { DaemonClient } from "../daemon/client";

type Props = {
  /** The environment to manage the agents of; null while it is being created. */
  environment: EnvironmentInfo | null;
  client: DaemonClient | null;
  disabled?: boolean;
};

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

type Draft = {
  id: string | null;
  name: string;
  role: string;
  access: PermissionDraft;
  /** The permissions the form opened with: they are sent only when the user changed them. */
  initial: PermissionDraft;
};

export function EnvironmentAgents({ environment, client, disabled }: Props) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** A note that is not an error: the agent was added but is not running yet. */
  const [notice, setNotice] = useState<string | null>(null);
  const agents = environment?.agents ?? [];

  const run = async (work: (client: DaemonClient, environmentId: string) => Promise<unknown>) => {
    if (!client || !environment) return false;
    setBusy(true);
    setError(null);
    try {
      await work(client, environment.id);
      return true;
    } catch (failure) {
      setError(message(failure));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    if (!draft) return;
    const name = draft.name.trim();
    const problem = nameProblem(name);
    if (problem) {
      setError(problem);
      return;
    }
    if (nameTaken(agents, name, draft.id ?? undefined)) {
      setError(`This environment already has an agent named “${name}”.`);
      return;
    }
    setNotice(null);
    const permissions = permissionsChanged(draft.initial, draft.access) ? permissionsOfDraft(draft.access) : undefined;
    const outcome: { created: EnvironmentAgentCreated | null } = { created: null };
    const done = await run(async (c, environment_id) => {
      if (draft.id === null) {
        outcome.created = await c.call<EnvironmentAgentCreated>("environments.agent_create", {
          environment_id,
          name,
          role: draft.role.trim(),
          permissions,
        });
      } else {
        await c.call("environments.agent_update", {
          environment_id,
          agent_id: draft.id,
          name,
          role: draft.role.trim(),
          permissions,
        });
      }
    });
    if (done) {
      setDraft(null);
      setNotice(notStartedNotice(outcome.created));
    }
  };

  const remove = async (agent: EnvironmentAgent) => {
    const done = await run((c, environment_id) =>
      c.call("environments.agent_delete", { environment_id, agent_id: agent.id }),
    );
    if (done) setDeleting(null);
  };

  return (
    <section className="environment-section" aria-label="Agents">
      <div className="environment-section-head">
        <span
          className="environment-label"
          title="Persistent agents are always-available sessions that keep their conversation across days. An environment has none until you add one."
        >
          Agents
        </span>
        <button
          type="button"
          onClick={() => {
            setError(null);
            setDraft({
              id: null,
              name: "",
              role: "",
              access: draftOfPermissions(undefined),
              initial: draftOfPermissions(undefined),
            });
          }}
          disabled={disabled || busy || !environment || draft !== null}
          title={environment ? "Add a persistent agent" : "Create the environment first"}
        >
          <Plus aria-hidden="true" /> Add agent
        </button>
      </div>
      <ul className="environment-agents" aria-label="Agents">
        {agents.map((agent) => (
          <li key={agent.id}>
            <span className="agent-name">{agent.name}</span>
            <span className="muted agent-role" title={agent.role}>
              {agent.role || "No role set"}
            </span>
            {describePermissions(agent.permissions) && (
              <span
                className="agent-limits"
                title="This agent can only use what its permissions allow, on top of the environment's own."
              >
                {describePermissions(agent.permissions)}
              </span>
            )}
            {!agent.session_id && (
              <span className="muted" title="The agent is not running yet; it starts once the environment can run it.">
                not running
              </span>
            )}
            {deleting === agent.id ? (
              <span className="environment-delete-confirm" role="group" aria-label={`Delete ${agent.name}`}>
                <span>Delete? Its history is archived.</span>
                <button type="button" className="danger" disabled={busy} onClick={() => void remove(agent)} autoFocus>
                  Delete
                </button>
                <button type="button" onClick={() => setDeleting(null)}>
                  Keep
                </button>
              </span>
            ) : (
              <>
                <button
                  type="button"
                  className="ghost icon-only row-tool"
                  aria-label={`Edit ${agent.name}`}
                  title="Rename, or change the role or permissions"
                  disabled={disabled || busy}
                  onClick={() => {
                    setError(null);
                    setDraft({
                      id: agent.id,
                      name: agent.name,
                      role: agent.role,
                      access: draftOfPermissions(agent.permissions),
                      initial: draftOfPermissions(agent.permissions),
                    });
                  }}
                >
                  <Pencil />
                </button>
                <button
                  type="button"
                  className="ghost icon-only row-tool"
                  aria-label={`Delete ${agent.name}`}
                  title="Delete the agent (its session is archived)"
                  disabled={disabled || busy}
                  onClick={() => setDeleting(agent.id)}
                >
                  <Trash />
                </button>
              </>
            )}
          </li>
        ))}
        {agents.length === 0 && draft === null && (
          <li className="muted">No agents. Add one to get an always-available assistant for this environment.</li>
        )}
      </ul>
      {draft && (
        <div className="environment-agent-form">
          <label className="environment-field">
            <span className="environment-label">Name</span>
            <input
              value={draft.name}
              placeholder="Planner"
              autoFocus
              onChange={(event) => setDraft({ ...draft, name: event.target.value })}
            />
          </label>
          <label className="environment-field">
            <span className="environment-label">Role</span>
            <textarea
              value={draft.role}
              rows={3}
              placeholder="What this agent is for, for example: keep the roadmap current and critique open designs."
              onChange={(event) => setDraft({ ...draft, role: event.target.value })}
            />
          </label>
          <fieldset className="agent-access">
            <legend className="environment-label">Permissions</legend>
            <p className="muted">
              These only narrow what this environment already allows; its permission mode and safety checks still
              apply. They also bind the sessions this agent starts.
            </p>
            <label className="agent-access-lock">
              <input
                type="checkbox"
                checked={draft.access.localOnly}
                onChange={(event) => setDraft({ ...draft, access: { ...draft.access, localOnly: event.target.checked } })}
              />
              <span>
                Local files only: no web, browser, search, MCP, plugin or command tools; files stay inside this
                environment’s folders and the agent’s scratch folder.
              </span>
            </label>
            <AccessRow
              label="MCP servers"
              namesLabel="MCP server names"
              placeholder="github, slack"
              access={draft.access.mcp}
              names={draft.access.mcpNames}
              disabled={draft.access.localOnly}
              onAccess={(mcp) => setDraft({ ...draft, access: { ...draft.access, mcp } })}
              onNames={(mcpNames) => setDraft({ ...draft, access: { ...draft.access, mcpNames } })}
            />
            <AccessRow
              label="Plugins"
              namesLabel="Plugin names"
              placeholder="docs, search"
              access={draft.access.plugins}
              names={draft.access.pluginNames}
              disabled={draft.access.localOnly}
              onAccess={(plugins) => setDraft({ ...draft, access: { ...draft.access, plugins } })}
              onNames={(pluginNames) => setDraft({ ...draft, access: { ...draft.access, pluginNames } })}
            />
          </fieldset>
          <div className="runtime-row">
            <button type="button" className="primary" disabled={busy || !draft.name.trim()} onClick={() => void save()}>
              {draft.id === null ? "Add" : "Save agent"}
            </button>
            <button type="button" onClick={() => setDraft(null)}>
              <X aria-hidden="true" /> Cancel
            </button>
          </div>
        </div>
      )}
      {notice && (
        <p className="muted" role="status">
          {notice}
        </p>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/** One allowlist: all, none, or only the named ones. */
function AccessRow({
  label,
  namesLabel,
  placeholder,
  access,
  names,
  disabled,
  onAccess,
  onNames,
}: {
  label: string;
  namesLabel: string;
  placeholder: string;
  access: ToolAccess;
  names: string;
  disabled: boolean;
  onAccess: (access: ToolAccess) => void;
  onNames: (names: string) => void;
}) {
  return (
    <div className="agent-access-row">
      <label className="environment-field">
        <span className="environment-label">{label}</span>
        <select value={access} disabled={disabled} onChange={(event) => onAccess(event.target.value as ToolAccess)}>
          <option value="all">All the environment allows</option>
          <option value="none">None</option>
          <option value="named">Only these</option>
        </select>
      </label>
      {access === "named" && !disabled && (
        <label className="environment-field">
          <span className="environment-label">{namesLabel}</span>
          <input
            value={names}
            placeholder={placeholder}
            aria-label={namesLabel}
            onChange={(event) => onNames(event.target.value)}
          />
        </label>
      )}
    </div>
  );
}
