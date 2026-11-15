// "Use this model": which models are off and what the switches need (src/modelAvailability.ts).
// Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  defaultTurnedOff,
  disabledToSave,
  listedAfter,
  modelIsOff,
  NO_SWITCHES,
  offSpecs,
  replacementDefaults,
  type AvailabilityInput,
} from "../src/modelAvailability.ts";

const input: AvailabilityInput = {
  disabled: ["llamacpp:old"],
  local: ["llamacpp:old", "llamacpp:new", "vllm:big"],
  rows: [
    { id: "coder", spec: "cloud:coder-1", enabled: true },
    { id: "twin-a", spec: "cloud:twin", enabled: false },
    { id: "twin-b", spec: "cloud:twin", enabled: true },
    { id: "idle", spec: "cloud:idle", enabled: false },
  ],
  defaultModel: "llamacpp:new",
};

test("the listed models and the rows that are all off are off", () => {
  assert.deepEqual([...offSpecs(input)].sort(), ["cloud:idle", "llamacpp:old"]);
});

test("switches add and remove specs and never change the input", () => {
  assert.deepEqual(listedAfter(input.disabled, { local: { "llamacpp:new": false, "llamacpp:old": true }, rows: {} }), ["llamacpp:new"]);
  const off = offSpecs(input, { local: {}, rows: { "twin-b": false, idle: true } });
  assert.deepEqual([...off].sort(), ["cloud:twin", "llamacpp:old"]);
  assert.deepEqual(input.disabled, ["llamacpp:old"]);
});

test("only a changed list is sent, sorted", () => {
  assert.equal(disabledToSave(input.disabled, NO_SWITCHES), null);
  assert.equal(disabledToSave(input.disabled, { local: { "llamacpp:old": false }, rows: {} }), null, "off again is no change");
  assert.deepEqual(disabledToSave(input.disabled, { local: { "vllm:big": false, "llamacpp:new": false }, rows: {} }), [
    "llamacpp:new",
    "llamacpp:old",
    "vllm:big",
  ]);
  assert.deepEqual(disabledToSave(["a", "b"], { local: { a: true, b: true }, rows: {} }), []);
  // A row's switch is the row's own setting, not the list.
  assert.equal(disabledToSave(input.disabled, { local: {}, rows: { coder: false } }), null);
});

test("turning off the default is found, whichever switch does it", () => {
  assert.equal(defaultTurnedOff(input, NO_SWITCHES), null);
  assert.equal(defaultTurnedOff(input, { local: { "llamacpp:new": false }, rows: {} }), "llamacpp:new");
  assert.equal(defaultTurnedOff(input, { local: { "llamacpp:old": true }, rows: {} }), null);
  const hosted = { ...input, defaultModel: "cloud:coder-1" };
  assert.equal(defaultTurnedOff(hosted, { local: {}, rows: { coder: false } }), "cloud:coder-1");
  // A default that was already off is not a new refusal.
  assert.equal(defaultTurnedOff({ ...input, defaultModel: "llamacpp:old" }, NO_SWITCHES), null);
  assert.equal(defaultTurnedOff({ ...input, defaultModel: null }, { local: { "llamacpp:new": false }, rows: {} }), null);
});

test("the replacement defaults are the usable models that stay on", () => {
  const specs = [
    { spec: "llamacpp:new", usable: true },
    { spec: "cloud:coder-1", usable: true },
    { spec: "cloud:unset", usable: false },
    { spec: "vllm:big", usable: true },
    { spec: "llamacpp:old", usable: true },
  ];
  const off = offSpecs(input, { local: { "vllm:big": false }, rows: {} });
  assert.deepEqual(replacementDefaults(specs, off, "llamacpp:new"), ["cloud:coder-1"]);
});

test("a default named by its table id is found too", () => {
  const byId = { ...input, defaultModel: "coder" };
  assert.equal(defaultTurnedOff(byId, { local: {}, rows: { coder: false } }), "cloud:coder-1");
});

test("the lists the daemon sends say which model is off", () => {
  const rows = [
    { spec: "cloud:a", enabled: false },
    { spec: "cloud:twin", enabled: false },
    { spec: "cloud:twin", enabled: true },
  ];
  const local = [
    { runtime: "llamacpp", id: "old", enabled: false },
    { runtime: "llamacpp", id: "new", enabled: true },
  ];
  assert.equal(modelIsOff("cloud:a", rows, local), true);
  assert.equal(modelIsOff("cloud:twin", rows, local), false);
  assert.equal(modelIsOff("llamacpp:old", rows, local), true);
  assert.equal(modelIsOff("llamacpp:new", rows, local), false);
  assert.equal(modelIsOff("cloud:unknown", rows, local), false);
});
