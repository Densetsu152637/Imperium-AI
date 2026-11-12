// The persistent agents of an environment, in its window: list, add, rename or change the role of,
// and delete. A new environment has none. Each action is its own daemon call and applies at once;
// the daemon announces the changed environment (`environments.changed`), which redraws the list.

import { Pencil, Plus, Trash, X } from "lucide-react";
import { useState } from "react";

import type { EnvironmentAgent, EnvironmentInfo } from "@imperium/protocol";

import { nameTaken } from "../agents";
import type { DaemonClient } from "../daemon/client";

type Props = {
  /** The environment to manage the agents of; null while it is being created. */
  environment: EnvironmentInfo | null;
  client: DaemonClient | null;
  disabled?: boolean;
};

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

type Draft = { id: string | null; name: string; role: string };

export function EnvironmentAgents({ environment, client, disabled }: Props) {
  const [draft, setDraft] = useState<Draft | null>(null);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
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
    if (!name) {
      setError("An agent needs a name.");
      return;
    }
    if (nameTaken(agents, name, draft.id ?? undefined)) {
      setError(`This environment already has an agent named “${name}”.`);
      return;
    }
    const done = await run((c, environment_id) =>
      draft.id === null
        ? c.call("environments.agent_create", { environment_id, name, role: draft.role.trim() })
        : c.call("environments.agent_update", { environment_id, agent_id: draft.id, name, role: draft.role.trim() }),
    );
    if (done) setDraft(null);
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
            setDraft({ id: null, name: "", role: "" });
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
                  title="Rename or change the role"
                  disabled={disabled || busy}
                  onClick={() => {
                    setError(null);
                    setDraft({ id: agent.id, name: agent.name, role: agent.role });
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
              maxLength={60}
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
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
