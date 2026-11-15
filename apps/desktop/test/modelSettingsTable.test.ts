// The declarative settings table of the model configuration dialog (src/modelSettingsTable.ts).
// Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { EMPTY_LOCAL, EMPTY_TABLE } from "../src/modelConfigDrafts.ts";
import {
  affectsText,
  ESTIMATE_ROWS,
  groupedSettings,
  LOCAL_GROUPS,
  LOCAL_SETTINGS,
  settingFor,
  TABLE_GROUPS,
  TABLE_SETTINGS,
} from "../src/modelSettingsTable.ts";

test("every field of the local form has one setting with an explanation, and the on/off switch", () => {
  const keys = LOCAL_SETTINGS.map((setting) => setting.key).sort();
  assert.deepEqual(keys, [...Object.keys(EMPTY_LOCAL), "enabled"].sort());
  assert.equal(new Set(keys).size, keys.length, "no setting twice");
});

test("every field of the table-row form has one setting", () => {
  const keys = TABLE_SETTINGS.map((setting) => setting.key).sort();
  assert.deepEqual(keys, Object.keys(EMPTY_TABLE).sort());
});

test("each setting says what it affects and explains itself in a sentence or more", () => {
  for (const setting of [...LOCAL_SETTINGS, ...TABLE_SETTINGS]) {
    assert.ok(setting.label.length > 0, setting.key);
    assert.ok(setting.affects.length > 0, `${setting.key} affects nothing`);
    assert.ok(setting.help.length >= 40, `${setting.key}: ${setting.help}`);
    assert.ok(/[.!]$/.test(setting.help), `${setting.key} help ends a sentence`);
  }
  for (const row of ESTIMATE_ROWS) {
    assert.ok(row.help.length >= 40 && row.label.length > 0, row.key);
  }
});

test("every setting is in a group that exists, and the groups come out in order", () => {
  for (const [groups, settings] of [
    [LOCAL_GROUPS, LOCAL_SETTINGS],
    [TABLE_GROUPS, TABLE_SETTINGS],
  ] as const) {
    const ids = new Set(groups.map((group) => group.id));
    for (const setting of settings) assert.ok(ids.has(setting.group), `${setting.key} is in ${setting.group}`);
    const grouped = groupedSettings(groups, settings);
    assert.equal(grouped.flatMap((entry) => entry.settings).length, settings.length);
    assert.deepEqual(
      grouped.map((entry) => entry.group.id),
      groups.map((group) => group.id).filter((id) => settings.some((setting) => setting.group === id)),
    );
  }
});

test("the memory settings are in one group, and the switch comes first among the identity rows", () => {
  const memory = groupedSettings(LOCAL_GROUPS, LOCAL_SETTINGS).find((entry) => entry.group.id === "memory");
  assert.deepEqual(
    memory?.settings.map((setting) => setting.key),
    ["context", "pool_tokens", "slots", "cache_type_k", "cache_type_v", "flash_attn", "max_gpu_layers"],
  );
  const identity = groupedSettings(LOCAL_GROUPS, LOCAL_SETTINGS)[0];
  assert.equal(identity.settings[0].key, "enabled");
  assert.equal(groupedSettings(TABLE_GROUPS, TABLE_SETTINGS)[0].settings[0].key, "enabled");
});

test("llama.cpp-only settings are marked so a vLLM model can show them as unavailable", () => {
  for (const key of ["cache_type_k", "cache_type_v", "flash_attn", "max_gpu_layers", "draft_model"] as const) {
    assert.equal(settingFor(LOCAL_SETTINGS, key).llamacppOnly, true, key);
  }
  assert.equal(settingFor(LOCAL_SETTINGS, "context").llamacppOnly, undefined);
  assert.throws(() => settingFor(LOCAL_SETTINGS, "nope" as never), /no setting nope/);
});

test("what a setting affects reads as a sentence", () => {
  assert.equal(affectsText(["memory"]), "Affects memory.");
  assert.equal(affectsText(["memory", "quality"]), "Affects memory and quality.");
  assert.equal(affectsText(["memory", "speed", "quality"]), "Affects memory, speed and quality.");
  assert.equal(affectsText([]), "");
});
