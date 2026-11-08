// Event routing per nursery, snapshot repair, and the nursery state they drive. No DOM.

import { test } from "node:test";

import type { AgentSnapshot, SessionEvent, SessionSnapshot } from "@imperium/mod-sdk";

import type { Env } from "../src/arbiter.ts";
import { isReviewer } from "../src/events.ts";
import { Nursery } from "../src/nursery.ts";
import type { Deps } from "../src/nursery.ts";
import { MIX, clip, hash, pickPet } from "../src/pick.ts";
import { allDrawn } from "../src/pet.ts";
import { REMOVE_MS, busyOf, repair, routeEvent } from "../src/route.ts";
import { newWalker, stepWalker } from "../src/wander.ts";
import { expect } from "./expect.ts";

const T0 = 1_000_000;
const env: Env = { catalog: allDrawn(), sleepAfterMs: 180_000, fidget: true, rng: () => 0 };
const deps: Deps = { envFor: () => env, assign: () => "pet" };

const ev = (agentId: string, type: string, extra: Record<string, unknown> = {}, sessionId = "s1"): SessionEvent => ({ sessionId, agentId, event: { type, ...extra } });
const agent = (agentId: string, status: AgentSnapshot["status"], brief = ""): AgentSnapshot => ({ agentId, parentId: agentId === "root" ? null : "root", brief, status });
const session = (sessionId: string, agents: AgentSnapshot[], title = sessionId): SessionSnapshot => ({ sessionId, title, projectDir: "/p", status: "idle", agents });

const shown = (n: Nursery, key: string, at: number): string => n.view(n.entries.get(key)!, at).name;

test("session routing: an agent's pet hears only its own events", () => {
  const steps = routeEvent("session", ev("root", "tool_started", { call_id: "c", tool: "x", summary: "" }));
  expect(steps).toHaveLength(1);
  expect(steps[0]).toMatchObject({ key: "s1:root", root: true });
  expect(steps[0]!.events).toContainEqual({ type: "loop", source: "main", name: "working" });
  expect(routeEvent("session", ev("root", "text_delta", { text: "secret" }))).toEqual([]);
});

test("session routing: task_spawned makes the child's pet, task_finished removes it later", () => {
  const spawned = routeEvent("session", ev("root", "task_spawned", { task_id: "t1", brief: "do it", model: "m" }));
  expect(spawned.map((s) => s.key)).toEqual(["s1:root", "s1:t1"]);
  expect(spawned[0]!.events).toEqual([{ type: "shot", name: "summoning", source: "main" }]);
  expect(spawned[1]).toMatchObject({ spawn: { agentId: "t1", brief: "do it" }, create: true });
  const done = routeEvent("session", ev("root", "task_finished", { task_id: "t1", ok: false, report: "", usage: {}, cost_usd: null }));
  expect(done).toHaveLength(1);
  expect(done[0]).toMatchObject({ key: "s1:t1", create: false, removeAfterMs: REMOVE_MS });
  expect(done[0]!.events).toContainEqual({ type: "shot", name: "failed", source: "main" });
});

test("session routing: denial is confusion; failure clears and fails; a subagent's turn end schedules removal", () => {
  const denied = routeEvent("session", ev("root", "tool_finished", { call_id: "c", ok: false, output: "The user denied this action." }));
  expect(denied[0]!.events).toContainEqual({ type: "shot", name: "confused", source: "main" });
  const failed = routeEvent("session", ev("t1", "turn_failed", { message: "x" }));
  expect(failed[0]!.events.slice(-2)).toEqual([{ type: "loop", source: "main", name: null }, { type: "shot", name: "failed", source: "main" }]);
  expect(failed[0]!.removeAfterMs).toBe(REMOVE_MS);
  expect(routeEvent("session", ev("root", "turn_completed", { usage: {}, steps: 1 }))[0]!.removeAfterMs).toBeUndefined();
});

test("global routing: one pet per session, the root agent as main and subagents as their own sources", () => {
  const sub = routeEvent("global", ev("t1", "permission_requested", { request_id: 1, tool: "x", summary: "" }));
  expect(sub).toHaveLength(1);
  expect(sub[0]).toMatchObject({ key: "s1", root: false });
  expect(sub[0]!.events).toEqual([{ type: "block", source: "t1", id: "perm" }]);
  const root = routeEvent("global", ev("root", "permission_requested", { request_id: 1, tool: "x", summary: "" }));
  expect(root[0]!.events).toEqual([{ type: "block", source: "main", id: "perm" }]);
  const spawn = routeEvent("global", ev("root", "task_spawned", { task_id: "t1", brief: "b", model: "m" }));
  expect(spawn[0]!.events).toContainEqual({ type: "spawn", source: "t1", name: "working" });
  const end = routeEvent("global", ev("t1", "turn_completed", { usage: {}, steps: 1 }));
  expect(end[0]!.events).toContainEqual({ type: "end", source: "t1" });
  expect(routeEvent("global", ev("root", "task_finished", { task_id: "t1", ok: true }))[0]!.events).toEqual([
    { type: "end", source: "t1" },
    { type: "shot", name: "jumping", source: "main" },
  ]);
});

test("repair makes a source match a snapshot, and is idempotent", () => {
  const n = new Nursery("global", deps);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "waiting")])], null, T0);
  const a = n.entries.get("s1")!.arbiter;
  expect(a.sources.main?.base).toBe("working");
  expect(Object.keys(a.sources.t1?.waits ?? {})).toEqual(["perm"]);
  expect(repair(a, "main", "working", false)).toEqual([]);
  expect(repair(a, "t1", "waiting", true)).toEqual([]);
  expect(repair(a, "t1", "finished", true)).toEqual([{ type: "end", source: "t1" }]);
  expect(repair(a, "main", "idle", false)).toEqual([{ type: "loop", source: "main", name: null }]);
});

test("global nursery: a pet per session, busy sessions first, subagents counted", () => {
  const n = new Nursery("global", deps);
  n.snapshots(
    [
      session("a", [agent("root", "idle")], "Alpha"),
      session("b", [agent("root", "working"), agent("t1", "working"), agent("t2", "waiting")], "Beta"),
      session("c", [agent("root", "waiting")], "Gamma"),
    ],
    "a",
    T0,
  );
  expect(n.sorted().map((e) => e.label)).toEqual(["Beta", "Gamma", "Alpha"]);
  expect(n.summary()).toEqual({ pets: 3, busy: 2, subagents: 2 });
  // Events feed the session's pet; a sub's finish ends its source.
  n.event(ev("t1", "turn_completed", { usage: {}, steps: 1 }, "b"), T0 + 10);
  expect(n.summary().subagents).toBe(1);
  // A session that goes away takes its pet with it.
  n.snapshots([session("a", [agent("root", "idle")], "Alpha")], "a", T0 + 20);
  expect(n.entries.size).toBe(1);
});

test("session nursery: follows the selection, one pet per agent, finished subagents leave after a while", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "working", "fix"), agent("t2", "failed", "old")], "One"), session("s2", [agent("root", "idle")], "Two")], "s1", T0);
  expect(n.sorted().map((e) => e.key)).toEqual(["s1:root", "s1:t1"]);
  expect(n.summary()).toEqual({ pets: 2, busy: 0, subagents: 1 });
  // Another session's events are ignored.
  n.event(ev("root", "tool_started", {}, "s2"), T0 + 5);
  expect(n.entries.size).toBe(2);
  // A new subagent appears on task_spawned, then finishes and leaves REMOVE_MS later.
  n.event(ev("root", "task_spawned", { task_id: "t3", brief: "write\n tests", model: "m" }), T0 + 10);
  expect(n.entries.get("s1:t3")?.label).toBe("write tests");
  expect(n.summary().subagents).toBe(2);
  n.event(ev("root", "task_finished", { task_id: "t3", ok: true }), T0 + 1000);
  expect(n.summary().subagents).toBe(1);
  expect(n.tick(T0 + 1000 + REMOVE_MS - 1)).toBe(false);
  expect(n.tick(T0 + 1000 + REMOVE_MS)).toBe(true);
  expect(n.entries.has("s1:t3")).toBe(false);
  // Stray events do not bring a retired pet back.
  n.event(ev("t3", "tool_started", {}), T0 + 40_000);
  expect(n.entries.has("s1:t3")).toBe(false);
  // Selecting the other session swaps the pets; pinning holds the first.
  n.snapshots([session("s1", [agent("root", "working")], "One"), session("s2", [agent("root", "idle")], "Two")], "s2", T0 + 50_000);
  expect([...n.entries.keys()]).toEqual(["s2:root"]);
  n.pinned = "s1";
  n.snapshots([session("s1", [agent("root", "working")], "One"), session("s2", [agent("root", "idle")], "Two")], "s2", T0 + 60_000);
  expect([...n.entries.keys()]).toEqual(["s1:root"]);
});

test("a pet shows what its agent does: working, waiting, then a jump when the turn completes", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "idle")])], "s1", T0);
  const t = T0 + 10_000;
  n.event(ev("root", "turn_started", { text: "go" }), t);
  expect(shown(n, "s1:root", t + 1000)).toBe("working");
  n.event(ev("root", "permission_requested", { request_id: 1, tool: "x", summary: "" }), t + 2000);
  expect(shown(n, "s1:root", t + 2000)).toBe("waiting");
  n.event(ev("root", "tool_started", {}), t + 4000);
  expect(busyOf(n.entries.get("s1:root")!.arbiter)).toBe("working");
  n.event(ev("root", "turn_completed", { usage: {}, steps: 1 }), t + 6000);
  expect(shown(n, "s1:root", t + 6000)).toBe("jumping");
  expect(busyOf(n.entries.get("s1:root")!.arbiter)).toBeNull();
});

test("a snapshot repairs a pet that missed events", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "working")])], "s1", T0);
  expect(busyOf(n.entries.get("s1:root")!.arbiter)).toBe("working");
  n.snapshots([session("s1", [agent("root", "idle")])], "s1", T0 + 10_000);
  expect(busyOf(n.entries.get("s1:root")!.arbiter)).toBeNull();
});

test("reviewer subagents draw review instead of working", () => {
  expect(isReviewer("", "Role: leaf; Profile: critic\nlook")).toBe(true);
  expect(isReviewer("plugin:validator", "")).toBe(true);
  expect(isReviewer("", "Profile: implementer")).toBe(false);
  const n = new Nursery("global", deps);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "working", "Profile: critic")])], null, T0);
  expect(n.entries.get("s1")!.arbiter.sources.t1?.base).toBe("review");
});

test("memory_saved overlays eating, which ends by itself", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "working")])], "s1", T0);
  n.event(ev("root", "memory_saved", { name: "m", scope: "user" }), T0 + 5000);
  expect(n.entries.get("s1:root")!.arbiter.sources.main?.over).toBe("eating");
  n.tick(T0 + 5000 + 3000);
  expect(n.entries.get("s1:root")!.arbiter.sources.main?.over).toBeNull();
});

test("My nursery: pets are added, patted, taken out", () => {
  const n = new Nursery("mine", deps);
  n.add("u1", "pet", "Rex", T0);
  n.add("u1", "pet", "Dup", T0);
  expect(n.entries.size).toBe(1);
  const t = T0 + 10_000;
  expect(shown(n, "u1", t)).toBe("idle");
  n.pat("u1", t);
  expect(shown(n, "u1", t)).toBe("petted");
  // Events from sessions do nothing here.
  n.event(ev("root", "turn_started", {}), t);
  expect(busyOf(n.entries.get("u1")!.arbiter)).toBeNull();
  // Left alone it falls asleep.
  expect(shown(n, "u1", t + 400_000)).toBe("sleeping");
  n.take("u1");
  expect(n.entries.size).toBe(0);
});

test("pet choice: a setting wins, otherwise a stable spread with the root agent first", () => {
  const ids = ["a", "b", "c"];
  expect(pickPet(ids, "b", "k", false)).toBe("b");
  expect(pickPet(ids, MIX, "k", true)).toBe("a");
  expect(pickPet(ids, "gone", "k", true)).toBe("a");
  expect(pickPet(ids, MIX, "k", false)).toBe(pickPet(ids, MIX, "k", false));
  expect(pickPet([], MIX, "k", true)).toBeNull();
  expect(hash("x")).toBe(hash("x"));
  expect(new Set(["s1", "s2", "s3", "s4", "s5", "s6"].map((k) => pickPet(ids, MIX, k, false))).size).toBeGreaterThan(1);
  expect(clip("  a   b\nc ", 10)).toBe("a b c");
  expect(clip("abcdefghij", 5)).toBe("abcd…");
});

test("wandering: an idle walker strays within range and stops; a busy one stops at once", () => {
  let w = newWalker(0, () => 0);
  expect(w.nextAt).toBe(6000);
  const rand = (): number => 1;
  let seen = 0;
  for (let t = 6000; t < 12_000; t += 50) {
    const r = stepWalker(w, t, 50, true, rand, 14);
    w = r.walker;
    seen = Math.max(seen, w.x);
    expect(w.x).toBeLessThanOrEqual(14);
  }
  expect(seen).toBeGreaterThan(10);
  const stopped = stepWalker({ x: 3, target: 14, nextAt: 0 }, 100, 50, false, rand);
  expect(stopped.dir).toBe(0);
  expect(stopped.walker.target).toBe(0);
});
