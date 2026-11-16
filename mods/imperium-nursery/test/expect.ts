// A tiny `expect` over node:assert, with the matchers the ported Pixel Pets tests use.

import assert from "node:assert/strict";

const subset = (actual: unknown, expected: unknown): boolean => {
  if (typeof expected !== "object" || expected === null) return Object.is(actual, expected);
  if (typeof actual !== "object" || actual === null) return false;
  return Object.entries(expected).every(([k, v]) => subset((actual as Record<string, unknown>)[k], v));
};

type Matchers = {
  toBe(expected: unknown): void;
  toEqual(expected: unknown): void;
  toMatchObject(expected: object): void;
  toContain(item: unknown): void;
  toContainEqual(item: unknown): void;
  toMatch(re: RegExp): void;
  toHaveLength(n: number): void;
  toBeNull(): void;
  toBeUndefined(): void;
  toBeGreaterThan(n: number): void;
  toBeGreaterThanOrEqual(n: number): void;
  toBeLessThan(n: number): void;
  toBeLessThanOrEqual(n: number): void;
};

const matchers = (actual: unknown, negate: boolean): Matchers => {
  const check = (pass: boolean, what: string): void => assert.ok(negate ? !pass : pass, `expected ${JSON.stringify(actual)} ${negate ? "not " : ""}${what}`);
  const num = (): number => actual as number;
  const includes = (item: unknown): boolean => (typeof actual === "string" ? actual.includes(item as string) : (actual as unknown[]).includes(item));
  return {
    toBe: (e) => check(Object.is(actual, e), `toBe ${JSON.stringify(e)}`),
    toEqual: (e) => {
      if (negate) assert.ok(JSON.stringify(actual) !== JSON.stringify(e));
      else assert.deepEqual(actual, e);
    },
    toMatchObject: (e) => check(subset(actual, e), `toMatchObject ${JSON.stringify(e)}`),
    toContainEqual: (i) => check((actual as unknown[]).some((x) => JSON.stringify(x) === JSON.stringify(i)), `toContainEqual ${JSON.stringify(i)}`),
    toContain: (i) => check(includes(i), `toContain ${JSON.stringify(i)}`),
    toMatch: (re) => check(re.test(actual as string), `toMatch ${re}`),
    toHaveLength: (n) => check((actual as { length: number }).length === n, `toHaveLength ${n}`),
    toBeNull: () => check(actual === null, "toBeNull"),
    toBeUndefined: () => check(actual === undefined, "toBeUndefined"),
    toBeGreaterThan: (n) => check(num() > n, `> ${n}`),
    toBeGreaterThanOrEqual: (n) => check(num() >= n, `>= ${n}`),
    toBeLessThan: (n) => check(num() < n, `< ${n}`),
    toBeLessThanOrEqual: (n) => check(num() <= n, `<= ${n}`),
  };
};

export const expect = (actual: unknown): Matchers & { not: Matchers } => ({ ...matchers(actual, false), not: matchers(actual, true) });
