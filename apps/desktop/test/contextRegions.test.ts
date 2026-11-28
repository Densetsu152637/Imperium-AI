// The context bar's three regions: used, pre-allocated buffer, unallocated (src/sessions/contextRegions.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { contextRegions } from "../src/sessions/contextRegions.ts";

/** The invariant every answer keeps while the request fits. */
function ordered(r: ReturnType<typeof contextRegions>) {
  assert.ok(r.used <= r.allocated && r.allocated <= r.max, JSON.stringify(r));
  assert.equal(r.used + r.buffer + r.unallocated, r.max);
  assert.equal(r.allocated, r.used + r.buffer);
}

test("a hosted model with no buffer has everything past the request unallocated", () => {
  const r = contextRegions({ used: 1000, configuredWindow: 8000 });
  ordered(r);
  assert.deepEqual([r.used, r.buffer, r.allocated, r.unallocated, r.max], [1000, 0, 1000, 7000, 8000]);
  assert.equal(r.maxSource, "configured");
});

test("reserved reply room is the buffer of a hosted model", () => {
  const r = contextRegions({ used: 1000, configuredWindow: 8000, reservedOutput: 2000 });
  ordered(r);
  assert.deepEqual([r.buffer, r.allocated, r.unallocated], [2000, 3000, 5000]);
});

test("a local server's allocated context beyond use is the buffer, reply room included", () => {
  const r = contextRegions({
    used: 1000,
    configuredWindow: 32768,
    contextWindow: 8192,
    reservedOutput: 2000,
    allocatedContext: 8192,
  });
  ordered(r);
  assert.deepEqual([r.buffer, r.allocated, r.unallocated, r.max], [7192, 8192, 24576, 32768]);
});

test("the larger of the server's allocation and the reply room wins", () => {
  const r = contextRegions({ used: 6000, configuredWindow: 32768, reservedOutput: 4000, allocatedContext: 8192 });
  ordered(r);
  assert.equal(r.allocated, 10000);
  assert.equal(r.buffer, 4000);
});

test("an allocation past the configured maximum is clamped to it", () => {
  const r = contextRegions({ used: 1000, configuredWindow: 4096, allocatedContext: 16384, reservedOutput: 9000 });
  ordered(r);
  assert.deepEqual([r.allocated, r.unallocated, r.max], [4096, 0, 4096]);
});

test("a request over the maximum fills the bar and reports the overflow", () => {
  const r = contextRegions({ used: 5000, configuredWindow: 4096, reservedOutput: 500 });
  ordered(r);
  assert.deepEqual([r.used, r.overflow, r.buffer, r.unallocated], [4096, 904, 0, 0]);
});

test("without a configured maximum the model's context window stands in, and says so", () => {
  const r = contextRegions({ used: 100, contextWindow: 2000 });
  ordered(r);
  assert.equal(r.max, 2000);
  assert.equal(r.maxSource, "window");
});

test("the configured maximum wins over the model's context window", () => {
  const r = contextRegions({ used: 100, configuredWindow: 4000, contextWindow: 2000 });
  assert.equal(r.max, 4000);
  assert.equal(r.maxSource, "configured");
});

test("with no maximum at all the bar spans what is allocated and nothing is unallocated", () => {
  const r = contextRegions({ used: 300, reservedOutput: 200 });
  assert.deepEqual([r.used, r.buffer, r.allocated, r.unallocated, r.max], [300, 200, 500, 0, 500]);
  assert.equal(r.maxSource, "unknown");
  const bare = contextRegions({ used: 300 });
  assert.deepEqual([bare.buffer, bare.max, bare.maxSource], [0, 300, "unknown"]);
});

test("zero, negative and missing values are ignored rather than trusted", () => {
  const r = contextRegions({
    used: -5,
    configuredWindow: 0,
    contextWindow: null,
    reservedOutput: -1,
    allocatedContext: Number.NaN,
  });
  assert.deepEqual([r.used, r.buffer, r.allocated, r.max, r.maxSource], [0, 0, 0, 0, "unknown"]);
});
