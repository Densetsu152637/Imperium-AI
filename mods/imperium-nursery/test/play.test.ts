// Play: choosing free pets, the scripted interactions, and their interruption. No DOM.

import { test } from "node:test";

import type { SessionEvent, SessionSnapshot } from "@imperium/mod-sdk";

import type { Env } from "../src/arbiter.ts";
import { Nursery } from "../src/nursery.ts";
import { allDrawn } from "../src/pet.ts";
import { angleTo, chooseKind, isFree, isOver, pickPair, poseAt, reachPx, startPlay, stepsOf } from "../src/play.ts";
import type { Candidate, PlayKind } from "../src/play.ts";
import { busyOf } from "../src/route.ts";
import { PLAY_SPEED, stepWalker } from "../src/wander.ts";
import { expect } from "./expect.ts";

const T0 = 1_000_000;
const KINDS: PlayKind[] = ["greet", "chase", "highfive", "nap", "showoff"];
const env: Env = { catalog: allDrawn(), sleepAfterMs: 600_000, fidget: false, rng: () => 0 };
const deps = { envFor: () => env, assign: () => "pet" };

const cand = (key: string, extra: Partial<Candidate> = {}): Candidate => ({ key, shown: "idle", kind: "loop", hidden: false, busy: false, leaving: false, inPlay: false, quietFor: 10_000, ...extra });
const name = (n: Nursery, key: string, at: number): string => n.view(n.entries.get(key)!, at).name;

/** A "My nursery" of three settled pets: a, b, c. */
const settled = (): Nursery => {
  const n = new Nursery("mine", deps);
  for (const k of ["a", "b", "c"]) n.add(k, "pet", k, T0);
  n.rand = () => 0;
  return n;
};

const run = (n: Nursery, from: number, to: number, reduced = false): void => {
  for (let t = from; t <= to; t += 100) n.tickPlay(t, reduced);
};

test("a free pet is awake, idle at P6, settled, and not at work or leaving", () => {
  expect(isFree(cand("a"))).toBe(true);
  expect(isFree(cand("a", { shown: "fidget", kind: "once" }))).toBe(true);
  for (const bad of [{ shown: "sleeping" }, { shown: "working" }, { shown: "petted", kind: "once" as const }, { hidden: true }, { busy: true }, { leaving: true }, { inPlay: true }, { quietFor: 1000 }]) {
    expect(isFree(cand("a", bad))).toBe(false);
  }
});

test("pairs are free neighbours; none when nobody is free next to anybody", () => {
  const next = (k: string): string[] => ({ a: ["b"], b: ["a", "c"], c: ["b"] })[k] ?? [];
  const pair = pickPair([cand("a"), cand("b"), cand("c")], next, () => 0);
  expect(pair?.a.key).toBe("a");
  expect(pair?.b.key).toBe("b");
  // b is busy: a and c are free but not neighbours.
  expect(pickPair([cand("a"), cand("b", { busy: true }), cand("c")], next, () => 0)).toBeNull();
  expect(pickPair([cand("a")], next, () => 0)).toBeNull();
});

test("a nap needs drowsy pets; the weights reach every other interaction", () => {
  const seen = new Set<PlayKind>();
  for (let i = 0; i < 100; i++) seen.add(chooseKind(() => i / 100, false));
  expect([...seen].sort()).toEqual(["chase", "greet", "highfive", "showoff"]);
  const all = new Set<PlayKind>();
  for (let i = 0; i < 100; i++) all.add(chooseKind(() => i / 100, true));
  expect(all.has("nap")).toBe(true);
});

test("every script ends with both pets back at their slot and idle", () => {
  for (const kind of KINDS) {
    const n = settled();
    const t = T0 + 10_000;
    n.playing = startPlay(kind, ["a", "b"], t, () => 0);
    const { endMs } = stepsOf(kind);
    run(n, t, t + endMs + 100);
    expect(n.playing).toBeNull();
    for (const key of ["a", "b"]) {
      expect(name(n, key, t + endMs + 4000)).toBe("idle");
      expect(n.view(n.entries.get(key)!, t + endMs + 4000).kind).toBe("loop");
    }
    // The last pose of each pet is idle or a walk back to its slot.
    const p = startPlay(kind, ["a", "b"], t, () => 0);
    for (const who of [0, 1] as const) {
      const last = poseAt(p, who, t + endMs);
      expect(last.mode === "idle" || (last.mode === "walk" && last.toward === 0)).toBe(true);
    }
    expect(isOver(p, t + endMs - 1)).toBe(false);
    expect(isOver(p, t + endMs)).toBe(true);
  }
});

test("the shots of a script reach the pets: greeting waves, high-five jumps", () => {
  const n = settled();
  const t = T0 + 10_000;
  n.playing = startPlay("greet", ["a", "b"], t, () => 0);
  run(n, t, t + 1500);
  expect(name(n, "a", t + 1500)).toBe("waving");
  expect(name(n, "b", t + 1500)).toBe("waving");
  expect(name(n, "c", t + 1500)).toBe("idle");
  const m = settled();
  m.playing = startPlay("highfive", ["a", "b"], t, () => 0);
  run(m, t, t + 2500);
  expect(name(m, "a", t + 2500)).toBe("jumping");
  expect(name(m, "b", t + 2500)).toBe("jumping");
});

test("a pet that does not draw an animation still plays along: it just stays idle", () => {
  const sparse: Env = { ...env, catalog: Object.fromEntries(Object.entries(allDrawn()).map(([k, a]) => [k, { ...a, drawn: k === "idle" }])) };
  const n = new Nursery("mine", { envFor: () => sparse, assign: () => "pet" });
  n.add("a", "pet", "a", T0);
  n.add("b", "pet", "b", T0);
  const t = T0 + 10_000;
  n.playing = startPlay("greet", ["a", "b"], t, () => 0);
  run(n, t, t + 4000);
  expect(name(n, "a", t + 2000)).toBe("idle");
  expect(n.playing).toBeNull();
});

test("a real event ends play for both pets at once", () => {
  const ev = (agentId: string, type: string): SessionEvent => ({ sessionId: "s1", agentId, event: { type } });
  const snap: SessionSnapshot = {
    sessionId: "s1",
    title: "S",
    projectDir: "/p",
    status: "idle",
    agents: [
      { agentId: "root", parentId: null, brief: "", status: "idle" },
      { agentId: "t1", parentId: "root", brief: "x", status: "idle" },
    ],
  };
  const n = new Nursery("session", deps);
  n.snapshots([snap], "s1", T0);
  const t = T0 + 10_000;
  n.playing = startPlay("chase", ["s1:root", "s1:t1"], t, () => 0);
  run(n, t, t + 1000);
  expect(n.playing).not.toBeNull();
  n.event(ev("t1", "tool_started"), t + 1050);
  expect(n.playing).toBeNull();
  expect(name(n, "s1:t1", t + 1050)).toBe("working");
  // The partner is not at work and is free to idle again.
  expect(busyOf(n.entries.get("s1:root")!.arbiter)).toBeNull();
  expect(n.playInfo("s1:root")).toBeNull();
  // A snapshot that says an agent is busy ends play too.
  n.playing = startPlay("greet", ["s1:root", "s1:t1"], t + 2000, () => 0);
  n.entries.get("s1:t1")!.arbiter.sources.main!.base = null;
  n.snapshots([{ ...snap, agents: [snap.agents[0]!, { ...snap.agents[1]!, status: "working" }] }], "s1", t + 2100);
  n.tickPlay(t + 2200, false);
  expect(n.playing).toBeNull();
});

test("patting a playing pet ends the play; leaving pets and busy pets never start one", () => {
  const n = settled();
  const t = T0 + 10_000;
  n.playing = startPlay("greet", ["a", "b"], t, () => 0);
  n.pat("b", t + 100);
  expect(n.playing).toBeNull();
  expect(name(n, "b", t + 100)).toBe("petted");
  const s = new Nursery("session", deps);
  const base = { sessionId: "s1", title: "S", projectDir: "/p", status: "idle" };
  const root = { agentId: "root", parentId: null, brief: "", status: "idle" as const };
  s.snapshots([{ ...base, agents: [root, { agentId: "t1", parentId: "root", brief: "x", status: "working" }] }], "s1", T0);
  s.snapshots([{ ...base, agents: [root, { agentId: "t1", parentId: "root", brief: "x", status: "finished" }] }], "s1", T0 + 1000);
  expect(s.entries.get("s1:t1")!.removeAt).not.toBeNull();
  s.rand = () => 0;
  run(s, T0 + 5000, T0 + 100_000);
  expect(s.playing).toBeNull();
  const w = settled();
  for (const k of ["a", "b", "c"]) w.entries.get(k)!.arbiter.sources.main = { kind: "main", base: "working", over: null, since: T0, waits: {} };
  run(w, T0 + 5000, T0 + 100_000);
  expect(w.playing).toBeNull();
});

test("play starts on its own after the delay, and not when switched off", () => {
  const n = settled();
  n.tickPlay(T0 + 5000, false);
  expect(n.playing).toBeNull();
  let started = false;
  for (let t = T0 + 5000; t <= T0 + 5000 + 46_000; t += 100) {
    n.tickPlay(t, false);
    if (n.playing) started = n.playInfo("a") !== null || n.playInfo("b") !== null || started;
  }
  expect(started).toBe(true);
  const off = settled();
  off.playEnabled = false;
  run(off, T0 + 5000, T0 + 200_000);
  expect(off.playing).toBeNull();
});

test("reduced motion: no play starts and one in progress stops", () => {
  const n = settled();
  run(n, T0 + 5000, T0 + 200_000, true);
  expect(n.playing).toBeNull();
  n.playing = startPlay("greet", ["a", "b"], T0 + 210_000, () => 0);
  n.tickPlay(T0 + 210_100, true);
  expect(n.playing).toBeNull();
});

test("reach and facing: half the gap between the sprites toward the partner, at most 40 px", () => {
  expect(reachPx(200, 64)).toBe(40);
  expect(reachPx(-200, 64)).toBe(-40);
  expect(reachPx(100, 64)).toBe(15);
  expect(reachPx(60, 64)).toBe(0);
  expect(angleTo(10, 0)).toBe(90);
  expect(angleTo(-10, 0)).toBe(270);
  expect(angleTo(0, -10)).toBe(0);
  expect(angleTo(0, 10)).toBe(180);
});

test("a playing walker covers ground faster than a wanderer and stops at its target", () => {
  let w = { x: 0, target: 30, nextAt: Infinity };
  for (let t = 0; t < 2000; t += 50) w = stepWalker(w, t, 50, true, () => 0, 0, PLAY_SPEED).walker;
  expect(w.x).toBe(30);
  // A busy pet is eased back to its slot with no walk animation.
  const home = stepWalker({ x: 10, target: 10, nextAt: 0 }, 0, 100, false, () => 0);
  expect(home.dir).toBe(0);
  expect(home.walker.x).toBeLessThan(10);
});
