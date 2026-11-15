// "Use this model": which models are switched off, and what a change of the switches needs. A
// local model is off when its spec is in the daemon's `models.disabled`; a model-table row is off
// by its own `enabled`, or when its model is. A model that is off is greyed out, left out of the
// pickers, routing and escalation, and cannot be the default model: the daemon refuses to switch
// off the default (`models.set_config`), so the dialog asks for another default first. Pure
// (test/modelAvailability.test.ts).

import { rowById } from "./spec.ts";

/** What the dialog needs of the daemon's `models.config` result. */
export type AvailabilityInput = {
  /** `models.disabled`: specs switched off. */
  disabled: readonly string[];
  /** Local models by spec (`llamacpp:<id>`, `vllm:<id>`). */
  local: readonly string[];
  /** Model-table rows: each row's id, its spec, and whether its own switch is on. */
  rows: readonly { id: string; spec: string; enabled: boolean }[];
  /** `models.default`, a spec. */
  defaultModel: string | null;
};

/** The switches the user has set in the dialog and not saved yet, by spec / row spec. */
export type Switches = {
  /** Local models switched on (true) or off (false) in the dialog. */
  local: Readonly<Record<string, boolean>>;
  /** Row switches in the dialog, by row id. */
  rows: Readonly<Record<string, boolean>>;
};

export const NO_SWITCHES: Switches = { local: {}, rows: {} };

/** The listed specs after the local switches: switched off added, switched on removed. */
export function listedAfter(disabled: readonly string[], switches: Switches): string[] {
  const listed = new Set(disabled);
  for (const [spec, on] of Object.entries(switches.local)) {
    if (on) listed.delete(spec);
    else listed.add(spec);
  }
  return [...listed].sort();
}

/**
 * Every spec that is off: the listed ones, and a spec the model table names whose every row is
 * switched off (a row's own switch, or the dialog's row switch over it).
 */
export function offSpecs(input: AvailabilityInput, switches: Switches = NO_SWITCHES): Set<string> {
  const off = new Set(listedAfter(input.disabled, switches));
  const bySpec = new Map<string, boolean[]>();
  for (const row of input.rows) {
    const on = switches.rows[row.id] ?? row.enabled;
    bySpec.set(row.spec, [...(bySpec.get(row.spec) ?? []), on]);
  }
  for (const [spec, states] of bySpec) {
    if (states.every((on) => !on)) off.add(spec);
  }
  return off;
}

/** The list to send as `disabled`, or null when the switches leave the saved list as it is. */
export function disabledToSave(disabled: readonly string[], switches: Switches): string[] | null {
  const next = listedAfter(disabled, switches);
  const saved = [...new Set(disabled)].sort();
  return next.length === saved.length && next.every((spec, index) => spec === saved[index]) ? null : next;
}

/** The default model, when the switches would turn it off (the daemon refuses that). */
export function defaultTurnedOff(input: AvailabilityInput, switches: Switches): string | null {
  if (input.defaultModel === null) return null;
  // `models.default` may name a table row by its id.
  const spec = rowById(input.rows, input.defaultModel)?.spec ?? input.defaultModel;
  const before = offSpecs(input);
  const after = offSpecs(input, switches);
  return after.has(spec) && !before.has(spec) ? spec : null;
}

/** Models that could be the new default: on after the switches, and usable, other than `except`. */
export function replacementDefaults(
  specs: readonly { spec: string; usable: boolean }[],
  off: ReadonlySet<string>,
  except: string,
): string[] {
  return specs
    .filter((entry) => entry.usable && entry.spec !== except && !off.has(entry.spec))
    .map((entry) => entry.spec);
}

/**
 * Whether the model `spec` is off in the lists the daemon sent (`models.list`, `models.local`,
 * each carrying `enabled`): a local model that is off, or a spec the model table names whose
 * every row is off. A model neither list knows is not off.
 */
export function modelIsOff(
  spec: string,
  rows: readonly { spec: string; enabled: boolean }[],
  local: readonly { runtime: string; id: string; enabled: boolean }[],
): boolean {
  if (local.some((model) => `${model.runtime}:${model.id}` === spec && !model.enabled)) return true;
  const named = rows.filter((row) => row.spec === spec);
  return named.length > 0 && named.every((row) => !row.enabled);
}
