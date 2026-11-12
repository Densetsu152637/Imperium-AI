// The local model benchmark helpers (src/bench.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { BenchResult, BenchRun } from "@imperium/protocol";

import { activeRun, gibibytes, history, latestHere, mergeRun, millis, seconds, tokensPerSecond } from "../src/bench.ts";

const result = (model_id: string, hardware_fingerprint: string, extra: Partial<BenchResult> = {}): BenchResult => ({
  model_id,
  run_id: "bench-1",
  date: "2026-11-12T00:00:00Z",
  hardware_fingerprint,
  hardware: "",
  runtime: "llamacpp",
  placement: "gpu",
  machine: "quiet",
  load_s: 1,
  ttft_ms: 200,
  prompt_tok_s: 500,
  gen_tok_s: 20,
  peak_ram_bytes: null,
  peak_vram_bytes: null,
  prompts: [],
  error: null,
  ...extra,
});

const run = (run_id: string, state: string): BenchRun => ({
  run_id,
  state,
  models: [],
  current: null,
  step: null,
  models_done: 0,
  results: [],
  message: null,
});

test("progress replaces the run it belongs to and appends a new one", () => {
  const runs = mergeRun([run("bench-1", "running")], run("bench-1", "done"));
  assert.deepEqual(
    runs.map((r) => r.state),
    ["done"],
  );
  assert.equal(mergeRun(runs, run("bench-2", "running")).length, 2);
  assert.equal(activeRun(mergeRun(runs, run("bench-2", "running")))?.run_id, "bench-2");
  assert.equal(activeRun(runs), undefined);
});

test("the table shows each model's newest result on this machine only", () => {
  const newest = result("a", "here", { run_id: "bench-3" });
  const results = [newest, result("b", "other"), result("a", "here"), result("c", "here", { error: "x" })];
  const here = latestHere(results, ["here", "here-vllm"]);
  assert.deepEqual(
    here.map((r) => r.model_id),
    ["a"],
  );
  assert.equal(here[0], newest);
  // Everything else that measured something is history.
  assert.deepEqual(
    history(results, here).map((r) => r.model_id),
    ["b", "a"],
  );
});

test("numbers that were not measured show a dash", () => {
  assert.equal(seconds(null), "–");
  assert.equal(seconds(4.5), "4.5 s");
  assert.equal(millis(212.4), "212 ms");
  assert.equal(tokensPerSecond(19.55, 1), "19.6 tok/s");
  assert.equal(tokensPerSecond(undefined), "–");
  assert.equal(gibibytes(3 * 1024 ** 3), "3.0 GiB");
  assert.equal(gibibytes(null), "–");
});
