// Local models are off by default and stay listed as off; the Rescan button asks the daemon to
// scan again (src/models/localModels.ts, panels/LocalModelsPanel.tsx).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import {
  LOCAL_OFF_NOTE,
  downloadSummary,
  localEnabled,
  offersOf,
  offReason,
  rescanMessage,
  rowIsOff,
  runRescan,
  setLocalEnabled,
  setModelEnabled,
  sizeText,
  updateAvailable,
  type RescanState,
} from "../src/models/localModels.ts";
import { HOUSE_MODELS, houseModelSuggestion } from "../src/models/houseModels.ts";

const row = (id: string, ownEnabled: boolean) => ({ id, runtime: "llamacpp", own_enabled: ownEnabled, enabled: ownEnabled }) as never;
const listing = (localOn: boolean, models: unknown[]) => ({ local_enabled: localOn, models }) as never;

test("with local models off every model shows as off, whatever its own switch says", () => {
  const off = listing(false, [row("a", true), row("b", false)]);
  assert.equal(localEnabled(off), false);
  assert.ok(rowIsOff(off, row("a", true)));
  assert.match(offReason(off, row("a", true)) ?? "", /Local models are off/);
  const on = listing(true, []);
  assert.ok(!rowIsOff(on, row("a", true)));
  assert.equal(offReason(on, row("a", true)), null);
  assert.ok(rowIsOff(on, row("b", false)));
  assert.match(offReason(on, row("b", false)) ?? "", /Switched off/);
});

test("an old daemon that sends no switch counts as on, and a missing listing too", () => {
  assert.equal(localEnabled({ models: [] } as never), true);
  assert.equal(localEnabled(null), true);
  assert.match(LOCAL_OFF_NOTE, /off by default/);
});

test("the global switch writes models.local.enabled", async () => {
  const calls: [string, unknown][] = [];
  await setLocalEnabled(async (method, params) => (calls.push([method, params]), undefined as never), true);
  assert.deepEqual(calls, [["config.set", { key: "models.local.enabled", value: true }]]);
});

test("a model's own switch keeps the other switched-off models", async () => {
  const calls: [string, unknown][] = [];
  const call = async (method: string, params?: unknown) => {
    calls.push([method, params]);
    return (method === "models.config" ? { disabled: ["vllm:x", "llamacpp:a"] } : undefined) as never;
  };
  await setModelEnabled(call, "llamacpp:a", true);
  await setModelEnabled(call, "llamacpp:b", false);
  const saved = calls.filter(([method]) => method === "models.set_config").map(([, params]) => (params as { disabled: string[] }).disabled);
  assert.deepEqual(saved, [["vllm:x"], ["llamacpp:a", "llamacpp:b", "vllm:x"]]);
});

test("the scan result says how many models were found and what is new or gone", () => {
  assert.equal(rescanMessage({ found: 0, added: [], removed: [] }), "Found 0 models · nothing new.");
  assert.equal(rescanMessage({ found: 1, added: ["tiny"], removed: [] }), "Found 1 model · 1 new (tiny).");
  assert.equal(rescanMessage({ found: 3, added: ["a", "b"], removed: ["c"] }), "Found 3 models · 2 new (a, b) · 1 gone (c).");
});

test("Rescan calls models.rescan, shows a spinner state, then hands over the new listing", async () => {
  const calls: string[] = [];
  const states: RescanState[] = [];
  const applied: unknown[] = [];
  const fresh = listing(false, [row("new", true)]);
  await runRescan(
    async (method) => {
      calls.push(method);
      // The button is already in its scanning state while the daemon works.
      assert.equal(states.at(-1)?.kind, "scanning");
      return { listing: fresh, found: 1, added: ["new"], removed: [] } as never;
    },
    (state) => states.push(state),
    (next) => applied.push(next),
  );
  assert.deepEqual(calls, ["models.rescan"]);
  assert.deepEqual(applied, [fresh]);
  assert.deepEqual(
    states.map((state) => state.kind),
    ["scanning", "done"],
  );
  assert.equal(states[1].kind === "done" && states[1].message, "Found 1 model · 1 new (new).");
});

test("a failed scan shows the error instead of doing nothing", async () => {
  const states: RescanState[] = [];
  let applied = 0;
  await runRescan(
    async () => {
      throw new Error("no model folders");
    },
    (state) => states.push(state),
    () => applied++,
  );
  assert.equal(applied, 0);
  assert.deepEqual(states[1], { kind: "failed", message: "The rescan failed: no model folders" });
});

test("first run suggests no house model while local models are off", () => {
  const none = { hostedConfigured: 0, localFound: 0, defaultModel: null, loaded: true };
  assert.equal(houseModelSuggestion({ ...none, localEnabled: false }, false).length, 0);
  assert.equal(houseModelSuggestion({ ...none, localEnabled: true }, false).length, HOUSE_MODELS.length);
  assert.equal(houseModelSuggestion(none, false).length, HOUSE_MODELS.length);
});

test("the panel lists models with a switch and a rescan button wired to the daemon call", () => {
  const panel = readFileSync(new URL("../src/panels/LocalModelsPanel.tsx", import.meta.url), "utf8");
  assert.match(panel, /runRescan\(/, "the Rescan button runs the daemon scan");
  assert.doesNotMatch(panel, /<button className="ghost small" onClick=\{onRefresh\}>/, "Rescan is not a plain re-read any more");
  assert.match(panel, /<Switch\s+checked=\{row\.own_enabled\}/, "each model has an Enabled switch");
  assert.match(panel, /LOCAL_OFF_NOTE/, "the note says local models are off by default");
  assert.match(panel, /Spinner/, "a spinner shows while it scans");
});

const check = (over: Record<string, unknown>) =>
  ({
    enabled: true,
    checked_at: 1,
    latest: "b12900",
    current: "b12823",
    available: true,
    target: "managed",
    target_dir: "/data/runtimes/llama.cpp/b12900",
    pack: "linux-x64-cuda13",
    bytes: 190 * 1024 * 1024,
    source: "github.com/ggml-org/llama.cpp",
    message: null,
    ...over,
  }) as never;

test("an update is announced only when the last check found a newer build", () => {
  assert.equal(updateAvailable(check({})), "Update available: b12900 (you have b12823)");
  assert.equal(updateAvailable(check({ current: null })), "Update available: b12900");
  assert.equal(updateAvailable(check({ available: false })), null);
  assert.equal(updateAvailable(null), null);
  assert.equal(updateAvailable(undefined), null);
});

test("a download shows its source and size before it asks, and where it goes", () => {
  const managed = downloadSummary(check({}));
  assert.match(managed, /github\.com\/ggml-org\/llama\.cpp/);
  assert.match(managed, /190 MiB to download/);
  assert.match(managed, /SHA-256/);
  assert.match(managed, /kept for rollback/);
  const beside = downloadSummary(check({ target: "beside", target_dir: "D:/AI-Models/llama.cpp/b12900" }));
  assert.match(beside, /new folder beside yours \(D:\/AI-Models\/llama\.cpp\/b12900\)/);
  assert.match(beside, /your folder is left as it is/);
  // Before the release has been read the size is not made up.
  const unknown = downloadSummary(check({ bytes: null, target_dir: null }));
  assert.match(unknown, /size shown once the release has been read/);
  assert.match(downloadSummary(null), /github\.com\/ggml-org\/llama\.cpp/);
});

test("sizes and offers read from an older daemon that sends neither", () => {
  assert.equal(sizeText(null), null);
  assert.equal(sizeText(undefined), null);
  assert.equal(sizeText(1024 * 1024 * 3), "3 MiB");
  assert.deepEqual(offersOf(undefined), []);
  assert.deepEqual(offersOf({} as never), []);
  assert.equal(offersOf({ offers: [{ runtime: "llamacpp" }] } as never).length, 1);
});
