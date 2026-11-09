// Event routing per nursery, snapshot repair, and the nursery state they drive. No DOM.

import { test } from "node:test";

import type { AgentSnapshot, SessionEvent, SessionSnapshot } from "@imperium/mod-sdk";

import type { Env } from "../src/arbiter.ts";
import { isReviewer } from "../src/events.ts";
import { FIRST, LEAVE_MS, Nursery, RESIDENT, sessionKey } from "../src/nursery.ts";
import type { Deps } from "../src/nursery.ts";
import { MINE, MIX, clip, hash, pickPet, sessionPet } from "../src/pick.ts";
import { allDrawn } from "../src/pet.ts";
import { REMOVE_MS, busyOf, repair, routeEvent } from "../src/route.ts";
import { expect } from "./expect.ts";

const T0 = 1_000_000;
const env: Env = { catalog: allDrawn(), sleepAfterMs: 180_000, fidget: true, rng: () => 0 };
const deps: Deps = { envFor: () => env, assign: () => "pet" };

const ev = (agentId: string, type: string, extra: Record<string, unknown> = {}, sessionId = "s1"): SessionEvent => ({ sessionId, agentId, event: { type, ...extra } });
const agent = (agentId: string, status: AgentSnapshot["status"], brief = ""): AgentSnapshot => ({ agentId, parentId: agentId === "root" ? null : "root", brief, status });
const session = (sessionId: string, agents: AgentSnapshot[], title = sessionId, extra: Partial<SessionSnapshot> = {}): SessionSnapshot => ({
  sessionId,
  title,
  projectDir: "/p",
  status: "idle",
  environmentId: null,
  archived: false,
  pet: null,
  createdAt: null,
  agents,
  ...extra,
});

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
  const n = new Nursery("nursery", deps);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "waiting")])], null, T0);
  const a = n.entries.get(sessionKey("s1"))!.arbiter;
  expect(a.sources.main?.base).toBe("working");
  expect(Object.keys(a.sources.t1?.waits ?? {})).toEqual(["perm"]);
  expect(repair(a, "main", "working", false)).toEqual([]);
  expect(repair(a, "t1", "waiting", true)).toEqual([]);
  expect(repair(a, "t1", "finished", true)).toEqual([{ type: "end", source: "t1" }]);
  expect(repair(a, "main", "idle", false)).toEqual([{ type: "loop", source: "main", name: null }]);
});

test("nursery: a pet per unarchived session, busy sessions first, subagents counted", () => {
  const n = new Nursery("nursery", deps);
  n.snapshots(
    [
      session("a", [agent("root", "idle")], "Alpha"),
      session("b", [agent("root", "working"), agent("t1", "working"), agent("t2", "waiting")], "Beta"),
      session("c", [agent("root", "waiting")], "Gamma"),
      session("d", [agent("root", "idle")], "Delta", { archived: true }),
    ],
    "a",
    T0,
  );
  expect(n.sorted().map((e) => e.label)).toEqual(["Beta", "Gamma", "Alpha"]);
  expect(n.summary()).toEqual({ pets: 3, sessions: 3, busy: 2, subagents: 2 });
  expect(n.entries.has(sessionKey("d"))).toBe(false);
  // Events feed the session's pet; a sub's finish ends its source.
  n.event(ev("t1", "turn_completed", { usage: {}, steps: 1 }, "b"), T0 + 10);
  expect(n.summary().subagents).toBe(1);
});

test("nursery: persistent agent sessions have no pet", () => {
  const n = new Nursery("nursery", deps);
  n.snapshots([session("a", [agent("root", "idle")], "Alpha"), session("p", [agent("root", "working")], "Keeper", { persistent: true })], null, T0);
  expect([...n.entries.keys()]).toEqual([sessionKey("a")]);
  expect(n.summary()).toMatchObject({ sessions: 1, busy: 0 });
});

test("nursery: a new session's pet pops in, an archived one walks out and comes back when unarchived", () => {
  const n = new Nursery("nursery", deps);
  n.snapshots([session("a", [agent("root", "idle")], "Alpha")], null, T0);
  expect([...n.entries.keys()]).toEqual([sessionKey("a")]);
  // A session created later brings its pet.
  n.snapshots([session("a", [agent("root", "idle")], "Alpha"), session("b", [agent("root", "idle")], "Beta")], null, T0 + 100);
  expect(n.entries.get(sessionKey("b"))?.label).toBe("Beta");
  // Archiving makes its pet leave: it is leaving at once and gone after a moment.
  const both = (archived: boolean) => [session("a", [agent("root", "idle")], "Alpha", { archived }), session("b", [agent("root", "idle")], "Beta")];
  n.snapshots(both(true), null, T0 + 1000);
  expect(n.entries.get(sessionKey("a"))?.removeAt).toBe(T0 + 1000 + LEAVE_MS);
  expect(n.summary().sessions).toBe(1);
  // Unarchived while it is leaving: it stays.
  n.snapshots(both(false), null, T0 + 2000);
  expect(n.entries.get(sessionKey("a"))?.removeAt).toBeNull();
  // Archived and left: gone; unarchived later it is back.
  n.snapshots(both(true), null, T0 + 3000);
  expect(n.tick(T0 + 3000 + LEAVE_MS)).toBe(true);
  expect(n.entries.has(sessionKey("a"))).toBe(false);
  n.snapshots(both(false), null, T0 + 9000);
  expect(n.entries.has(sessionKey("a"))).toBe(true);
  // A deleted session leaves the same way.
  n.snapshots([session("b", [agent("root", "idle")], "Beta")], null, T0 + 10_000);
  expect(n.entries.get(sessionKey("a"))?.removeAt).toBe(T0 + 10_000 + LEAVE_MS);
});

test("nursery: an event for a session without a pet (archived) does not make one", () => {
  const n = new Nursery("nursery", deps);
  n.snapshots([session("a", [agent("root", "idle")], "Alpha", { archived: true })], null, T0);
  n.event(ev("root", "tool_started", {}, "a"), T0 + 10);
  expect(n.entries.size).toBe(0);
});

test("nursery: your pet is always there, hears every session, and cannot be taken out", () => {
  const n = new Nursery("nursery", deps);
  n.setResident("pet", "Clawd", T0);
  expect(n.entries.get(RESIDENT)?.label).toBe("Clawd");
  n.snapshots([session("a", [agent("root", "idle")], "Alpha"), session("b", [agent("root", "idle")], "Beta")], null, T0);
  const t = T0 + 10_000;
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBeNull();
  // Either session's work makes it busy; it is idle only when both are.
  n.event(ev("root", "turn_started", {}, "a"), t);
  n.event(ev("root", "turn_started", {}, "b"), t);
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBe("working");
  n.event(ev("root", "turn_completed", { usage: {}, steps: 1 }, "a"), t + 1000);
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBe("working");
  n.event(ev("root", "turn_completed", { usage: {}, steps: 1 }, "b"), t + 2000);
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBeNull();
  // A permission request in a subagent makes it wait; the session's pet shows it too.
  n.event(ev("t1", "permission_requested", { request_id: 1, tool: "x", summary: "" }, "b"), t + 3000);
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBe("waiting");
  expect(busyOf(n.entries.get(sessionKey("b"))!.arbiter)).toBe("waiting");
  expect(busyOf(n.entries.get(sessionKey("a"))!.arbiter)).toBeNull();
  // A snapshot that no longer lists the session ends what it held on your pet.
  n.snapshots([session("a", [agent("root", "idle")], "Alpha")], null, t + 4000);
  expect(busyOf(n.entries.get(RESIDENT)!.arbiter)).toBeNull();
  // It cannot be taken out, and it is first.
  n.take(RESIDENT);
  n.take(sessionKey("a"));
  expect(n.entries.has(RESIDENT)).toBe(true);
  expect(n.entries.has(sessionKey("a"))).toBe(true);
  expect(n.sorted()[0]!.key).toBe(RESIDENT);
  // Changing it keeps the entry and changes the pet.
  n.setResident("other", "Other", t + 5000);
  expect(n.entries.get(RESIDENT)).toMatchObject({ petId: "other", label: "Other" });
});

test("nursery: pets are born in session order, your pet first, so older sessions stand behind newer ones", () => {
  const n = new Nursery("nursery", deps);
  n.setResident("pet", "Clawd", T0);
  // Listed newest first, as the daemon may list them; the creation time decides, not the order.
  n.snapshots(
    [
      session("new", [agent("root", "idle")], "New", { createdAt: 3_000 }),
      session("old", [agent("root", "idle")], "Old", { createdAt: 1_000 }),
      session("unknown", [agent("root", "idle")], "Unknown"),
    ],
    null,
    T0,
  );
  const born = (key: string) => n.entries.get(key)!.born;
  expect(born(RESIDENT)).toBe(FIRST);
  expect(born(sessionKey("old"))).toBe(1_000);
  expect(born(sessionKey("new"))).toBe(3_000);
  // Without a creation time a session counts from when the nursery first saw it.
  expect(born(sessionKey("unknown"))).toBe(T0);
  const depth = [...n.entries.values()].sort((a, b) => a.born - b.born || a.seq - b.seq).map((e) => e.key);
  expect(depth).toEqual([RESIDENT, sessionKey("old"), sessionKey("new"), sessionKey("unknown")]);
});

test("nursery: a session pet's kind comes from its environment, then the setting, then your pet", () => {
  const ids = ["a", "b", "c"];
  expect(sessionPet(ids, "c", MIX, "s1", "a")).toBe("c");
  // An environment pet that is not installed is ignored.
  expect(sessionPet(ids, "gone", "b", "s1", "a")).toBe("b");
  expect(sessionPet(ids, null, "b", "s1", "a")).toBe("b");
  expect(sessionPet(ids, null, MIX, "s1", "a")).toBe(pickPet(ids, MIX, "s1", false));
  expect(sessionPet(ids, null, MINE, "s1", "c")).toBe("c");
  expect(sessionPet(ids, null, MINE, "s1", null)).toBe("a");
  expect(sessionPet([], null, MIX, "s1", null)).toBeNull();
  // In the nursery the environment's pet decides, and changes with it.
  const pick = (who: { envPet: string | null; sessionId: string }) => sessionPet(ids, who.envPet, MINE, who.sessionId, "a");
  const n = new Nursery("nursery", { envFor: () => env, assign: pick });
  n.snapshots([session("s1", [agent("root", "idle")], "One", { pet: "c" }), session("s2", [agent("root", "idle")], "Two")], null, T0);
  expect(n.entries.get(sessionKey("s1"))?.petId).toBe("c");
  expect(n.entries.get(sessionKey("s2"))?.petId).toBe("a");
  n.snapshots([session("s1", [agent("root", "idle")], "One", { pet: "b" }), session("s2", [agent("root", "idle")], "Two")], null, T0 + 10);
  expect(n.entries.get(sessionKey("s1"))?.petId).toBe("b");
});

test("session nursery: follows the selection, one pet per agent, finished subagents leave after a while", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "working", "fix"), agent("t2", "failed", "old")], "One"), session("s2", [agent("root", "idle")], "Two")], "s1", T0);
  expect(n.sorted().map((e) => e.key)).toEqual(["s1:root", "s1:t1"]);
  expect(n.summary()).toEqual({ pets: 2, sessions: 0, busy: 0, subagents: 1 });
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
  const n = new Nursery("nursery", deps);
  n.setResident("pet", "Pet", T0);
  n.snapshots([session("s1", [agent("root", "working"), agent("t1", "working", "Profile: critic")])], null, T0);
  expect(n.entries.get(sessionKey("s1"))!.arbiter.sources.t1?.base).toBe("review");
  expect(n.entries.get(RESIDENT)!.arbiter.sources["s1:t1"]?.base).toBe("review");
});

test("memory_saved overlays eating, which ends by itself", () => {
  const n = new Nursery("session", deps);
  n.snapshots([session("s1", [agent("root", "working")])], "s1", T0);
  n.event(ev("root", "memory_saved", { name: "m", scope: "user" }), T0 + 5000);
  expect(n.entries.get("s1:root")!.arbiter.sources.main?.over).toBe("eating");
  n.tick(T0 + 5000 + 3000);
  expect(n.entries.get("s1:root")!.arbiter.sources.main?.over).toBeNull();
});

test("extra pets are added, patted, taken out; migrated residents keep their ids", () => {
  const n = new Nursery("nursery", deps);
  n.add("u1", "pet", "Rex", T0);
  n.add("u1", "pet", "Dup", T0);
  expect(n.entries.size).toBe(1);
  expect(n.entries.get("u1")?.kind).toBe("extra");
  const t = T0 + 10_000;
  expect(shown(n, "u1", t)).toBe("idle");
  n.pat("u1", t);
  expect(shown(n, "u1", t)).toBe("petted");
  // Session events do not reach an extra pet.
  n.event(ev("root", "turn_started", {}), t);
  expect(busyOf(n.entries.get("u1")!.arbiter)).toBeNull();
  // Left alone it falls asleep.
  expect(shown(n, "u1", t + 400_000)).toBe("sleeping");
  // Snapshots and reassigning leave it be.
  n.snapshots([], null, t);
  n.reassign(t);
  expect(n.entries.get("u1")?.petId).toBe("pet");
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
