// The house-model catalogue and the rule for showing its suggestion (src/houseModels.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  HOUSE_MODELS,
  approxSize,
  downloadable,
  houseModelSuggestion,
  parseHouseCatalogue,
  type ModelInventory,
} from "../src/houseModels.ts";

const none: ModelInventory = { hostedConfigured: 0, localFound: 0, defaultModel: null };

test("the shipped catalogue lists Emperor and Consul, neither downloadable yet", () => {
  assert.deepEqual(
    HOUSE_MODELS.map((model) => model.id),
    ["emperor", "consul"],
  );
  assert.ok(HOUSE_MODELS.every((model) => model.source === null && !downloadable(model)));
  assert.ok(HOUSE_MODELS.every((model) => model.role.length > 0 && model.approxBytes !== null));
});

test("parsing keeps good entries, drops bad ones and never invents a source", () => {
  const parsed = parseHouseCatalogue([
    { id: "a", name: "A", role: "r", approx_bytes: 2e9, source: { repo: "owner/a-GGUF", quant: " Q4_K_M " } },
    { id: "a", name: "Again", role: "r" },
    { id: "b", name: "B", role: "r", approx_bytes: -1, source: { repo: "not a repo" } },
    { id: "c", name: "C" },
    "nonsense",
    null,
    { id: "d", name: "D", role: "r", source: { repo: "owner/d" } },
  ]);
  assert.deepEqual(
    parsed.map((model) => model.id),
    ["a", "b", "d"],
  );
  assert.deepEqual(parsed[0].source, { repo: "owner/a-GGUF", quant: "Q4_K_M" });
  assert.equal(parsed[1].source, null);
  assert.equal(parsed[1].approxBytes, null);
  assert.deepEqual(parsed[2].source, { repo: "owner/d" });
  assert.ok(downloadable(parsed[0]) && !downloadable(parsed[1]));
});

test("a catalogue that is not an array is empty", () => {
  assert.deepEqual(parseHouseCatalogue({ id: "x" }), []);
  assert.deepEqual(parseHouseCatalogue(undefined), []);
});

test("the suggestion shows with no models, and only then", () => {
  assert.equal(houseModelSuggestion(none, false).length, 2);
  assert.deepEqual(houseModelSuggestion({ ...none, localFound: 1 }, false), []);
  assert.deepEqual(houseModelSuggestion({ ...none, hostedConfigured: 1 }, false), []);
  assert.deepEqual(houseModelSuggestion({ ...none, defaultModel: "llamacpp:x" }, false), []);
});

test("a dismissed suggestion stays gone, and an empty catalogue suggests nothing", () => {
  assert.deepEqual(houseModelSuggestion(none, true), []);
  assert.deepEqual(houseModelSuggestion(none, false, []), []);
});

test("sizes are approximate and may be unknown", () => {
  assert.equal(approxSize(2.7e9), "about 2.7 GB");
  assert.equal(approxSize(null), "size not known yet");
});
