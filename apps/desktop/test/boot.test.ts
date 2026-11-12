// The remembered theme: public/boot.js (a plain script linked from index.html, which sets the theme
// before the first paint) and src/boot.ts (which remembers it) must use the same storage key, or
// the first paint would ignore what the app saved.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { test } from "node:test";

import { THEME_KEY, bootTheme, rememberTheme } from "../src/boot.ts";

const script = readFileSync(new URL("../public/boot.js", import.meta.url), "utf8");

/** Runs boot.js as the page would, with a storage holding `stored`; returns the theme it set. */
function paintWith(stored: Record<string, string>): string | undefined {
  const dataset: Record<string, string> = {};
  runInNewContext(script, {
    localStorage: { getItem: (key: string) => stored[key] ?? null },
    document: { documentElement: { dataset } },
  });
  return dataset.theme;
}

test("boot.js reads the key that boot.ts writes", () => {
  const keys = [...script.matchAll(/getItem\(\s*"([^"]+)"\s*\)/g)].map((match) => match[1]);
  assert.deepEqual(keys, [THEME_KEY]);
  assert.equal(THEME_KEY, "imperium.theme");
});

test("the first paint follows what rememberTheme stored", () => {
  const stored: Record<string, string> = {};
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    value: { setItem: (key: string, value: string) => void (stored[key] = value) },
  });
  try {
    rememberTheme("dark");
    assert.equal(paintWith(stored), "dark");
    rememberTheme("light");
    assert.equal(paintWith(stored), "light");
    // "system" is remembered too, and leaves the choice to the operating system.
    rememberTheme("system");
    assert.equal(paintWith(stored), undefined);
  } finally {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else delete (globalThis as { localStorage?: unknown }).localStorage;
  }
});

test("a storage that throws leaves the first paint alone", () => {
  const dataset: Record<string, string> = {};
  runInNewContext(script, {
    localStorage: {
      getItem: () => {
        throw new Error("blocked");
      },
    },
    document: { documentElement: { dataset } },
  });
  assert.equal(dataset.theme, undefined);
});

test("the theme painted before the settings load is the remembered one, else the system's", () => {
  assert.equal(bootTheme("dark", true), "dark");
  assert.equal(bootTheme("light", false), "light");
  assert.equal(bootTheme("system", true), "light");
  assert.equal(bootTheme(null, false), "dark");
});
