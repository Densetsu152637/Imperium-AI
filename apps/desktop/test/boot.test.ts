// The loading screen's removal logic and fallback (src/boot.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { afterPaint, BOOT_BACKGROUND, bootTheme, FADE_MS, FALLBACK_MS, startSplash, type SplashParts, type SplashTimers } from "../src/boot.ts";

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

test("afterPaint dismisses only on the second frame", () => {
  const frames: (() => void)[] = [];
  let dismissed = 0;
  afterPaint(
    () => void dismissed++,
    (run) => void frames.push(run),
  );
  assert.equal(frames.length, 1);
  frames.shift()?.();
  assert.equal(dismissed, 0);
  assert.equal(frames.length, 1);
  frames.shift()?.();
  assert.equal(dismissed, 1);
});

test("dismissing after the fallback has already fired is a no-op", () => {
  const { clock, parts, state, fire, timers } = fixture();
  const dismiss = startSplash(parts, clock);
  fire(FALLBACK_MS);
  dismiss();
  fire(FADE_MS);
  assert.equal(state.removed, 1);
  assert.equal(timers.filter((t) => t.ms === FADE_MS).length, 1);
});

test("the background and accent colours agree across the CSS, TypeScript, and Rust copies", () => {
  const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8").toLowerCase();
  const styles = read("../src/styles.css");
  const bootCss = read("../public/boot.css");
  const mainRs = read("../src-tauri/src/main.rs");
  const config = read("../src-tauri/tauri.conf.json");
  const dark = BOOT_BACKGROUND.dark;
  const light = BOOT_BACKGROUND.light;
  // styles.css: dark tokens in :root, light tokens under data-theme="light".
  assert.match(styles, new RegExp(`:root \\{[^}]*--bg: ${dark}`));
  assert.match(styles, new RegExp(`:root\\[data-theme="light"\\] \\{[^}]*--bg: ${light}`));
  const accentDark = /:root \{[^}]*--accent: (#[0-9a-f]{6})/.exec(styles)?.[1];
  const accentLight = /:root\[data-theme="light"\] \{[^}]*--accent: (#[0-9a-f]{6})/.exec(styles)?.[1];
  assert.ok(accentDark && accentLight);
  // boot.css: each pair appears in both of its theme rules.
  for (const value of [`--boot-bg: ${dark}`, `--boot-bg: ${light}`, `--boot-accent: ${accentDark}`, `--boot-accent: ${accentLight}`]) {
    assert.equal(bootCss.split(value).length - 1, 2, value);
  }
  // Native window backgrounds.
  assert.ok(config.includes(`"backgroundcolor": "${dark}"`));
  const bytes = (hex: string) => [1, 3, 5].map((i) => `0x${hex.slice(i, i + 2)}`).join(", ");
  assert.ok(mainRs.includes(`color(${bytes(dark)}, 0xff)`));
  assert.ok(mainRs.includes(`color(${bytes(light)}, 0xff)`));
});

test("the boot theme is the remembered one, else the system's", () => {
  assert.equal(bootTheme("light", false), "light");
  assert.equal(bootTheme("dark", true), "dark");
  assert.equal(bootTheme("system", true), "light");
  assert.equal(bootTheme(null, false), "dark");
});
