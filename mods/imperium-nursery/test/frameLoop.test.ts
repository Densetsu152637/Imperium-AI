// The frame loop paints on every display frame, so a dragged or walking pet moves at the
// monitor's refresh rate; only the membership ticks are held to every 50 ms.

import { test } from "node:test";

import { FrameLoop } from "../src/view.ts";
import type { Room } from "../src/view.ts";
import { expect } from "./expect.ts";

const globals = globalThis as unknown as { requestAnimationFrame?: (cb: () => void) => number };

test("at 144 Hz every display frame paints, and membership ticks run at most every 50 ms", () => {
  const queued: (() => void)[] = [];
  const realRaf = globals.requestAnimationFrame;
  const realNow = Date.now;
  let clock = 1_000_000;
  globals.requestAnimationFrame = (cb) => queued.push(cb);
  Date.now = () => clock;
  try {
    const paints: number[] = [];
    let ticks = 0;
    const loop = new FrameLoop();
    loop.add({ paint: (now: number) => paints.push(now) } as unknown as Room);
    loop.onFrame(() => ticks++);
    loop.start();
    // 144 frames: one second at about 6.94 ms a frame.
    for (let i = 0; i < 144; i++) {
      clock += 1000 / 144;
      queued.shift()!();
    }
    expect(paints.length).toBe(144);
    // A tick needs 50 ms, so it lands every 8th frame (55.6 ms): 18 in a second, never more than 20.
    expect(ticks).toBeLessThanOrEqual(20);
    expect(ticks).toBeGreaterThanOrEqual(17);
  } finally {
    globals.requestAnimationFrame = realRaf;
    Date.now = realNow;
  }
});
