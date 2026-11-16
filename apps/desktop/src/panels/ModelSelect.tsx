// Choosing a session's model: the rows of the model table (`models.list`) and the local models
// found in the model folders (`models.local`), as one themed list. Used by the session footer to
// switch an open session's model and by the new-session composer to pick the first one.

import { X } from "lucide-react";
import { useEffect, useId, useState } from "react";

import type { LocalModelRow, ModelRow, RuntimeInfo, SessionControlResult } from "@imperium/protocol";

import { Select, type SelectOption } from "../components/Select";
import { modelOptions } from "../models/modelOptions";
import { effortHint, effortToSend } from "../sessions/effort";
import { formatTokens } from "../sessions/sessions";
import { useSettings } from "../settings/settings";

export { asSpec, localDescription, localSpec, memoryPlan, modelOptions, sizeShort, tokensShort } from "../models/modelOptions";

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

/**
 * The open session's model, switchable at any time: between turns it switches at once; while a
 * turn runs the choice waits and is applied when the turn ends, since the daemon switches models
 * between turns. A smaller context window makes the daemon summarise older turns first; the
 * result is noted beside the list.
 */
export function SessionModelSelect({
  current,
  busy,
  disabled,
  models,
  local,
  runtime,
  onOpenLocalModels,
  onSwitch,
}: {
  /** `null` while the session has no model yet. */
  current: string | null;
  busy: boolean;
  disabled?: boolean;
  models: ModelRow[];
  local: LocalModelRow[];
  /** The llama.cpp runtime (`models.local`); a llama.cpp model is chosen after a warning when it is missing. */
  runtime?: RuntimeInfo | null;
  /** Opens the Local models panel, where the runtime is installed or chosen. */
  onOpenLocalModels?: () => void;
  /** Switches the model; rejects with the daemon's reason. */
  onSwitch: (model: string) => Promise<SessionControlResult>;
}) {
  const settings = useSettings();
  const [pending, setPending] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // A llama.cpp model chosen while no runtime is ready, waiting for "Use anyway".
  const [unready, setUnready] = useState<string | null>(null);
  // A llama.cpp model chosen while its runtime will be installed on first use.
  const [installInfo, setInstallInfo] = useState(false);

  const switchTo = (spec: string) => {
    setSwitching(true);
    setNote(null);
    setError(null);
    onSwitch(spec)
      .then((result) => {
        // The model is not loaded until the next message; the fit is an estimate until then.
        const context = result.context;
        const compaction = context?.compaction;
        if (compaction) {
          setNote(`Compacted ${formatTokens(compaction.before_tokens)} → ${formatTokens(compaction.after_tokens)} tokens`);
        } else if (context && context.budget !== null && context.tokens > context.budget) {
          setNote(`About ${formatTokens(context.tokens)} tokens (estimated) for a ${formatTokens(context.budget)} budget: older turns are compacted when you send`);
        }
      })
      .catch((reason: unknown) => setError(message(reason)))
      .finally(() => setSwitching(false));
  };

  // A choice made during a turn applies once the turn is over.
  useEffect(() => {
    if (busy || pending === null) return;
    const spec = pending;
    setPending(null);
    if (spec !== current) switchTo(spec);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [busy, pending]);

  const apply = (spec: string) => {
    if (busy) setPending(spec);
    else switchTo(spec);
  };

  const choose = (spec: string) => {
    setError(null);
    setUnready(null);
    if (spec === current) {
      setPending(null);
      return;
    }
    setInstallInfo(false);
    if (needsRuntime(spec, runtime)) {
      setUnready(spec);
      return;
    }
    // No runtime yet, but it is fetched on first use: say so, without stopping the switch.
    if (spec.startsWith("llamacpp:") && runtime != null && !runtime.installed) setInstallInfo(true);
    apply(spec);
  };

  const value = pending ?? current ?? "";
  return (
    <span className="model-select">
      <Select
        aria-label="Model"
        className="model-select-button"
        value={value}
        options={modelOptions(models, local, current, settings.modelMarks, true)}
        placeholder="No model"
        disabled={disabled || switching}
        onChange={choose}
      />
      {switching && (
        <span className="model-status muted" role="status">
          Switching…
        </span>
      )}
      {pending !== null && !switching && (
        <span className="model-status" role="status">
          After this turn
          <button
            type="button"
            className="ghost icon-only model-status-cancel"
            onClick={() => setPending(null)}
            aria-label="Keep the current model"
            title="Keep the current model"
          >
            <X aria-hidden="true" />
          </button>
        </span>
      )}
      {note && !switching && pending === null && (
        <span className="model-status model-note" role="status">
          {note}
        </span>
      )}
      {error && !switching && (
        <span className="model-status error" role="alert" title={error}>
          {error}
        </span>
      )}
      {installInfo && unready === null && (
        <span className="model-status muted" role="status">
          {INSTALL_ON_FIRST_USE}
        </span>
      )}
      {unready !== null && (
        <span className="model-status model-runtime-warning" role="alert">
          {NO_RUNTIME}
          {onOpenLocalModels && (
            <button
              type="button"
              className="ghost"
              onClick={() => {
                setUnready(null);
                onOpenLocalModels();
              }}
            >
              Open Local models
            </button>
          )}
          <button
            type="button"
            className="ghost"
            onClick={() => {
              const spec = unready;
              setUnready(null);
              apply(spec);
            }}
          >
            Use anyway
          </button>
          <button
            type="button"
            className="ghost icon-only model-status-cancel"
            onClick={() => setUnready(null)}
            aria-label="Keep the current model"
            title="Keep the current model"
          >
            <X aria-hidden="true" />
          </button>
        </span>
      )}
    </span>
  );
}

/** What the session selector says before a llama.cpp model is chosen without a runtime. */
export const NO_RUNTIME =
  "No llama.cpp runtime yet — open Local models → Runtime to install one or choose an existing llama-server.";

/** What the session selector says when a llama.cpp model is chosen and the runtime is fetched on first use. */
export const INSTALL_ON_FIRST_USE = "llama.cpp will be installed on first use.";

/**
 * Whether `spec` is a llama.cpp model and `runtime` says no `llama-server` is there to load it
 * and none will be installed by itself (`auto_install` off).
 */
export function needsRuntime(spec: string, runtime: RuntimeInfo | null | undefined): boolean {
  return spec.startsWith("llamacpp:") && runtime != null && !runtime.installed && !runtime.auto_install;
}

/** A permission mode, as `session.control` `set_permission_mode` takes it. */
export type PermissionMode = "ask" | "default" | "auto" | "accept_edits" | "plan" | "full";

const MODE_OPTIONS: SelectOption[] = [
  { value: "plan", label: "Plan only", description: "Agents read and plan; no writes or commands" },
  { value: "ask", label: "Ask every time", description: "Every write, edit, and command asks, whatever the rules allow" },
  { value: "default", label: "Ask by rules", description: "Writes and commands ask unless a rule allows them" },
  { value: "auto", label: "Auto", description: "A classifier lets routine in-scope calls run; anything unsure asks you" },
  { value: "accept_edits", label: "Accept edits", description: "File edits run without asking; commands follow the rules" },
  { value: "full", label: "Full access", description: "Everything runs except what deny rules or the safety floor stop" },
];

/**
 * The open session's permission mode, beside its model and effort. A change applies from the
 * agents' next call, during a turn too; deny rules and the safety floor hold in every mode.
 */
export function PermissionModeSelect({
  current,
  disabled,
  onChange,
}: {
  /** The session's mode; daemons before protocol 0.17 send none, shown as default. */
  current: PermissionMode | undefined;
  disabled?: boolean;
  /** Sets the mode; rejects with the daemon's reason. */
  onChange: (mode: PermissionMode) => Promise<unknown>;
}) {
  const [pending, setPending] = useState<PermissionMode | null>(null);
  const [error, setError] = useState<string | null>(null);
  const choose = (value: string) => {
    const mode = value as PermissionMode;
    if (mode === current) return;
    setPending(mode);
    setError(null);
    onChange(mode)
      .catch((reason: unknown) => setError(message(reason)))
      .finally(() => setPending(null));
  };
  return (
    <span className="mode-select">
      <Select
        aria-label="Permission mode"
        className="mode-select-button"
        value={pending ?? current ?? "default"}
        options={MODE_OPTIONS}
        disabled={disabled || pending !== null}
        onChange={choose}
      />
      {error && (
        <span className="model-status error" role="alert" title={error}>
          {error}
        </span>
      )}
    </span>
  );
}

/** A reasoning effort level, as `session.control` `set_effort` takes it. */
export type Effort = "low" | "medium" | "high" | "max";

const EFFORT_OPTIONS: SelectOption[] = [
  { value: "low", label: "Low effort", description: "Thinks briefly: fastest and cheapest" },
  { value: "medium", label: "Medium effort", description: "The usual balance" },
  { value: "high", label: "High effort", description: "Thinks longer on hard problems" },
  { value: "max", label: "Max effort", description: "The most the model's backend allows" },
];

/**
 * The open session's reasoning effort, beside its model. For a model that cannot reason it is
 * greyed out, not hidden, with a tooltip saying why; a change applies from the agents' next model call, during a turn too.
 */
export function EffortSelect({
  current,
  disabled,
  reasons = true,
  hasModel = true,
  onChange,
}: {
  /** The session's effort; daemons before protocol 0.9 send none, shown as medium. */
  current: Effort | undefined;
  disabled?: boolean;
  /** Whether the model can reason; if not the control stays, greyed out, and sends nothing. */
  reasons?: boolean;
  /** Whether the session has a model; without one the hint says to choose it first. */
  hasModel?: boolean;
  /** Sets the effort; rejects with the daemon's reason. */
  onChange: (effort: Effort) => Promise<unknown>;
}) {
  const [pending, setPending] = useState<Effort | null>(null);
  const [error, setError] = useState<string | null>(null);
  const hintId = useId();
  const hint = effortHint(hasModel);
  const choose = (value: string) => {
    const effort = effortToSend(reasons, current, value as Effort);
    if (effort === null) return;
    setPending(effort);
    setError(null);
    onChange(effort)
      .catch((reason: unknown) => setError(message(reason)))
      .finally(() => setPending(null));
  };
  return (
    <span
      className={`effort-select${reasons ? "" : " unavailable"}`}
      title={reasons ? undefined : hint}
    >
      <Select
        aria-label="Reasoning effort"
        className="effort-select-button"
        value={pending ?? current ?? "medium"}
        options={EFFORT_OPTIONS}
        inert={!reasons}
        aria-describedby={reasons ? undefined : hintId}
        disabled={disabled || pending !== null}
        onChange={choose}
      />
      {!reasons && (
        <span id={hintId} className="visually-hidden">
          {hint}
        </span>
      )}
      {error && (
        <span className="model-status error" role="alert" title={error}>
          {error}
        </span>
      )}
    </span>
  );
}
