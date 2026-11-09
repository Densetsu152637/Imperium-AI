// The middle panel while no session is open (and after New session): a composer for the first
// message with the new session's options under it — its environment (the selected one), its
// model (the configured default, or none), and, for an environment with a git folder, whether it
// starts on worktrees. Sending creates the session with those options and sends the message.

import { ArrowUp } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import type { ConfigDescribeResult, EnvironmentInfo, LocalModelRow, ModelRow } from "@imperium/protocol";

import { Select } from "../components/Select";
import { Switch } from "../components/Switch";
import type { DaemonClient } from "../daemon/client";
import { useSettings } from "../settings";
import { asSpec, modelOptions } from "./ModelSelect";
import { ModelSetup } from "./ModelSetup";

export type NewSessionOptions = {
  environmentId: string;
  /** A model spec, or null for none (the session waits for one). */
  model: string | null;
  worktree: boolean;
  text: string;
};

type Props = {
  environments: EnvironmentInfo[];
  /** Ids of environments with a folder in a git repository (they can start on worktrees). */
  gitEnvironments: string[];
  /** The selected environment, offered first. */
  environmentId: string | null;
  onSelectEnvironment: (id: string) => void;
  models: ModelRow[];
  localModels: LocalModelRow[];
  /** `models.default` as the daemon applies it (from `models.local`); undefined until known. */
  defaultModel?: string | null;
  client: DaemonClient | null;
  /** Creates the session and sends the message; rejects with the daemon's reason. */
  onCreate: (options: NewSessionOptions) => Promise<void>;
  /** Refreshes the model lists after the setup added a folder or an endpoint. */
  onModelsChanged: () => void;
  /** Opens the Local models panel (for its Hugging Face download form). */
  onOpenLocalModels: () => void;
};

/** `models.default` from the daemon's configuration, or null when unset. */
function useDefaultModel(client: DaemonClient | null): { model: string | null; loaded: boolean } {
  const [state, setState] = useState<{ model: string | null; loaded: boolean }>({ model: null, loaded: false });
  useEffect(() => {
    if (!client) return;
    let live = true;
    client
      .call<ConfigDescribeResult>("config.describe", {})
      .then((config) => {
        const value = config.entries.find((entry) => entry.key === "models.default")?.value;
        if (live) setState({ model: typeof value === "string" && value ? value : null, loaded: true });
      })
      .catch(() => live && setState({ model: null, loaded: true }));
    return () => {
      live = false;
    };
  }, [client]);
  return state;
}

export function NewSessionPanel({
  environments,
  gitEnvironments,
  environmentId,
  onSelectEnvironment,
  models,
  localModels,
  defaultModel,
  client,
  onCreate,
  onModelsChanged,
  onOpenLocalModels,
}: Props) {
  const settings = useSettings();
  const [draft, setDraft] = useState("");
  // Undefined until the user picks: then the configured default applies.
  const [chosenModel, setChosenModel] = useState<string | undefined>(undefined);
  const [worktree, setWorktree] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const defaults = useDefaultModel(client);

  useEffect(() => field.current?.focus(), []);

  // The selected environment, else the first (pinned ones lead) so there is always a choice made.
  const environment =
    environments.find((env) => env.id === environmentId) ??
    environments.find((env) => env.pinned) ??
    environments[0] ??
    null;
  const hasGit = environment !== null && gitEnvironments.includes(environment.id);
  // The daemon's live default wins: it changes at once when a model is made the default.
  const defaultSpec = asSpec(models, defaultModel !== undefined ? defaultModel : defaults.model);
  const usable = models.some((row) => row.endpoint_configured) || localModels.some((row) => !row.problem);
  const needsSetup = client !== null && !defaultSpec && !usable;
  const model = chosenModel ?? defaultSpec ?? "";
  const options = [
    // Without a configured default a session may start with no model; its messages wait.
    ...(defaultSpec ? [] : [{ value: "", label: "No model", description: "Pick one later; messages wait until then" }]),
    ...modelOptions(models, localModels, defaultSpec),
  ];
  const blocker = !client
    ? "Not connected to imperium-core"
    : !environment
      ? environments.length === 0
        ? "Create an environment first (Environments → New environment)"
        : "Choose an environment"
      : environment.folders.length === 0
        ? `Add a folder to ${environment.name} first`
        : null;

  const create = () => {
    const text = draft.trim();
    if (!environment || blocker || creating) return;
    setCreating(true);
    setError(null);
    onCreate({ environmentId: environment.id, model: model || null, worktree: hasGit && worktree, text })
      .catch((reason: unknown) => {
        setError(reason instanceof Error ? reason.message : String(reason));
        setCreating(false);
      });
  };
  const enterSends = settings.sendKey === "enter";

  return (
    <div className="session new-session-view">
      <div className="new-session-intro">
        <h3>New session</h3>
        <p className="muted">
          {environment ? (
            <>
              In <strong>{environment.name}</strong>
              {environment.folders[0] ? <span title={environment.folders[0]}> · {environment.folders[0]}</span> : null}
            </>
          ) : (
            "Choose an environment to work in."
          )}
        </p>
      </div>

      {error && (
        <p className="error new-session-error" role="alert">
          {error}
        </p>
      )}

      {needsSetup && <ModelSetup client={client} onChanged={onModelsChanged} onDownload={onOpenLocalModels} />}

      <form
        className="composer"
        onSubmit={(event) => {
          event.preventDefault();
          create();
        }}
      >
        <textarea
          ref={field}
          value={draft}
          aria-label="First message"
          placeholder={`What should the agent do? (${enterSends ? "Enter" : "Ctrl+Enter"} starts the session)`}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            const modified = event.ctrlKey || event.metaKey;
            if (enterSends ? !event.shiftKey && !modified : modified) {
              event.preventDefault();
              create();
            }
          }}
          rows={1}
        />
        <button
          type="submit"
          className="send"
          disabled={blocker !== null || creating}
          aria-label="Start session"
          title={blocker ?? (draft.trim() ? "Start the session and send this message" : "Start the session")}
        >
          <ArrowUp aria-hidden="true" />
        </button>
      </form>

      <div className="session-footer new-session-options" role="group" aria-label="New session options">
        <Select
          aria-label="Environment"
          className="footer-select"
          value={environment?.id ?? ""}
          placeholder="Environment"
          onChange={onSelectEnvironment}
          options={environments.map((env) => ({
            value: env.id,
            label: env.name,
            description: env.folders.length === 0 ? "No folders yet" : env.folders[0],
          }))}
        />
        <span className="model-select">
          <Select
            aria-label="Model"
            className="model-select-button"
            value={model}
            placeholder="No model"
            onChange={setChosenModel}
            options={options}
          />
        </span>
        {hasGit && (
          <label className="option-row new-session-worktree" title="Start on a new git worktree and branch">
            <Switch checked={worktree} onChange={setWorktree} label="Start in a worktree" />
            Start in a worktree
          </label>
        )}
        <span className="session-footer-spacer" />
        {blocker && client && <span className="muted new-session-blocker">{blocker}</span>}
      </div>
    </div>
  );
}
