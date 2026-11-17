// The model configuration form's drafts and their validation (src/models/modelConfigDrafts.ts). Run
// with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalModelConfig } from "@imperium/protocol";

import { aliasValue, cardValue, EMPTY_LOCAL, EMPTY_TABLE, localDraft, localSettings, tableDraft, tableSettings } from "../src/models/modelConfigDrafts.ts";

const effective = {
  context: 8192,
  context_auto: false,
  pool_tokens: 8192,
  pool_max: 32768,
  pool_fixed: false,
  pool_clamped: false,
  slots: 4,
  cache_type_k: "f16",
  cache_type_v: "f16",
  flash_attn: "auto",
  max_gpu_layers: null,
  max_wait_s: 60,
  max_output_tokens: 0,
  extra_args: [],
  draft_model: null,
  draft_max: null,
  draft_min: null,
  draft_gpu_layers: null,
};
const llama = { id: "qwen", runtime: "llamacpp", effective } as unknown as LocalModelConfig;
const vllm = { id: "big", runtime: "vllm", effective } as unknown as LocalModelConfig;

test("names and cards are trimmed, empty means not set, and long ones are refused", () => {
  assert.equal(aliasValue("  My   model "), "My model");
  assert.equal(aliasValue("   "), null);
  assert.throws(() => aliasValue("x".repeat(81)), /at most 80/);
  assert.equal(cardValue(" Good at: code. "), "Good at: code.");
  assert.equal(cardValue(""), null);
  assert.throws(() => cardValue("x".repeat(4001)), /at most 4000/);
});

test("an empty local form saves nothing set; typed numbers are checked", () => {
  const empty = localSettings(EMPTY_LOCAL, llama);
  assert.equal(empty.slots, null);
  assert.equal(empty.context_auto, false);
  assert.deepEqual(empty.extra_args, null);
  const typed = localSettings({ ...EMPTY_LOCAL, context: "auto", slots: "8", extra_args: " --foo \n\n--bar " }, llama);
  assert.equal(typed.context_auto, true);
  assert.equal(typed.context, null);
  assert.equal(typed.slots, 8);
  assert.deepEqual(typed.extra_args, ["--foo", "--bar"]);
  assert.throws(() => localSettings({ ...EMPTY_LOCAL, slots: "300" }, llama), /at most 256/);
  assert.throws(() => localSettings({ ...EMPTY_LOCAL, slots: "x" }, llama), /whole number/);
  assert.throws(() => localSettings({ ...EMPTY_LOCAL, pool_tokens: "100", context: "200" }, llama), /pool must be at least/);
});

test("llama.cpp-only fields are dropped for a vLLM model, and a quantised V cache needs flash attention", () => {
  const settings = localSettings({ ...EMPTY_LOCAL, cache_type_k: "q8_0", max_gpu_layers: "20", draft_model: "small" }, vllm);
  assert.equal(settings.cache_type_k, null);
  assert.equal(settings.max_gpu_layers, null);
  assert.equal(settings.draft_model, null);
  assert.throws(() => localSettings({ ...EMPTY_LOCAL, cache_type_v: "q8_0", flash_attn: "off" }, llama), /needs flash attention/);
});

test("a table row's prices come together, and the switch is kept as set", () => {
  assert.throws(() => tableSettings({ ...EMPTY_TABLE, input_usd_per_mtok: "1" }), /both prices or neither/);
  const row = tableSettings({ ...EMPTY_TABLE, input_usd_per_mtok: "1", output_usd_per_mtok: "2.5", tags: "vision, coder ,", enabled: false });
  assert.equal(row.enabled, false);
  assert.deepEqual(row.tags, ["vision", "coder"]);
  assert.equal(row.output_usd_per_mtok, 2.5);
  assert.equal(tableSettings(EMPTY_TABLE).enabled, null);
});

test("drafts round-trip saved settings", () => {
  const saved = localSettings({ ...EMPTY_LOCAL, slots: "2", alias: "Q" }, llama);
  assert.equal(localDraft(saved).slots, "2");
  assert.equal(localDraft(saved).alias, "Q");
  const row = tableSettings({ ...EMPTY_TABLE, tier: "strong" });
  assert.equal(tableDraft(row).tier, "strong");
});
