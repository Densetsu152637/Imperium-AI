// The shared room, pure: bounds, wandering, separation, pairing, spawning and layouts.

import { test } from "node:test";

import {
  AREA_PER_PET,
  MIN_ROOM_HEIGHT,
  MIN_SCALE,
  PLAY_SPEED,
  SPEED,
  boundsOf,
  boxSize,
  boxSlots,
  boxesOverlap,
  clampTo,
  clearTarget,
  edgeTarget,
  fitMetrics,
  fitScale,
  isTight,
  MIN_LABEL,
  nearestPartner,
  newRoamer,
  pickTarget,
  playTarget,
  reducedLayout,
  roam,
  roomHeight,
  scaleMetrics,
  stranded,
  separate,
  spawnPoint,
  spreadSpots,
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

// K7: a pet's box is its sprite plus its label (112 px wide, 112 px of sprite and an 18 px label
// line), so crowds are placed by whole boxes, and drawn smaller when the room is narrow.
const FULL = { half: 56, above: 112, below: 18, pad: 4 };
const narrow = { width: 140, height: 300 };

/** Every pair of boxes that overlap, and every box that is not inside the room. */
const crowding = (spots: Pos[], m: typeof FULL, room: { width: number; height: number }): { pairs: number; outside: number } => {
  const box = boxSize(m);
  let pairs = 0;
  for (let i = 0; i < spots.length; i++) for (let j = i + 1; j < spots.length; j++) if (boxesOverlap(spots[i]!, spots[j]!, m)) pairs++;
  const outside = spots.filter((p) => p.x - box.width / 2 < 0 || p.x + box.width / 2 > room.width || p.y - m.above < 0 || p.y + m.below > room.height).length;
  return { pairs, outside };
};

test("a box is the sprite above the feet and the label below", () => {
  expect(boxSize(FULL)).toEqual({ width: 112, height: 130 });
  expect(boxesOverlap({ x: 100, y: 200 }, { x: 211, y: 200 }, FULL)).toBe(true);
  expect(boxesOverlap({ x: 100, y: 200 }, { x: 212, y: 200 }, FULL)).toBe(false);
  // The label under one pet is part of its box: a pet 120 px behind it still overlaps.
  expect(boxesOverlap({ x: 100, y: 200 }, { x: 100, y: 320 }, FULL)).toBe(true);
  expect(boxesOverlap({ x: 100, y: 200 }, { x: 100, y: 330 }, FULL)).toBe(false);
});

test("ten pets in a 140 x 300 room never have overlapping boxes", () => {
  const scale = fitScale(narrow, 10, FULL);
  expect(scale).toBeLessThan(1);
  expect(scale).toBeGreaterThanOrEqual(MIN_SCALE);
  const m = fitMetrics(narrow, 10, FULL).metrics;
  // Laid out in one go (reduced motion).
  const laid = spreadSpots(10, narrow, m);
  expect(laid).toHaveLength(10);
  expect(crowding(laid, m, narrow)).toEqual({ pairs: 0, outside: 0 });
  // Arriving one at a time, wherever they pick.
  for (const seed of [1, 2, 3, 4, 5]) {
    const rand = seeded(seed);
    const bounds = boundsOf(narrow, m);
    const placed: Pos[] = [];
    for (let i = 0; i < 10; i++) placed.push(spawnPoint(bounds, placed, rand, i > 0 && i % 3 === 0 ? placed[0]! : null, undefined, { room: narrow, m }));
    expect(crowding(placed, m, narrow)).toEqual({ pairs: 0, outside: 0 });
    for (const p of placed) expect(inside(bounds, p)).toBe(true);
  }
});

test("the slots are whole boxes that stay inside the room", () => {
  const slots = boxSlots(room, FULL);
  expect(slots.length).toBeGreaterThan(0);
  expect(crowding(slots, FULL, room)).toEqual({ pairs: 0, outside: 0 });
  // Too small for one box: no slots, so callers fall back to the soft layout.
  expect(boxSlots({ width: 100, height: 300 }, FULL)).toEqual([]);
  expect(boxSlots({ width: 500, height: 100 }, FULL)).toEqual([]);
  // Every slot is a legal place to stand.
  const b = boundsOf(room, FULL);
  for (const p of slots) expect(inside(b, p)).toBe(true);
});

test("pets are drawn at full size while they fit, and shrink as the room fills, down to a minimum", () => {
  expect(fitScale({ width: 500, height: 300 }, 2, FULL)).toBe(1);
  // Pets get spare room to wander in: more boxes fit than there are pets, unless already at the minimum.
  const sized = fitMetrics({ width: 105, height: 515 }, 6, FULL);
  expect(sized.scale).toBeLessThan(1);
  expect(boxSlots({ width: 105, height: 515 }, sized.metrics).length).toBeGreaterThanOrEqual(7);
  const few = fitScale(narrow, 3, FULL);
  const many = fitScale(narrow, 10, FULL);
  expect(many).toBeLessThan(few);
  // More pets than a room can hold even at the minimum: the minimum, and the layout falls back.
  expect(fitScale(narrow, 200, FULL)).toBe(MIN_SCALE);
  expect(spreadSpots(200, narrow, scaleMetrics(FULL, MIN_SCALE))).toHaveLength(200);
  expect(spreadSpots(0, narrow, FULL)).toEqual([]);
  // A new pet that finds no free box still gets a place inside the room.
  const m = scaleMetrics(FULL, MIN_SCALE);
  const full = spreadSpots(boxSlots(narrow, m).length, narrow, m);
  expect(inside(boundsOf(narrow, m), spawnPoint(boundsOf(narrow, m), full, seeded(9), null, undefined, { room: narrow, m }))).toBe(true);
});

test("separation by box keeps the slots of a grid where they are", () => {
  const m = scaleMetrics(FULL, 0.5);
  const slots = boxSlots({ width: 300, height: 300 }, m);
  const bodies: Body[] = slots.map((p, i) => ({ key: String(i), x: p.x, y: p.y, movable: true, group: null }));
  const out = separate(bodies, 1000, undefined, boxSize(m));
  out.forEach((p, i) => expect(p).toEqual({ x: slots[i]!.x, y: slots[i]!.y }));
  // Two pets whose boxes overlap are pushed apart.
  const near = separate([{ key: "a", x: 100, y: 100, movable: true, group: null }, { key: "b", x: 110, y: 100, movable: true, group: null }], 1000, undefined, boxSize(m));
  expect(near[1]!.x - near[0]!.x).toBeGreaterThan(10);
});

test("a roaming pet picks a spot whose whole box is clear of the others, when the room has one", () => {
  const m = fitMetrics(narrow, 6, FULL).metrics;
  const bounds = boundsOf(narrow, m);
  const standing = spreadSpots(6, narrow, m);
  for (let seed = 1; seed <= 20; seed++) {
    const t = clearTarget(bounds, standing[0]!, standing.slice(1), m, narrow, seeded(seed));
    expect(t).not.toBeNull();
    expect(inside(bounds, t!)).toBe(true);
    for (const q of standing.slice(1)) expect(boxesOverlap(t!, q, m)).toBe(false);
  }
  // Nowhere clear (every grid spot taken): null, and the caller takes any spot.
  const all = boxSlots(narrow, m);
  expect(clearTarget(bounds, { x: -999, y: -999 }, all, m, narrow, seeded(3))).toBeNull();
});

test("pets shrink only when their boxes do not fit, and labels stay readable", () => {
  // A 122 px wide room at its minimum height: one pet at full size, two a little smaller.
  const room122 = { width: 122, height: 220 };
  expect(fitScale(room122, 1, FULL)).toBe(1);
  expect(fitScale(room122, 2, FULL)).toBeGreaterThanOrEqual(0.7);
  expect(fitScale(room122, 0, FULL)).toBe(1);
  // A room as tall as the nursery makes it for ten pets 121 px wide: the sprite shrinks, the label stays.
  const tall = { width: 121, height: roomHeight(10, 121) };
  const { scale, metrics } = fitMetrics(tall, 10, FULL);
  expect(scale).toBeLessThan(1);
  expect(scale).toBeGreaterThanOrEqual(MIN_SCALE);
  expect(metrics.half * 2).toBeGreaterThanOrEqual(MIN_LABEL);
  const spots = spreadSpots(10, tall, metrics);
  expect(crowding(spots, metrics, tall)).toEqual({ pairs: 0, outside: 0 });
  // A label is never wider than it was.
  expect(scaleMetrics(FULL, 1).half).toBe(FULL.half);
  expect(scaleMetrics({ ...FULL, half: 20 }, 0.5).half).toBe(20);
  expect(scaleMetrics(FULL, 0.3).half).toBe(MIN_LABEL / 2);
  expect(scaleMetrics(FULL, 0.3, false).half).toBe(17);
});

test("after the boxes grow, only pets that cannot move are put back; walking and just-dropped pets stay", () => {
  const m = scaleMetrics(FULL, 1);
  const a = { x: 60, y: 130 };
  // The second pet was dropped on the first (it can move), the third is asleep on the first, the fourth asleep far away.
  const pets = [
    { pos: a, moves: true },
    { pos: { x: 70, y: 150 }, moves: true },
    { pos: { x: 65, y: 140 }, moves: false },
    { pos: { x: 60, y: 600 }, moves: false },
  ];
  expect(stranded(pets, m)).toEqual([2]);
  // Remove the sleeper: nobody is put back, the dropped pet stays where it was.
  expect(stranded([pets[0]!, pets[1]!, pets[3]!], m)).toEqual([]);
});

test("a room is tight only when the pets cannot fit with room to move", () => {
  const tightFor = (room: { width: number; height: number }, count: number): boolean => {
    const { scale, metrics } = fitMetrics(room, count, FULL);
    return isTight(room, count, metrics, scale);
  };
  // Sized to fit exactly is not tight: these pets wander.
  expect(tightFor({ width: 122, height: 220 }, 1)).toBe(false);
  expect(tightFor({ width: 122, height: 220 }, 2)).toBe(false);
  expect(tightFor({ width: 122, height: 222 }, 3)).toBe(false);
  expect(tightFor({ width: 122, height: 500 }, 3)).toBe(false);
  // More pets than boxes at the smallest size, or no box to spare there: tight.
  expect(tightFor(narrow, 200)).toBe(true);
  const m = scaleMetrics(FULL, MIN_SCALE, false);
  expect(isTight(narrow, boxSlots(narrow, m).length, m, MIN_SCALE)).toBe(true);
  expect(isTight(narrow, 1, m, MIN_SCALE)).toBe(false);
});
