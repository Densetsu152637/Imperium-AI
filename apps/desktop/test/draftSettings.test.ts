// The draft model fields of the model settings form (src/draftSettings.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { checkDraft } from "../src/draftSettings.ts";

const none = { model: null, max: null, min: null, gpuLayers: null };

test("no draft needs no draft settings", () => {
  assert.doesNotThrow(() => checkDraft("main", none));
  assert.throws(() => checkDraft("main", { ...none, max: 4 }), /need a draft model/);
  assert.throws(() => checkDraft("main", { ...none, min: 0 }), /need a draft model/);
  assert.throws(() => checkDraft("main", { ...none, gpuLayers: 0 }), /need a draft model/);
});

test("a model cannot draft for itself", () => {
  assert.throws(() => checkDraft("main", { ...none, model: "main" }), /itself/);
  assert.doesNotThrow(() => checkDraft("main", { ...none, model: "small" }));
});

test("the minimum may not exceed the maximum, or the default maximum when none is set", () => {
  assert.throws(() => checkDraft("main", { model: "small", max: 2, min: 3, gpuLayers: null }), /may not exceed/);
  assert.doesNotThrow(() => checkDraft("main", { model: "small", max: 8, min: 8, gpuLayers: null }));
  assert.throws(() => checkDraft("main", { ...none, model: "small", min: 4 }), /the default/);
  assert.doesNotThrow(() => checkDraft("main", { ...none, model: "small", min: 3 }));
  assert.doesNotThrow(() => checkDraft("main", { ...none, model: "small", min: 0 }));
  // A maximum in effect from elsewhere replaces the default.
  assert.doesNotThrow(() => checkDraft("main", { ...none, model: "small", min: 6 }, 8));
});
