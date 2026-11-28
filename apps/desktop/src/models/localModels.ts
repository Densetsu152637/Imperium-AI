// Local models are off by default (`models.local.enabled`); the models, runtimes and house-model
// choices stay listed, shown as "off", and can be set up before the switch is turned on. This
// holds the wording and the daemon calls of that part of the Local models panel, and the Rescan
// button's flow, as pure functions over an injected `call`, so tests drive them without a window
// (test/localModels.test.ts).

import type { LocalModelListResult, LocalModelRow, ModelConfigResult, ModelRescanResult } from "@imperium/protocol";

/** Calls a daemon method; the app's `DaemonClient.call` fits. */
export type Call = <T>(method: string, params?: unknown) => Promise<T>;

/** What the panel says while local models are off, and how to turn them on. */
export const LOCAL_OFF_NOTE =
  "Local models are off by default: none is downloaded, installed, started or chosen by routing until you turn them on. They stay listed below, shown as off, so you can set them up first.";

/** The setting the global switch writes. */
export const LOCAL_SWITCH_KEY = "models.local.enabled";

/** An old daemon does not send the switch; its local models were always on. */
export const localEnabled = (listing: LocalModelListResult | null): boolean => listing?.local_enabled ?? true;

/** Whether a model is off, either through the global switch or its own. */
export const rowIsOff = (listing: LocalModelListResult, row: LocalModelRow): boolean => !localEnabled(listing) || !row.own_enabled;

/** Why a model is off, for its tooltip; null when it is on. */
export function offReason(listing: LocalModelListResult, row: LocalModelRow): string | null {
  if (!localEnabled(listing)) return "Local models are off (Settings → Local models): it is never routed to, offered or started";
  if (!row.own_enabled) return "Switched off for this model: never routed to, offered or chosen";
  return null;
}

/** Turns local models on or off for everything (`models.local.enabled`). */
export async function setLocalEnabled(call: Call, on: boolean): Promise<void> {
  await call("config.set", { key: LOCAL_SWITCH_KEY, value: on });
}

/** Switches one local model (by spec, `llamacpp:<id>`) on or off, keeping the other switches. */
export async function setModelEnabled(call: Call, spec: string, on: boolean): Promise<void> {
  const config = await call<Pick<ModelConfigResult, "disabled">>("models.config");
  const disabled = new Set(config.disabled ?? []);
  if (on) disabled.delete(spec);
  else disabled.add(spec);
  await call("models.set_config", { local: [], table: [], hidden: null, disabled: [...disabled].sort(), dry_run: false });
}

/** The Rescan button's state. */
export type RescanState =
  | { kind: "idle" }
  | { kind: "scanning" }
  | { kind: "done"; message: string }
  | { kind: "failed"; message: string };

const plural = (count: number, word: string): string => `${count} ${word}${count === 1 ? "" : "s"}`;

/** What a scan found, in words: how many models, and which are new or gone. */
export function rescanMessage(result: Pick<ModelRescanResult, "found" | "added" | "removed">): string {
  const parts = [`Found ${plural(result.found, "model")}`];
  if (result.added.length > 0) parts.push(`${result.added.length} new (${result.added.join(", ")})`);
  if (result.removed.length > 0) parts.push(`${result.removed.length} gone (${result.removed.join(", ")})`);
  if (result.added.length === 0 && result.removed.length === 0) parts.push("nothing new");
  return `${parts.join(" · ")}.`;
}

const failure = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/**
 * Scans the model folders again through the daemon (`models.rescan`: a plain `models.local`
 * only reads the last scan, which is why the button used to do nothing), shows the progress
 * and the outcome through `show`, and hands the new listing to `apply`. Turns nothing on.
 */
export async function runRescan(
  call: Call,
  show: (state: RescanState) => void,
  apply: (listing: LocalModelListResult) => void,
): Promise<void> {
  show({ kind: "scanning" });
  try {
    const result = await call<ModelRescanResult>("models.rescan", {});
    apply(result.listing);
    show({ kind: "done", message: rescanMessage(result) });
  } catch (error) {
    show({ kind: "failed", message: `The rescan failed: ${failure(error)}` });
  }
}
