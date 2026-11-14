// Whether the effort control is live for a session's model (src/effort.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { effortToSend, NO_REASONING_HINT, reasoningAvailable } from "../src/effort.ts";

test("a model that reasons enables the effort control", () => {
  assert.equal(reasoningAvailable({ model: "deepseek", reasoning: true }), true);
});

test("a model that does not reason, or no model yet, disables it", () => {
  assert.equal(reasoningAvailable({ model: "plain", reasoning: false }), false);
  assert.equal(reasoningAvailable({ model: null, reasoning: true }), false);
});

test("an effort is never sent for a model that cannot reason", () => {
  assert.equal(effortToSend(false, "medium", "high"), null);
  assert.equal(effortToSend(true, "medium", "high"), "high");
  assert.equal(effortToSend(true, "high", "high"), null);
  assert.equal(effortToSend(true, undefined, "medium"), "medium");
});

test("the tooltip says why the control is greyed out", () => {
  assert.equal(NO_REASONING_HINT, "This model doesn't reason");
});
