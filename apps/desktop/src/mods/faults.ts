// Behaviour faults reported by mod frames: a WebAssembly behaviour that was refused or stopped
// (docs/06-extensibility.md). A frame is untrusted, so what it reports is checked and clipped
// here before it is kept, and the list is short; Settings → Mods shows it.

export type BehaviourFault = { modId: string; name: string; phase: "load" | "tick"; reason: string; at: number };

const MAX_FAULTS = 50;
const MAX_PER_MOD = 10;
const MAX_TEXT = 200;

let faults: BehaviourFault[] = [];
const listeners = new Set<() => void>();

const clip = (value: unknown): string | null => (typeof value === "string" ? value.replace(/\s+/g, " ").trim().slice(0, MAX_TEXT) : null);

/**
 * Records a report from the frame of mod `modId`. Returns the fault kept, or null when the report
 * was malformed or repeats the mod's latest one.
 */
export function recordFault(modId: string, report: unknown, now: number = Date.now()): BehaviourFault | null {
  if (typeof report !== "object" || report === null) return null;
  const { name, phase, reason } = report as Record<string, unknown>;
  const [label, why] = [clip(name), clip(reason)];
  if (!label || !why || (phase !== "load" && phase !== "tick")) return null;
  const fault: BehaviourFault = { modId, name: label, phase, reason: why, at: now };
  const own = faults.filter((f) => f.modId === modId);
  const last = own[own.length - 1];
  if (last && last.name === fault.name && last.reason === fault.reason && last.phase === fault.phase) return null;
  let next = [...faults, fault];
  if (own.length >= MAX_PER_MOD) {
    const drop = own[0];
    next = next.filter((f) => f !== drop);
  }
  faults = next.slice(-MAX_FAULTS);
  listeners.forEach((listener) => listener());
  return fault;
}

/** Every fault kept, oldest first (a new array each time the list changes, so it suits `useSyncExternalStore`). */
export const getFaults = (): readonly BehaviourFault[] => faults;

export function subscribeFaults(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Forgets one mod's faults, or all of them. */
export function clearFaults(modId?: string): void {
  faults = modId === undefined ? [] : faults.filter((f) => f.modId !== modId);
  listeners.forEach((listener) => listener());
}
