// Frame timing, ported from Pixel Pets' render tests (the SVG fragment part is not used here).

import { test } from "node:test";

import { standardCatalog } from "../src/pet.ts";
import { fitSize, frameAt, frameStops, phaseDelay } from "../src/render.ts";
import { expect } from "./expect.ts";

const idle = standardCatalog().idle!;

test("keyframe percentages follow the per-frame durations", () => {
  const { total, stops } = frameStops(idle.durations);
  expect(total).toBe(1100);
  // Frames start at 0, 280, 390, 500, 640, 780 of 1100 ms.
  expect(stops).toEqual([0, 25.45, 35.45, 45.45, 58.18, 70.91]);
});

test("the playhead wraps for a loop and holds the end for a one-shot", () => {
  expect(phaseDelay(2500, 1100, true)).toBe(300);
  expect(phaseDelay(-5, 1100, true)).toBe(0);
  expect(phaseDelay(5000, 1000, false)).toBe(1000);
  expect(phaseDelay(5000, 0, true)).toBe(0);
});

test("frameAt picks the frame the playhead is in", () => {
  expect(frameAt(idle.durations, 0, true)).toBe(0);
  expect(frameAt(idle.durations, 279, true)).toBe(0);
  expect(frameAt(idle.durations, 280, true)).toBe(1);
  expect(frameAt(idle.durations, 1099, true)).toBe(5);
  // One cycle later it is in the same phase.
  expect(frameAt(idle.durations, 1100 + 300, true)).toBe(1);
  const j = standardCatalog().jumping!;
  expect(frameAt(j.durations, 99_999, false)).toBe(j.frames - 1);
  expect(frameAt([], 100, true)).toBe(0);
});

test("size fit keeps the aspect ratio inside size x size", () => {
  expect(fitSize({ width: 16, height: 16 }, 96)).toEqual({ width: 96, height: 96 });
  expect(fitSize({ width: 64, height: 32 }, 96)).toEqual({ width: 96, height: 48 });
  expect(fitSize({ width: 32, height: 64 }, 48)).toEqual({ width: 24, height: 48 });
});
