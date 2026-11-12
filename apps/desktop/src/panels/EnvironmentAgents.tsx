// The persistent agents of an environment, in its window's Agents section: a compact list (name,
// role, limits), and an edit view for one agent (add, rename, change the role or permissions, set
// its initiative) that replaces the list until saved or cancelled. A new environment has none. An
// agent's permissions only narrow what the environment allows: MCP servers and plugins (all, none
// or named) and a local-files-only lock. Each action is its own daemon call and applies at once;
// the daemon announces the changed environment (`environments.changed`), which redraws the list.

import { ChevronLeft, Pencil, Plus, Trash } from "lucide-react";
import { useEffect, useImperativeHandle, useState, type Ref } from "react";

import type {
  EnvironmentAgent,
  EnvironmentAgentCreated,
  EnvironmentInfo,
  InitiativeState,
  Notification,
  ToolAccess,
} from "@imperium/protocol";

import {
  type PermissionDraft,
  agentDraftDirty,
  describePermissions,
  draftOfPermissions,
  nameProblem,
  nameTaken,
  notStartedNotice,
  openSuggestions,
  permissionsChanged,
  permissionsOfDraft,
  suggestionsLabel,
} from "../agents";
import { Select } from "../components/Select";
import { Row, RowNote } from "../components/SettingsRow";
import { Switch } from "../components/Switch";
import type { DaemonClient } from "../daemon/client";
import { AgentInitiative } from "./AgentInitiative";

type Props = {
  /** The environment to manage the agents of; null while it is being created. */
  environment: EnvironmentInfo | null;
  client: DaemonClient | null;
  disabled?: boolean;
  /** Opens a session the user started from an agent's suggestion. */
  onOpenSession?: (sessionId: string) => void;
  /** Lets the window save the open agent form (Ctrl+S). */
  ref?: Ref<EnvironmentAgentsHandle>;
  /** The agent form was opened, changed or closed: `open` while shown, `dirty` while it holds unsaved changes. */
  onDraftChange?: (state: { open: boolean; dirty: boolean }) => void;
};

export type EnvironmentAgentsHandle = { save: () => void };

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

type Draft = {
  id: string | null;
  name: string;
  role: string;
  access: PermissionDraft;
  /** The permissions the form opened with: they are sent only when the user changed them. */
  initial: PermissionDraft;
};

export function EnvironmentAgents({ environment, client, disabled, onOpenSession, ref, onDraftChange }: Props) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** A note that is not an error: the agent was added but is not running yet. */
  const [notice, setNotice] = useState<string | null>(null);
  const agents = environment?.agents ?? [];
  const [suggestions, setSuggestions] = useState<Record<string, number>>({});

  const saved = draft?.id ? (agents.find((agent) => agent.id === draft.id) ?? null) : null;
  const isOpen = draft !== null;
  const dirty = draft !== null && agentDraftDirty(draft, saved);
  useEffect(() => {
    onDraftChange?.({ open: isOpen, dirty });
  }, [isOpen, dirty, onDraftChange]);

  // Open suggestions per agent with initiative on, for the badge on its row.
  const environmentId = environment?.id;
  const watched = agents
    .filter((agent) => (agent.initiative ?? "off") !== "off")
    .map((agent) => agent.id)
    .join(",");
  useEffect(() => {
    if (!client || !environmentId || !watched) {
      setSuggestions({});
      return;
    }
    let live = true;
    for (const agent_id of watched.split(",")) {
      client
        .call<InitiativeState>("initiative.get", { environment_id: environmentId, agent_id })
        .then((state) => live && setSuggestions((all) => ({ ...all, [agent_id]: openSuggestions(state).length })))
        .catch(() => {});
    }
    const stop = client.onNotification((notification: Notification) => {
      if (notification.method !== "initiative.changed") return;
      const state = notification.params as InitiativeState;
      if (live && state.environment_id === environmentId) {
        setSuggestions((all) => ({ ...all, [state.agent_id]: openSuggestions(state).length }));
      }
    });
    return () => {
      live = false;
      stop();
    };
  }, [client, environmentId, watched]);

  const run =async (work: (client: DaemonClient, environmentId: string) => Promise<unknown>) => {
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
    // Only spaces at the edges are dropped: a separator or a control there is refused, as the
    // daemon refuses it, not quietly trimmed.
    const name = draft.name.replace(/^ +| +$/g, "");
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
    const outcome: { created: EnvironmentAgentCreated | null; saved: boolean } = { created: null, saved: false };
    const done = await run(async (c, environment_id) => {
      if (draft.id === null) {
        outcome.created = await c.call<EnvironmentAgentCreated>("environments.agent_create", {
          environment_id,
          name,
          role: draft.role.trim(),
          permissions,
        });
      } else {
        outcome.created = await c.call<EnvironmentAgentCreated>("environments.agent_update", {
          environment_id,
          agent_id: draft.id,
          name,
          role: draft.role.trim(),
          permissions,
        });
        outcome.saved = true;
      }
    });
    if (done) {
      setDraft(null);
      setNotice(notStartedNotice(outcome.created, outcome.saved ? "was saved" : "was added"));
    }
  };

  useImperativeHandle(ref, () => ({ save: () => void save() }));

  const remove = async (agent: EnvironmentAgent) => {
    const done = await run((c, environment_id) =>
      c.call("environments.agent_delete", { environment_id, agent_id: agent.id }),
    );
    if (done) setDeleting(null);
  };

  const open = (next: Draft) => {
    setError(null);
    setDraft(next);
  };

  if (!environment) {
    return (
      <Row
        label="Agents"
        description="Persistent agents are always-available sessions that keep their conversation across days. Create the environment first, then add agents here."
      >
        <button type="button" disabled title="Create the environment first">
          <Plus aria-hidden="true" /> Add agent
        </button>
      </Row>
    );
  }

  if (draft) {
    const editing = draft.id === null ? null : (agents.find((agent) => agent.id === draft.id) ?? null);
    return (
      <>
        <Row
          label={draft.id === null ? "New agent" : `Edit ${editing?.name ?? draft.name}`}
          description="Name, role and permissions apply when you save the agent (Ctrl+S), not with the environment's Save. The Initiative select below applies as soon as you change it."
        >
          <button type="button" onClick={() => setDraft(null)} disabled={busy}>
            <ChevronLeft aria-hidden="true" /> Back to agents
          </button>
        </Row>
        {error && (
          <RowNote>
            <p className="error" role="alert">
              {error}
            </p>
          </RowNote>
        )}
        <Row label="Name" description="At most 60 characters; unique in this environment.">
          <input
            type="text"
            aria-label="Agent name"
            value={draft.name}
            placeholder="Planner"
            autoFocus
            onChange={(event) => setDraft({ ...draft, name: event.target.value })}
          />
        </Row>
        <Row label="Role" description="What this agent is for, for example: keep the roadmap current and critique open designs.">
          <textarea
            aria-label="Agent role"
            value={draft.role}
            rows={3}
            onChange={(event) => setDraft({ ...draft, role: event.target.value })}
          />
        </Row>
        <RowNote>
          <p className="muted agent-access-hint">
            Permissions only narrow what this environment already allows; its permission mode and safety checks still
            apply. They also bind the sessions this agent starts.
          </p>
        </RowNote>
        <Row
          label="Local files only"
          description="No web, browser, search, MCP, plugin or command tools; files stay inside this environment’s folders and the agent’s scratch folder."
        >
          <Switch
            label="Local files only"
            checked={draft.access.localOnly}
            onChange={(localOnly) => setDraft({ ...draft, access: { ...draft.access, localOnly } })}
          />
        </Row>
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
        <Row label={draft.id === null ? "Add this agent" : "Save this agent"}>
          <span className="row-actions">
            <button type="button" onClick={() => setDraft(null)} disabled={busy}>
              Cancel
            </button>
            <button type="button" className="primary" disabled={busy || !draft.name.trim()} onClick={() => void save()}>
              {draft.id === null ? "Add" : "Save agent"}
            </button>
          </span>
        </Row>
        {editing && (
          <RowNote>
            <AgentInitiative
              environmentId={environment.id}
              agent={editing}
              client={client}
              disabled={disabled}
              onOpenSession={onOpenSession}
            />
          </RowNote>
        )}
      </>
    );
  }

  return (
    <>
      <Row
        label="Agents"
        description="Persistent agents are always-available sessions that keep their conversation across days. An environment has none until you add one."
      >
        <button
          type="button"
          onClick={() =>
            open({
              id: null,
              name: "",
              role: "",
              access: draftOfPermissions(undefined),
              initial: draftOfPermissions(undefined),
            })
          }
          disabled={disabled || busy}
          title="Add a persistent agent"
        >
          <Plus aria-hidden="true" /> Add agent
        </button>
      </Row>
      <RowNote>
        <ul className="environment-agents" aria-label="Agents">
          {agents.map((agent) => (
            <li key={agent.id}>
              <div className="agent-row">
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
                {(agent.initiative ?? "off") !== "off" && (
                  <span className="agent-limits" title="What the agent does on its own when idle">
                    Initiative: {agent.initiative}
                  </span>
                )}
                {suggestionsLabel(suggestions[agent.id] ?? 0) && (
                  <span
                    className="agent-limits agent-suggestions-badge"
                    title="Suggestions waiting for you: open Edit to start or dismiss them"
                  >
                    {suggestionsLabel(suggestions[agent.id] ?? 0)}
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
                      title="Rename, or change the role, permissions or initiative"
                      disabled={disabled || busy}
                      onClick={() =>
                        open({
                          id: agent.id,
                          name: agent.name,
                          role: agent.role,
                          access: draftOfPermissions(agent.permissions),
                          initial: draftOfPermissions(agent.permissions),
                        })
                      }
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
              </div>
            </li>
          ))}
          {agents.length === 0 && (
            <li className="muted">No agents. Add one to get an always-available assistant for this environment.</li>
          )}
        </ul>
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
      </RowNote>
    </>
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
    <>
      <Row label={label}>
        <Select
          aria-label={label}
          value={access}
          disabled={disabled}
          options={[
            { value: "all", label: "All the environment allows" },
            { value: "none", label: "None" },
            { value: "named", label: "Only these" },
          ]}
          onChange={(value) => onAccess(value as ToolAccess)}
        />
      </Row>
      {access === "named" && !disabled && (
        <Row label={namesLabel} description="Separated by commas." sub>
          <input
            type="text"
            value={names}
            placeholder={placeholder}
            aria-label={namesLabel}
            onChange={(event) => onNames(event.target.value)}
          />
        </Row>
      )}
    </>
  );
}
