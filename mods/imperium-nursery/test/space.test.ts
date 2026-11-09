// The shared room, pure: bounds, wandering, separation, pairing, spawning and layouts.

import { test } from "node:test";

import {
  AREA_PER_PET,
  MIN_ROOM_HEIGHT,
  PLAY_SPEED,
  SPEED,
  boundsOf,
  clampTo,
  edgeTarget,
  nearestPartner,
  newRoamer,
  pickTarget,
  playTarget,
  reducedLayout,
  roam,
  roomHeight,
  separate,
  spawnPoint,
  stepToward,
} from "../src/space.ts";
import type { Body, Bounds, Pos } from "../src/space.ts";
import { expect } from "./expect.ts";

const M = { half: 48, above: 64, below: 18, pad: 4 };
const room = { width: 500, height: 300 };
const B = boundsOf(room, M);

/** A small seeded generator, so wandering is reproducible. */
const seeded = (seed: number): (() => number) => {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
};

const inside = (b: Bounds, p: Pos): boolean => p.x >= b.minX && p.x <= b.maxX && p.y >= b.minY && p.y <= b.maxY;

test("bounds keep the whole pet (feet, sprite above, label below) inside the room", () => {
  expect(B).toEqual({ minX: 52, maxX: 448, minY: 68, maxY: 278 });
  expect(clampTo(B, { x: -10, y: 999 })).toEqual({ x: 52, y: 278 });
  expect(clampTo(B, { x: 200, y: 100 })).toEqual({ x: 200, y: 100 });
  // A room too small for a pet collapses to its middle instead of inverting.
  const tiny = boundsOf({ width: 40, height: 40 }, M);
  expect(tiny.minX).toBe(tiny.maxX);
  expect(tiny.minY).toBe(tiny.maxY);
});

test("the room grows with the crowd: a minimum area per pet", () => {
  expect(roomHeight(1, 500)).toBe(MIN_ROOM_HEIGHT);
  expect(roomHeight(40, 500)).toBe(Math.ceil((40 * AREA_PER_PET) / 500));
  expect(roomHeight(40, 1000)).toBeLessThan(roomHeight(40, 500));
  expect(roomHeight(5, 0)).toBe(MIN_ROOM_HEIGHT);
});

test("wander targets lie anywhere in the room and are not next to where the pet stands", () => {
  const rand = seeded(7);
  let wide = 0;
  let deep = 0;
  const from = { x: 100, y: 100 };
  for (let i = 0; i < 200; i++) {
    const t = pickTarget(B, from, rand);
    expect(inside(B, t)).toBe(true);
    if (Math.abs(t.x - from.x) > 200) wide++;
    if (Math.abs(t.y - from.y) > 100) deep++;
  }
  expect(wide).toBeGreaterThan(20);
  expect(deep).toBeGreaterThan(20);
});

test("a step goes at the given speed and never overshoots", () => {
  const s = stepToward({ x: 0, y: 0 }, { x: 30, y: 40 }, 50, 100);
  expect(Math.abs(s.x - 3)).toBeLessThan(1e-9);
  expect(Math.abs(s.y - 4)).toBeLessThan(1e-9);
  expect(s.arrived).toBe(false);
  const end = stepToward({ x: 0, y: 0 }, { x: 3, y: 4 }, 50, 1000);
  expect(end.x).toBe(3);
  expect(end.y).toBe(4);
  expect(end.arrived).toBe(true);
  expect(stepToward({ x: 5, y: 5 }, { x: 5, y: 5 }, 50, 100).dx).toBe(0);
});

test("a roamer pauses, walks across the room at a steady speed, and pauses again", () => {
  const rand = seeded(3);
  let r = newRoamer({ x: 100, y: 100 }, 0, rand);
  const first = r.nextAt;
  // Standing still until the pause is over.
  for (let t = 0; t < first; t += 50) {
    const out = roam(r, B, t, 50, rand);
    expect(out.dx).toBe(0);
    r = out.roamer;
  }
  let moved = 0;
  let travelled = 0;
  for (let t = first; t < first + 120_000; t += 50) {
    const out = roam(r, B, t, 50, rand);
    r = out.roamer;
    expect(inside(B, r)).toBe(true);
    const step = Math.hypot(out.dx, out.dy);
    expect(step).toBeLessThanOrEqual((SPEED * 50) / 1000 + 1e-9);
    if (step > 0) moved++;
    travelled += step;
  }
  expect(moved).toBeGreaterThan(100);
  // Over two minutes at 22 px/s a pet that pauses 2 to 8 s between walks covers real ground.
  expect(travelled).toBeGreaterThan(300);
});

test("separation pushes overlapping pets apart, spares pets at work, and leaves play partners alone", () => {
  const body = (key: string, x: number, y: number, movable = true, group: string | null = null): Body => ({ key, x, y, movable, group });
  const two = separate([body("a", 100, 100), body("b", 110, 100)], 1000);
  expect(two[0]!.x).toBeLessThan(100);
  expect(two[1]!.x).toBeGreaterThan(110);
  // A pet at work stays; the free one gives way all by itself.
  const busy = separate([body("a", 100, 100, false), body("b", 110, 100)], 1000);
  expect(busy[0]).toEqual({ x: 100, y: 100 });
  expect(busy[1]!.x).toBeGreaterThan(two[1]!.x);
  // Partners of one play stand close on purpose.
  const mates = separate([body("a", 100, 100, true, "p"), body("b", 110, 100, true, "p")], 1000);
  expect(mates).toEqual([{ x: 100, y: 100 }, { x: 110, y: 100 }]);
  // Far apart: untouched. On the very same spot: still pushed apart.
  expect(separate([body("a", 0, 0), body("b", 300, 0)], 1000)).toEqual([{ x: 0, y: 0 }, { x: 300, y: 0 }]);
  const same = separate([body("a", 50, 50), body("b", 50, 50)], 1000);
  expect(same[0]!.x).not.toBe(same[1]!.x);
});

test("repeated separation spreads a crowd on one spot", () => {
  let bodies: Body[] = Array.from({ length: 6 }, (_, i) => ({ key: String(i), x: 200 + i, y: 150, movable: true, group: null }));
  for (let t = 0; t < 200; t++) {
    const out = separate(bodies, 50);
    bodies = bodies.map((b, i) => ({ ...b, ...clampTo(B, out[i]!) }));
  }
  let least = Infinity;
  for (let i = 0; i < bodies.length; i++) for (let j = i + 1; j < bodies.length; j++) least = Math.min(least, Math.hypot(bodies[i]!.x - bodies[j]!.x, (bodies[i]!.y - bodies[j]!.y) * 2));
  expect(least).toBeGreaterThan(30);
});

test("the partner is usually the nearest free pet, sometimes any", () => {
  const others = [
    { key: "far", x: 400, y: 0 },
    { key: "near", x: 20, y: 10 },
    { key: "mid", x: 150, y: 0 },
  ];
  expect(nearestPartner({ x: 0, y: 0 }, others, () => 0)).toBe("near");
  expect(nearestPartner({ x: 0, y: 0 }, others, () => 0.69)).toBe("near");
  // The other 30 %: a random pick over the sorted pets.
  const picks = new Set<string | null>();
  for (let i = 0; i < 30; i++) picks.add(nearestPartner({ x: 0, y: 0 }, others, () => (i % 2 === 0 ? 0.9 : i / 30)));
  expect(picks.has("far") || picks.has("mid")).toBe(true);
  expect(nearestPartner({ x: 0, y: 0 }, [], () => 0)).toBeNull();
});

test("a new pet appears beside its anchor, or far from the others", () => {
  const anchor = { x: 200, y: 150 };
  for (let i = 0; i < 20; i++) {
    const p = spawnPoint(B, [anchor], seeded(i), anchor);
    expect(inside(B, p)).toBe(true);
    expect(Math.abs(p.x - anchor.x)).toBeGreaterThanOrEqual(50);
    expect(Math.abs(p.x - anchor.x)).toBeLessThan(130);
  }
  // Without an anchor it picks the spot farthest from the pets already there.
  const crowd = [{ x: 60, y: 80 }, { x: 60, y: 270 }, { x: 250, y: 150 }];
  const p = spawnPoint(B, crowd, seeded(5));
  expect(inside(B, p)).toBe(true);
  const gap = Math.min(...crowd.map((q) => Math.hypot(q.x - p.x, q.y - p.y)));
  expect(gap).toBeGreaterThan(60);
});

test("a window resize pulls pets back inside by clamping", () => {
  const small = boundsOf({ width: 200, height: 180 }, M);
  const out = clampTo(small, { x: 420, y: 270 });
  expect(inside(small, out)).toBe(true);
  expect(out).toEqual({ x: small.maxX, y: small.maxY });
});

test("reduced motion: a deterministic layout spreads the pets over the room", () => {
  const spots = reducedLayout(7, B);
  expect(spots).toHaveLength(7);
  expect(reducedLayout(7, B)).toEqual(spots);
  for (const p of spots) expect(inside(B, p)).toBe(true);
  const keys = new Set(spots.map((p) => `${Math.round(p.x)},${Math.round(p.y)}`));
  expect(keys.size).toBe(7);
  expect(Math.max(...spots.map((p) => p.x)) - Math.min(...spots.map((p) => p.x))).toBeGreaterThan(200);
  expect(reducedLayout(1, B)).toHaveLength(1);
  expect(reducedLayout(0, B)).toHaveLength(0);
});

test("play targets: to the partner's side, away from the partner, or stay put", () => {
  const me = { x: 100, y: 150 };
  const other = { x: 300, y: 150 };
  const to = playTarget(1, me, other, B)!;
  expect(Math.abs(to.x - 240)).toBeLessThan(1e-9);
  expect(Math.abs(to.y - 150)).toBeLessThan(1);
  const away = playTarget(-1, me, other, B)!;
  expect(away.x).toBeLessThan(me.x);
  expect(inside(B, away)).toBe(true);
  // Cornered against the left wall, it runs along the wall instead of nowhere.
  const wall = { x: B.minX, y: 150 };
  const along = playTarget(-1, wall, { x: 300, y: 150 }, B)!;
  expect(Math.hypot(along.x - wall.x, along.y - wall.y)).toBeGreaterThan(20);
  expect(playTarget(0, me, other, B)).toBeNull();
  expect(PLAY_SPEED).toBeGreaterThan(SPEED);
});

test("a finished pet leaves by the nearer side", () => {
  expect(edgeTarget(B, { x: 100, y: 120 }).x).toBeLessThan(B.minX);
  expect(edgeTarget(B, { x: 400, y: 120 }).x).toBeGreaterThan(B.maxX);
  expect(edgeTarget(B, { x: 400, y: 120 }).y).toBe(120);
});
