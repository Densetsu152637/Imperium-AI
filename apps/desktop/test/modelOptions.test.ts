// The session's model in the list when it was switched off after the session started
// (src/modelOptions.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalModelRow, ModelRow } from "@imperium/protocol";

import { modelOptions } from "../src/modelOptions.ts";

const row = (id: string, spec: string, enabled: boolean, alias: string | null = null) =>
  ({ id, spec, alias, enabled, endpoint_configured: true, location: "cloud", tier: "standard" }) as ModelRow;
const local = (id: string, enabled: boolean, alias: string | null = null) =>
  ({ id, runtime: "llamacpp", alias, enabled, problem: null }) as LocalModelRow;

test("a model switched off is left out of the list, but the session's own stays, marked off", () => {
  const models = [row("coder", "cloud:coder-1", true), row("old", "cloud:old-1", false, "Old one")];
  const without = modelOptions(models, [], null);
  assert.deepEqual(
    without.map((option) => option.value),
    ["cloud:coder-1"],
  );
  const options = modelOptions(models, [], "cloud:old-1", undefined, true);
  assert.equal(options[0].value, "cloud:old-1");
  assert.equal(options[0].label, "Old one (off)");
  assert.equal(options[0].dimmed, true);
  assert.equal(options.length, 2);
});

test("a local model switched off shows by its alias or id with off", () => {
  const options = modelOptions([], [local("gemma", false, "Gemma")], "llamacpp:gemma", undefined, true);
  assert.equal(options.length, 1);
  assert.equal(options[0].label, "Gemma (off)");
  const plain = modelOptions([], [local("qwen", false)], "llamacpp:qwen", undefined, true);
  assert.equal(plain[0].label, "qwen (off)");
});

test("a model that is on, or one the settings do not know, is not marked off", () => {
  const models = [row("coder", "cloud:coder-1", true)];
  const on = modelOptions(models, [], "cloud:coder-1");
  assert.equal(on[0].label, "coder");
  const unknown = modelOptions(models, [], "cloud:mystery");
  assert.equal(unknown[0].label, "cloud:mystery");
  assert.notEqual(unknown[0].dimmed, true);
});

test("a new-session picker does not offer a model that is off, even as its chosen one", () => {
  const models = [row("coder", "cloud:coder-1", true), row("old", "cloud:old-1", false)];
  const options = modelOptions(models, [], "cloud:old-1");
  assert.deepEqual(
    options.map((option) => option.value),
    ["cloud:coder-1"],
  );
  assert.deepEqual(modelOptions([], [local("gemma", false)], "llamacpp:gemma").length, 0);
});

test("the session's model is found by its table id too", () => {
  const models = [row("old", "cloud:old-1", false, "Old one")];
  const options = modelOptions(models, [], "old", undefined, true);
  assert.equal(options[0].value, "old");
  assert.equal(options[0].label, "Old one (off)");
  assert.equal(modelOptions(models, [], "old").length, 0);
});
