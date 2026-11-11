// Relative times in session rows (src/ago.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { formatAgo } from "../src/ago.ts";

const now = Date.UTC(2026, 10, 11, 12, 0, 0);

test("recent times are short", () => {
  assert.equal(formatAgo(now - 20_000, now), "now");
  assert.equal(formatAgo(now - 5 * 60_000, now), "5m");
  assert.equal(formatAgo(now - 3 * 3_600_000, now), "3h");
  assert.equal(formatAgo(now - 2 * 86_400_000, now), "2d");
  assert.equal(formatAgo(now - 15 * 86_400_000, now), "2w");
});

test("older times are dates, and non-times are nothing", () => {
  assert.notEqual(formatAgo(now - 60 * 86_400_000, now), "");
  assert.equal(formatAgo(null, now), "");
  assert.equal(formatAgo(3, now), "");
});

test("a time slightly in the future is now", () => {
  assert.equal(formatAgo(now + 5_000, now), "now");
});
