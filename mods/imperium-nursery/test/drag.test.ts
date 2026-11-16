// Picking a pet up, pure: the threshold, the grab offset, and the clamp to the room.

import { test } from "node:test";

import { DRAG_THRESHOLD, NUDGE, NUDGE_FAR, follow, grab, inRoom, nudge } from "../src/drag.ts";
import { boundsOf } from "../src/space.ts";
import { expect } from "./expect.ts";

const B = boundsOf({ width: 500, height: 300 }, { half: 56, above: 64, below: 18, pad: 4 });

test("a press that has not moved far enough is a click, not a drag", () => {
  const d = grab({ x: 100, y: 100 }, { x: 110, y: 130 });
  const out = follow(d, { x: 100 + DRAG_THRESHOLD - 1, y: 100 }, B);
  expect(out.feet).toBe(null);
  expect(out.drag.lifted).toBe(false);
});

test("past the threshold the pet is lifted and keeps the offset it was grabbed at", () => {
  const d = grab({ x: 100, y: 100 }, { x: 110, y: 130 });
  const out = follow(d, { x: 200, y: 150 }, B);
  expect(out.drag.lifted).toBe(true);
  expect(out.feet).toEqual({ x: 210, y: 180 });
});

test("once lifted it follows even when the pointer comes back near the start", () => {
  const lifted = follow(grab({ x: 100, y: 100 }, { x: 100, y: 100 }), { x: 160, y: 100 }, B).drag;
  const out = follow(lifted, { x: 101, y: 100 }, B);
  expect(out.feet).toEqual({ x: 101, y: 100 });
});

test("the feet are clamped to the room however far the pointer goes", () => {
  const lifted = follow(grab({ x: 100, y: 100 }, { x: 100, y: 100 }), { x: 140, y: 100 }, B).drag;
  expect(follow(lifted, { x: -900, y: -900 }, B).feet).toEqual({ x: B.minX, y: B.minY });
  expect(follow(lifted, { x: 9000, y: 9000 }, B).feet).toEqual({ x: B.maxX, y: B.maxY });
});

test("a room too small for a pet pins it to the middle", () => {
  const tiny = boundsOf({ width: 40, height: 40 }, { half: 56, above: 64, below: 18, pad: 4 });
  const lifted = follow(grab({ x: 5, y: 5 }, { x: 5, y: 5 }), { x: 30, y: 30 }, tiny).drag;
  expect(follow(lifted, { x: 0, y: 0 }, tiny).feet).toEqual({ x: tiny.minX, y: tiny.minY });
});

test("inRoom subtracts the room's corner", () => {
  expect(inRoom({ x: 150, y: 90 }, { left: 40, top: 30 })).toEqual({ x: 110, y: 60 });
});

test("arrow keys nudge a pet, shift nudges farther, other keys do nothing", () => {
  const at = { x: 200, y: 150 };
  expect(nudge("ArrowLeft", at, B)).toEqual({ x: 200 - NUDGE, y: 150 });
  expect(nudge("ArrowDown", at, B, true)).toEqual({ x: 200, y: 150 + NUDGE_FAR });
  expect(nudge("a", at, B)).toBe(null);
});

test("a nudge at the wall stays inside the room", () => {
  expect(nudge("ArrowRight", { x: B.maxX, y: 150 }, B)).toEqual({ x: B.maxX, y: 150 });
  expect(nudge("ArrowUp", { x: 200, y: B.minY }, B)).toEqual({ x: 200, y: B.minY });
});
