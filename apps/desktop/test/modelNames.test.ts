// Model display aliases (src/modelNames.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalModelRow, ModelRow } from "@imperium/protocol";

import { aliasMap, modelAliases, modelName, modelTitle, setModelAliases } from "../src/modelNames.ts";

const row = (id: string, spec: string, alias: string | null = null) => ({ id, spec, alias }) as ModelRow;
const local = (id: string, alias: string | null = null, runtime: "llamacpp" | "vllm" = "llamacpp") =>
  ({ id, runtime, alias }) as LocalModelRow;

const qwen = "Qwen3-4B-Instruct-2507-Q4_K_M";

test("a local model's alias names it by id and by spec", () => {
  const aliases = aliasMap([], [local(qwen, "Qwen 3 (4B)"), local("gemma")]);
  assert.equal(modelName(aliases, qwen), "Qwen 3 (4B)");
  assert.equal(modelName(aliases, `llamacpp:${qwen}`), "Qwen 3 (4B)");
  // No alias: the name as given.
  assert.equal(modelName(aliases, "llamacpp:gemma"), "llamacpp:gemma");
  assert.equal(modelTitle(aliases, "llamacpp:gemma"), undefined);
  // The real name stays the hover text.
  assert.equal(modelTitle(aliases, `llamacpp:${qwen}`), `llamacpp:${qwen}`);
});

test("a table row's alias wins, and a row without one takes its local model's", () => {
  const aliases = aliasMap(
    [row("gpt", "openai:gpt-5", "GPT-5"), row("qwen", `llamacpp:${qwen}`), row("plain", "openai:mini")],
    [local(qwen, "Qwen 3 (4B)")],
  );
  assert.equal(modelName(aliases, "gpt"), "GPT-5");
  assert.equal(modelName(aliases, "openai:gpt-5"), "GPT-5");
  assert.equal(modelName(aliases, "qwen"), "Qwen 3 (4B)");
  assert.equal(modelName(aliases, "plain"), "plain");
  const renamed = aliasMap([row("qwen", `llamacpp:${qwen}`, "Small Qwen")], [local(qwen, "Qwen 3 (4B)")]);
  assert.equal(modelName(renamed, "qwen"), "Small Qwen");
  assert.equal(modelName(renamed, `llamacpp:${qwen}`), "Small Qwen");
});

test("published aliases replace the previous ones", () => {
  setModelAliases([row("gpt", "openai:gpt-5", "GPT-5")], []);
  assert.equal(modelName(modelAliases(), "gpt"), "GPT-5");
  setModelAliases([], []);
  assert.equal(modelName(modelAliases(), "gpt"), "gpt");
});
