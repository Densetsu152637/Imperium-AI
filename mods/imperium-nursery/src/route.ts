// Routing, pure: which pet gets which arbiter events. The mapping from agent activity to pet
// behaviour mirrors the daemon's `pet_events` (crates/imperiumd/src/pets.rs, docs/16-pets.md).
//
//  - Session nursery: one pet per agent; each pet hears only its own agent's events, as its
//    `main` source. A subagent's pet is made by `task_spawned` and removed a while after it ends.
//  - Global nursery: one pet per session; it hears all the session's events, the root agent as
//    `main` and each subagent as its own source (so a waiting subagent shows `alert`).
//
// Snapshots seed and repair state (`repair`); events keep it fresh between snapshots.

import type { AgentSnapshot, SessionEvent } from "@imperium/mod-sdk";

import type { PetEvent } from "./arbiter.ts";
import type { PetsArbiter } from "./types.ts";

export type Mode = "session" | "global";

/** How long a finished subagent's pet stays (after its jumping/failed) in the session nursery. */
export const REMOVE_MS = 30_000;
/** How long `eating` lasts after a memory is saved. */
export const EAT_MS = 2500;
/** The wait id a permission request holds on its source. */
export const PERM = "perm";

/** The root agent's id in events and snapshots. */
export const ROOT = "root";

export const keyOf = (mode: Mode, sessionId: string, agentId: string): string => (mode === "session" ? `${sessionId}:${agentId}` : sessionId);

/** One batch of arbiter events for one pet. */
export type Step = {
  key: string;
  sessionId: string;
  agentId: string;
  /** Source a subagent's events are heard as, and its `main`-ness. */
  root: boolean;
  /** Whether the step may create the pet; a finish for a pet that is gone must not. */
  create: boolean;
  /** A subagent starts: its brief (for the label and reviewer detection). */
  spawn?: { agentId: string; brief: string };
  events: PetEvent[];
  removeAfterMs?: number;
  /** `overlay` on this source ends after this long. */
  overlay?: { source: string; ms: number };
};

const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** What one event does to the pet of the agent that sent it, heard as source `src`. */
function ownEvents(event: SessionEvent["event"], src: string, endsSource: boolean): PetEvent[] {
  const unblock: PetEvent = { type: "unblock", source: src, id: PERM };
  const shot = (name: string): PetEvent => ({ type: "shot", name, source: src });
  switch (event.type) {
    case "turn_started":
    case "tool_started":
      return [unblock, { type: "loop", source: src, name: "working" }];
    case "permission_requested":
      return [{ type: "block", source: src, id: PERM }];
    case "tool_finished":
      return event.ok === false && str(event.output).includes("denied")
        ? [unblock, { type: "loop", source: src, name: "working" }, shot("confused")]
        : [unblock];
    case "turn_completed":
      return endsSource ? [unblock, { type: "end", source: src }] : [unblock, { type: "loop", source: src, name: null }, shot("jumping")];
    case "turn_failed":
      return endsSource ? [unblock, { type: "end", source: src }] : [unblock, { type: "loop", source: src, name: null }, shot("failed")];
    case "memory_saved":
      return [{ type: "overlay", source: src, name: "eating" }];
    default:
      return [];
  }
}

/** The steps one raw session event makes in a nursery of `mode`. */
export function routeEvent(mode: Mode, e: SessionEvent): Step[] {
  const { sessionId, agentId, event } = e;
  const root = agentId === ROOT;
  const base = { sessionId, create: true as const };
  if (mode === "session") {
    const step = (id: string, events: PetEvent[], extra: Partial<Step> = {}): Step => ({
      ...base,
      key: keyOf(mode, sessionId, id),
      agentId: id,
      root: id === ROOT,
      events,
      ...extra,
    });
    if (event.type === "task_spawned") {
      const child = str(event.task_id);
      if (!child) return [];
      return [
        step(agentId, [{ type: "shot", name: "summoning", source: "main" }]),
        step(child, [{ type: "loop", source: "main", name: "working" }], { spawn: { agentId: child, brief: str(event.brief) } }),
      ];
    }
    if (event.type === "task_finished") {
      const child = str(event.task_id);
      if (!child) return [];
      const name = event.ok === false ? "failed" : "jumping";
      return [
        step(
          child,
          [
            { type: "unblock", source: "main", id: PERM },
            { type: "loop", source: "main", name: null },
            { type: "shot", name, source: "main" },
          ],
          { create: false, removeAfterMs: REMOVE_MS },
        ),
      ];
    }
    const events = ownEvents(event, "main", false);
    if (events.length === 0) return [];
    const ends = !root && (event.type === "turn_completed" || event.type === "turn_failed");
    const overlay = event.type === "memory_saved" ? { source: "main", ms: EAT_MS } : undefined;
    return [step(agentId, events, { ...(ends ? { removeAfterMs: REMOVE_MS } : {}), ...(overlay ? { overlay } : {}) })];
  }
  // Global: the session's pet hears everything; the root agent is `main`.
  const src = root ? "main" : agentId;
  const step = (events: PetEvent[], extra: Partial<Step> = {}): Step[] =>
    events.length === 0 ? [] : [{ ...base, key: keyOf(mode, sessionId, agentId), agentId, root, events, ...extra }];
  if (event.type === "task_spawned") {
    const child = str(event.task_id);
    if (!child) return [];
    return step(
      [
        { type: "spawn", source: child, name: "working" },
        { type: "shot", name: "summoning", source: src },
      ],
      { spawn: { agentId: child, brief: str(event.brief) } },
    );
  }
  if (event.type === "task_finished") {
    const child = str(event.task_id);
    if (!child) return [];
    return step([
      { type: "end", source: child },
      { type: "shot", name: event.ok === false ? "failed" : "jumping", source: src },
    ]);
  }
  const overlay = event.type === "memory_saved" ? { source: src, ms: EAT_MS } : undefined;
  return step(ownEvents(event, src, !root), overlay ? { overlay } : {});
}

/** Whether a source holds any work: a loop, an overlay, or a pending prompt. */
export const sourceBusy = (a: PetsArbiter, src: string): "waiting" | "working" | null => {
  const s = a.sources[src];
  if (!s) return null;
  if (Object.keys(s.waits).length > 0) return "waiting";
  return s.base || s.over ? "working" : null;
};

/** What the arbiter as a whole is busy with: waiting beats working. */
export function busyOf(a: PetsArbiter): "waiting" | "working" | null {
  let best: "waiting" | "working" | null = null;
  for (const id of Object.keys(a.sources)) {
    const b = sourceBusy(a, id);
    if (b === "waiting") return "waiting";
    if (b) best = b;
  }
  return best;
}

/** Whether the subagent sources (all but `main`) hold work: how many are busy. */
export function busySubs(a: PetsArbiter): number {
  return Object.keys(a.sources).filter((id) => id !== "main" && sourceBusy(a, id) !== null).length;
}

/**
 * The events that make source `src` match a snapshot's status: `ensure`/`block` for a working
 * or waiting agent, a cleared loop for an idle one, `end` for a finished subagent. Idempotent:
 * a source already in the state gets nothing.
 */
export function repair(a: PetsArbiter, src: string, status: AgentSnapshot["status"], endsSource: boolean): PetEvent[] {
  const s = a.sources[src];
  const waits = Object.keys(s?.waits ?? {});
  const unblocks = (): PetEvent[] => waits.map((id) => ({ type: "unblock", source: src, id }));
  switch (status) {
    case "working":
      return [...(s?.base ? [] : [{ type: "ensure", source: src, name: "working" } as PetEvent]), ...unblocks()];
    case "waiting":
      return waits.length > 0 ? [] : [{ type: "block", source: src, id: PERM }];
    case "idle":
    case "finished":
    case "failed":
      if (!s) return [];
      if (endsSource && status !== "idle") return [{ type: "end", source: src }];
      return [...unblocks(), ...(s.base || s.over ? [{ type: "loop", source: src, name: null } as PetEvent] : [])];
  }
}
