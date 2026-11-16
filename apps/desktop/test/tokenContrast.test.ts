// Contrast guards for styles/tokens.css (docs/plans/ui-polish.md, guard test 5): for both themes and
// all twelve accents, text meets 4.5:1 on every surface and on the accent wash that draws selected
// rows, accent ink meets 4.5:1 on the accent, and the focus ring and control edges meet 3:1.
// Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

type Rgb = [number, number, number];
type Rule = { selectors: string[]; order: number; vars: Map<string, string> };

const css = readFileSync(new URL("../src/styles/tokens.css", import.meta.url), "utf8").replace(/\/\*[\s\S]*?\*\//g, "");

const rules: Rule[] = [...css.matchAll(/([^{}]+)\{([^{}]*)\}/g)].map((match, order) => ({
  selectors: match[1].split(",").map((selector) => selector.trim()),
  order,
  vars: new Map([...match[2].matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)].map((decl) => [decl[1], decl[2].trim()])),
}));

/** `:root`, `:root[data-theme="light"]`, `:root[data-accent="x"]`, or both attributes: matches a state? Its specificity is the attribute count. */
function specificity(selector: string, state: { theme: string; accent: string }): number | null {
  const attributes = [...selector.matchAll(/\[data-(theme|accent)="([\w-]+)"\]/g)];
  if (!selector.startsWith(":root") || selector.replace(/\[data-(theme|accent)="[\w-]+"\]/g, "") !== ":root") return null;
  for (const [, name, value] of attributes) if ((name === "theme" ? state.theme : state.accent) !== value) return null;
  return attributes.length;
}

/** The custom properties in force for a theme and accent, by specificity then source order. */
function resolve(theme: string, accent: string): Map<string, string> {
  const state = { theme, accent };
  const matched = rules
    .flatMap((rule) => {
      const found = rule.selectors.map((selector) => specificity(selector, state)).filter((value): value is number => value !== null);
      return found.length ? [{ rule, specificity: Math.max(...found) }] : [];
    })
    .sort((a, b) => a.specificity - b.specificity || a.rule.order - b.rule.order);
  const vars = new Map<string, string>();
  for (const { rule } of matched) for (const [name, value] of rule.vars) vars.set(name, value);
  return vars;
}

function colour(value: string | undefined, name: string): { rgb: Rgb; alpha: number } {
  assert.ok(value, `${name} is not defined`);
  const hex = /^#([0-9a-f]{6})$/i.exec(value);
  if (hex) {
    const n = Number.parseInt(hex[1], 16);
    return { rgb: [(n >> 16) & 255, (n >> 8) & 255, n & 255], alpha: 1 };
  }
  const fn = /^rgb\(\s*(\d+)\s+(\d+)\s+(\d+)(?:\s*\/\s*([\d.]+))?\s*\)$/.exec(value);
  assert.ok(fn, `${name}: cannot read ${value}`);
  return { rgb: [Number(fn[1]), Number(fn[2]), Number(fn[3])], alpha: fn[4] === undefined ? 1 : Number(fn[4]) };
}

const over = (top: { rgb: Rgb; alpha: number }, under: Rgb): Rgb => top.rgb.map((c, i) => c * top.alpha + under[i] * (1 - top.alpha)) as Rgb;
const channel = (c: number) => ((c /= 255) <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const luminance = ([r, g, b]: Rgb) => 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
function ratio(a: Rgb, b: Rgb): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

test("the contrast helpers match known ratios", () => {
  assert.equal(ratio([255, 255, 255], [0, 0, 0]).toFixed(1), "21.0");
  assert.equal(ratio([119, 119, 119], [255, 255, 255]).toFixed(2), "4.48");
});

/** The twelve accents of settings.ts, each defined for both themes in tokens.css. */
const ACCENTS = ["cyan", "teal", "green", "lime", "amber", "orange", "rose", "magenta", "violet", "indigo", "blue", "silver"];

test("tokens.css defines exactly the twelve accents, each for both themes", () => {
  const dark = new Set([...css.matchAll(/(?<!\])\[data-accent="([\w-]+)"\]\s*[{,]/g)].map((match) => match[1]));
  assert.deepEqual([...dark].sort(), [...ACCENTS].sort());
  for (const accent of ACCENTS) assert.ok(css.includes(`:root[data-theme="light"][data-accent="${accent}"]`), accent);
});

const SURFACES = ["--bg", "--surface", "--surface-raised", "--surface-sunken"];

for (const theme of ["dark", "light"]) {
  for (const accent of ACCENTS) {
    test(`${theme} theme, ${accent} accent: text, accent text, ink, focus and control edges are readable`, () => {
      const vars = resolve(theme, accent);
      const get = (name: string) => colour(vars.get(name), `${theme}/${accent} ${name}`);
      const surfaces = SURFACES.map((name) => ({ name, rgb: get(name).rgb }));
      const wash = get("--accent-soft");
      const failures: string[] = [];
      const need = (label: string, actual: number, minimum: number) => {
        if (actual < minimum) failures.push(`${label} is ${actual.toFixed(2)}:1, under ${minimum}:1`);
      };
      for (const surface of surfaces) {
        for (const text of ["--text", "--text-muted", "--text-subtle"]) {
          need(`${text} on ${surface.name}`, ratio(get(text).rgb, surface.rgb), 4.5);
          need(`${text} on the accent wash over ${surface.name}`, ratio(get(text).rgb, over(wash, surface.rgb)), 4.5);
        }
        need(`--accent-text on the accent wash over ${surface.name}`, ratio(get("--accent-text").rgb, over(wash, surface.rgb)), 4.5);
        need(`--focus on ${surface.name}`, ratio(get("--focus").rgb, surface.rgb), 3);
        need(`--border-control on ${surface.name}`, ratio(get("--border-control").rgb, surface.rgb), 3);
      }
      need("--accent-ink on --accent", ratio(get("--accent-ink").rgb, get("--accent").rgb), 4.5);
      need("--accent-text on --bg", ratio(get("--accent-text").rgb, get("--bg").rgb), 4.5);
      assert.deepEqual(failures, []);
    });
  }
}
