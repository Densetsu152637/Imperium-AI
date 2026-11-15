// The memory estimate shown in the model configuration dialog (src/models/modelMemory.ts). Run with
// `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { LocalModelConfig } from "@imperium/protocol";

import { approx, estimateValue, formatBytes, localEstimate, placementText, rowEstimate, summaryLine } from "../src/models/modelMemory.ts";

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;

/** A local model as `models.config` reports it, with the fields the estimate reads. */
function model(over: Partial<LocalModelConfig> = {}): LocalModelConfig {
  return {
    id: "qwen",
    runtime: "llamacpp",
    required_bytes: 6 * GIB,
    kv_pool_bytes: 1 * GIB,
    weights_bytes: 4.5 * GIB,
    placement: "gpu",
    layers: 36,
    gpu_layers: 36,
    vram_bytes: 6 * GIB,
    ram_bytes: 0,
    ...over,
  } as LocalModelConfig;
}

test("bytes read in GiB, or MiB when small", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(Number.NaN), "0 B");
  assert.equal(formatBytes(5.34 * GIB), "5.3 GiB");
  assert.equal(formatBytes(640 * MIB), "0.6 GiB");
  assert.equal(formatBytes(80 * MIB), "80 MiB");
  assert.equal(formatBytes(1), "1 MiB");
  assert.equal(approx(2 * GIB), "≈ 2.0 GiB");
});

test("a local model's estimate carries the planner's split and what is left after weights and KV", () => {
  const estimate = localEstimate(model());
  assert.equal(estimate.kind, "local");
  if (estimate.kind !== "local") return;
  assert.equal(estimate.vramBytes, 6 * GIB);
  assert.equal(estimate.ramBytes, 0);
  assert.equal(estimate.otherBytes, 0.5 * GIB);
  assert.ok(estimate.fits);
  assert.equal(summaryLine(estimate), "≈ 6.0 GiB VRAM");
  assert.equal(estimateValue(estimate, "ram"), "none");
  assert.equal(estimateValue(estimate, "vram"), "≈ 6.0 GiB");
  assert.equal(estimateValue(estimate, "kv"), "≈ 1.0 GiB");
  assert.equal(estimateValue(estimate, "weights"), "≈ 4.5 GiB");
  assert.equal(estimateValue(estimate, "total"), "≈ 6.0 GiB");
});

test("a split model shows both memories and how many layers are on the GPU", () => {
  const estimate = localEstimate(model({ placement: "partial", gpu_layers: 22, vram_bytes: 4 * GIB, ram_bytes: 2.5 * GIB }));
  assert.equal(summaryLine(estimate), "≈ 4.0 GiB VRAM + ≈ 2.5 GiB RAM");
  if (estimate.kind !== "local") return assert.fail();
  assert.equal(placementText(estimate), "22 of 36 layers on the GPU, the rest on the CPU (slower)");
});

test("the CPU lane and a model too large for the machine say so", () => {
  const cpu = localEstimate(model({ placement: "cpu", gpu_layers: 0, vram_bytes: 0, ram_bytes: 6 * GIB }));
  assert.equal(summaryLine(cpu), "≈ 6.0 GiB RAM");
  if (cpu.kind !== "local") return assert.fail();
  assert.match(placementText(cpu), /CPU only/);
  const big = localEstimate(model({ placement: "none", gpu_layers: null, vram_bytes: 0, ram_bytes: 0, required_bytes: 40 * GIB }));
  assert.equal(summaryLine(big), "too large to load now (needs ≈ 40.0 GiB)");
  assert.equal(estimateValue(big, "vram"), "does not fit");
  assert.equal(estimateValue(big, "ram"), "does not fit");
  if (big.kind !== "local") return assert.fail();
  assert.match(placementText(big), /Too large/);
});

test("a model without a layer count is still described", () => {
  const unknown = localEstimate(model({ layers: null, gpu_layers: null }));
  if (unknown.kind !== "local") return assert.fail();
  assert.equal(placementText(unknown), "All layers on the GPU");
  const partial = localEstimate(model({ placement: "partial", layers: null, gpu_layers: null }));
  if (partial.kind !== "local") return assert.fail();
  assert.equal(placementText(partial), "Some layers on the GPU, the rest on the CPU (slower)");
});

test("a hosted row runs remotely; a row that names a local model shows that model's memory", () => {
  const remote = rowEstimate({ spec: "openai:gpt", location: "cloud" }, [model()]);
  assert.deepEqual(remote, { kind: "remote" });
  assert.equal(summaryLine(remote), "runs remotely");
  assert.equal(estimateValue(remote, "vram"), "runs remotely");
  const pinned = rowEstimate({ spec: "llamacpp:qwen", location: "local" }, [model()]);
  assert.equal(pinned.kind, "local");
  assert.equal(summaryLine(pinned), "≈ 6.0 GiB VRAM");
  // A local row whose model was not found has nothing to estimate here.
  assert.deepEqual(rowEstimate({ spec: "llamacpp:gone", location: "local" }, [model()]), { kind: "remote" });
});

test("weights of an unknown size are not shown as zero", () => {
  const estimate = localEstimate(model({ weights_bytes: 0 }));
  assert.equal(estimateValue(estimate, "weights"), "unknown");
});
