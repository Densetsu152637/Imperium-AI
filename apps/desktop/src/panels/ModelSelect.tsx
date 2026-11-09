// Choosing a session's model: the rows of the model table (`models.list`) and the local models
// found in the model folders (`models.local`), as one themed list. Used by the session footer to
// switch an open session's model and by the new-session composer to pick the first one.

import { X } from "lucide-react";
import { useEffect, useState } from "react";

import type { LocalModelRow, ModelRow, SessionControlResult } from "@imperium/protocol";

import { Select, type SelectOption } from "../components/Select";
import { formatTokens } from "../sessions";

const RUNTIMES: Record<LocalModelRow["runtime"], string> = { llamacpp: "llama.cpp", vllm: "vLLM" };

/** The spec a local model is used by: `<runtime>:<id>`. */
export const localSpec = (row: LocalModelRow): string => `${row.runtime}:${row.id}`;

/**
 * The models a session can use, by spec: table rows first (rows whose endpoint is not configured
 * are listed but cannot be chosen), then local models the table does not already list. A
 * `current` model outside both is listed as is, so the list always shows it.
 */
export function modelOptions(models: ModelRow[], local: LocalModelRow[], current: string | null): SelectOption[] {
  const options: SelectOption[] = models.map((row) => ({
    value: row.spec,
    label: row.id,
    disabled: !row.endpoint_configured,
    description: row.endpoint_configured
      ? [row.id === row.spec ? null : row.spec, row.location, row.tier].filter(Boolean).join(" · ")
      : "Endpoint not configured",
  }));
  for (const row of local) {
    const spec = localSpec(row);
    if (options.some((option) => option.value === spec)) continue;
    options.push({
      value: spec,
      label: row.id,
      description: `Local · ${RUNTIMES[row.runtime]}${row.loaded ? " · loaded" : ""}`,
    });
  }
  if (current && !options.some((option) => option.value === current)) {
    options.unshift({ value: current, label: current });
  }
  return options;
}

/** A configured model name (a table id or a spec) as the spec the list uses. */
export function asSpec(models: ModelRow[], name: string | null): string | null {
  if (!name) return null;
  return models.find((row) => row.id === name)?.spec ?? name;
}

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
  onSwitch,
}: {
  /** `null` while the session has no model yet. */
  current: string | null;
  busy: boolean;
  disabled?: boolean;
  models: ModelRow[];
  local: LocalModelRow[];
  /** Switches the model; rejects with the daemon's reason. */
  onSwitch: (model: string) => Promise<SessionControlResult>;
}) {
  const [pending, setPending] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const switchTo = (spec: string) => {
    setSwitching(true);
    setNote(null);
    setError(null);
    onSwitch(spec)
      .then((result) => {
        const compaction = result.context?.compaction;
        if (compaction) {
          setNote(`Compacted ${formatTokens(compaction.before_tokens)} → ${formatTokens(compaction.after_tokens)} tokens`);
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

  const choose = (spec: string) => {
    setError(null);
    if (spec === current) {
      setPending(null);
      return;
    }
    if (busy) setPending(spec);
    else switchTo(spec);
  };

  const value = pending ?? current ?? "";
  return (
    <span className="model-select">
      <Select
        aria-label="Model"
        className="model-select-button"
        value={value}
        options={modelOptions(models, local, current)}
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
    </span>
  );
}
