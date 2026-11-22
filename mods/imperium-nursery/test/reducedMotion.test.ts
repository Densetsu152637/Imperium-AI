// The nursery stops its pets playing together when the person wants less motion: the operating
// system's preference, or the app's Reduce motion setting, which the host marks on the frame's
// root as the `reduce-motion` class (apps/desktop/src/mods/ModFrame.tsx).

import { test } from "node:test";

import { reducedMotion } from "../src/view.ts";
import { expect } from "./expect.ts";

const globals = globalThis as unknown as { document?: unknown; matchMedia?: unknown };

function withEnvironment(rootClasses: string[], osReduces: boolean, run: () => void): void {
  const saved = { document: globals.document, matchMedia: globals.matchMedia };
  globals.document = { documentElement: { classList: { contains: (name: string) => rootClasses.includes(name) } } };
  globals.matchMedia = () => ({ matches: osReduces });
  try {
    run();
  } finally {
    globals.document = saved.document;
    globals.matchMedia = saved.matchMedia;
  }
}

test("motion is reduced by the app setting alone", () => {
  withEnvironment(["reduce-motion"], false, () => expect(reducedMotion()).toBe(true));
});

test("motion is reduced by the operating system alone", () => {
  withEnvironment([], true, () => expect(reducedMotion()).toBe(true));
});

test("motion is not reduced when neither asks", () => {
  withEnvironment([], false, () => expect(reducedMotion()).toBe(false));
});
