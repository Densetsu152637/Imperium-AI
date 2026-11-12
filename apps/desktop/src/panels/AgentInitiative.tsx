// A persistent agent's initiative, under its row in the environment window: how much it takes
// (off, suggest, act), where it stands (reviewing, waiting and why, limits in act mode), the
// work it suggests with one-click Start and Dismiss, and its latest reminder of what is left. The
// mode is saved at once; the state follows `initiative.changed`.

import { useEffect, useState } from "react";

import type {
  EnvironmentAgent,
  Initiative,
  InitiativeStartResult,
  InitiativeState,
  Notification,
} from "@imperium/protocol";

import { INITIATIVE_OPTIONS, actLimits, initiativeHeadline, openSuggestions } from "../agents";
import { Select } from "../components/Select";
import type { DaemonClient } from "../daemon/client";

type Props = {
  environmentId: string;
  agent: EnvironmentAgent;
  client: DaemonClient | null;
  disabled?: boolean;
  /** Opens a session the user started from a suggestion. */
  onOpenSession?: (sessionId: string) => void;
};

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

export function AgentInitiative({ environmentId, agent, client, disabled, onOpenSession }: Props) {
  const [state, setState] = useState<InitiativeState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mode = agent.initiative ?? "off";

  // The state now, and as it changes while the window is open.
  useEffect(() => {
    if (!client || mode === "off") {
      setState(null);
      return;
    }
    let live = true;
    client
      .call<InitiativeState>("initiative.get", { environment_id: environmentId, agent_id: agent.id })
      .then((result) => live && setState(result))
      .catch(() => {});
    const stop = client.onNotification((notification: Notification) => {
      if (notification.method !== "initiative.changed") return;
      const next = notification.params as InitiativeState;
      if (next.agent_id === agent.id) setState(next);
    });
    return () => {
      live = false;
      stop();
    };
  }, [client, environmentId, agent.id, mode]);

  const attempt = async (work: (client: DaemonClient) => Promise<unknown>) => {
    if (!client) return;
    setBusy(true);
    setError(null);
    try {
      await work(client);
    } catch (failure) {
      setError(message(failure));
    } finally {
      setBusy(false);
    }
  };

  const setMode = (next: Initiative) =>
    attempt((c) => c.call("environments.agent_update", { environment_id: environmentId, agent_id: agent.id, initiative: next }));

  const ref = { environment_id: environmentId, agent_id: agent.id };
  const open = openSuggestions(state);
  const handled = (state?.suggestions ?? []).filter((suggestion) => suggestion.status !== "open").slice(0, 3);

  return (
    <div className="agent-initiative">
      <div className="agent-initiative-head">
        <span className="environment-label" title="What the agent does on its own, when the machine and this environment are idle">
          Initiative
        </span>
        <Select
          aria-label={`Initiative of ${agent.name}`}
          value={mode}
          disabled={disabled || busy || !client}
          options={INITIATIVE_OPTIONS.map(({ value, label, description }) => ({ value, label, description }))}
          onChange={(value) => void setMode(value as Initiative)}
        />
        {mode !== "off" && (
          <button
            type="button"
            disabled={disabled || busy || !client || !!state?.reviewing}
            title="Ask the agent to review the environment now, without waiting for idle time"
            onClick={() => void attempt((c) => c.call("initiative.review", ref))}
          >
            Review now
          </button>
        )}
      </div>
      {mode !== "off" && state && (
        <>
          <p className="muted agent-initiative-line">
            {initiativeHeadline(state)}
            {state.mode === "act" && <> {actLimits(state)}.</>}
          </p>
          {open.length > 0 && (
            <ul className="agent-suggestions" aria-label={`Suggestions from ${agent.name}`}>
              {open.map((suggestion) => (
                <li key={suggestion.id}>
                  <div className="agent-suggestion-text">
                    <strong>{suggestion.title}</strong>
                    <span className="muted">{suggestion.reason}</span>
                    <span className="muted" title="What the agent read">
                      From {suggestion.source}
                    </span>
                    <em title="What the new session is told">“{suggestion.prompt}”</em>
                  </div>
                  <div className="agent-suggestion-actions">
                    <button
                      type="button"
                      className="primary"
                      disabled={disabled || busy}
                      onClick={() =>
                        void attempt(async (c) => {
                          const started = await c.call<InitiativeStartResult>("initiative.start", {
                            ...ref,
                            suggestion_id: suggestion.id,
                          });
                          onOpenSession?.(started.session_id);
                        })
                      }
                    >
                      Start
                    </button>
                    <button
                      type="button"
                      disabled={disabled || busy}
                      onClick={() => void attempt((c) => c.call("initiative.dismiss", { ...ref, suggestion_id: suggestion.id }))}
                    >
                      Dismiss
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
          {handled.length > 0 && (
            <ul className="muted agent-suggestions-done" aria-label="Handled suggestions">
              {handled.map((suggestion) => (
                <li key={suggestion.id}>
                  {suggestion.title} · {suggestion.status === "started" ? "started" : "dismissed"}
                </li>
              ))}
            </ul>
          )}
          {state.digest && state.digest.items.length > 0 && (
            <div className="agent-digest">
              <span className="environment-label">Work left</span>
              <p className="muted">{state.digest.summary}</p>
              <ul>
                {state.digest.items.map((item, index) => (
                  <li key={index}>
                    {item.text} <span className="muted">· {item.source}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
