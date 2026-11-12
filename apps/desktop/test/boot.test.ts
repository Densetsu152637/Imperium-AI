// The loading screen's removal logic and fallback (src/boot.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { bootTheme, FADE_MS, FALLBACK_MS, startSplash, type SplashParts, type SplashTimers } from "../src/boot.ts";

function fixture() {
  const timers: { id: number; ms: number; run: () => void; live: boolean }[] = [];
  const clock: SplashTimers = {
    after(ms, run) {
      const timer = { id: timers.length, ms, run, live: true };
      timers.push(timer);
      return timer.id;
    },
    cancel(handle) {
      const timer = timers[handle as number];
      if (timer) timer.live = false;
    },
  };
  const state = { removed: 0, busy: true };
  const parts: SplashParts = {
    splash: { dataset: {}, remove: () => void state.removed++ },
    busy: { removeAttribute: () => void (state.busy = false) },
  };
  const fire = (ms: number) => {
    for (const timer of timers.filter((t) => t.live && t.ms === ms)) {
      timer.live = false;
      timer.run();
    }
  };
  return { clock, parts, state, fire, timers };
}

test("dismissing fades out, clears aria-busy, then removes the screen", () => {
  const { clock, parts, state, fire } = fixture();
  const dismiss = startSplash(parts, clock);
  dismiss();
  assert.equal(parts.splash?.dataset.state, "leaving");
  assert.equal(state.busy, false);
  assert.equal(state.removed, 0);
  fire(FADE_MS);
  assert.equal(state.removed, 1);
});

test("dismissing cancels the fallback and is idempotent", () => {
  const { clock, parts, state, fire, timers } = fixture();
  const dismiss = startSplash(parts, clock);
  dismiss();
  dismiss();
  fire(FALLBACK_MS);
  fire(FADE_MS);
  assert.equal(state.removed, 1);
  assert.equal(timers.filter((t) => t.ms === FADE_MS).length, 1);
});

test("the fallback removes the screen when the panels never report in", () => {
  const { clock, parts, state, fire } = fixture();
  startSplash(parts, clock);
  fire(FALLBACK_MS);
  assert.equal(parts.splash?.dataset.state, "leaving");
  assert.equal(state.busy, false);
  fire(FADE_MS);
  assert.equal(state.removed, 1);
});

test("a page without the loading screen still clears aria-busy", () => {
  const { clock, state } = fixture();
  startSplash({ splash: null, busy: { removeAttribute: () => void (state.busy = false) } }, clock)();
  assert.equal(state.busy, false);
});

test("the boot theme is the remembered one, else the system's", () => {
  assert.equal(bootTheme("light", false), "light");
  assert.equal(bootTheme("dark", true), "dark");
  assert.equal(bootTheme("system", true), "light");
  assert.equal(bootTheme(null, false), "dark");
});
