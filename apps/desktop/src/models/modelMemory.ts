// The memory estimate the model configuration dialog shows for a local model: the figures the
// daemon's planner works out for the settings being edited (a `models.set_config` dry run, the
// same planner and calibration that decide where a model loads), arranged for display. Nothing is
// computed here: the planner's formula is
//
//   total = weights + KV pool + runtime overhead (+ a draft model's memory)
//   KV pool = tokens in the pool x bytes per token, which scales with the K and V cache types
//
// and the planner then splits the total between VRAM and RAM: every layer on the GPU when it fits,
// else as many layers as fit (the rest, with their share of the pool, in RAM), or all in RAM on
// the CPU lane. The figures are an estimate: real use also depends on the driver and on what
// else is loaded. Pure (test/modelMemory.test.ts).

import type { LocalModelConfig, TableModelConfig } from "@imperium/protocol";

const GIB = 1024 ** 3;
const MIB = 1024 ** 2;

/** Bytes in words, one decimal for GiB, whole MiB below that: "5.3 GiB", "640 MiB", "0 B". */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  if (bytes >= GIB / 10) return `${(bytes / GIB).toFixed(1)} GiB`;
  return `${Math.max(1, Math.round(bytes / MIB))} MiB`;
}

/** A figure marked as an estimate: "≈ 5.3 GiB". */
export const approx = (bytes: number): string => `≈ ${formatBytes(bytes)}`;

export type Placement = LocalModelConfig["placement"];

/** What the dialog shows for a model's memory. */
export type MemoryEstimate =
  | { kind: "remote" }
  | {
      kind: "local";
      vramBytes: number;
      ramBytes: number;
      totalBytes: number;
      weightsBytes: number;
      kvBytes: number;
      /** What is left of the total after the weights and the KV pool: buffers, and a draft model. */
      otherBytes: number;
      placement: Placement;
      gpuLayers: number | null;
      layers: number | null;
      /** False when the planner could not place the model (too large for this machine now). */
      fits: boolean;
    };

/** The estimate of a local model with the settings the daemon last computed for it. */
export function localEstimate(model: LocalModelConfig): MemoryEstimate {
  const fits = model.placement !== "none";
  const total = model.required_bytes;
  return {
    kind: "local",
    vramBytes: model.vram_bytes,
    ramBytes: model.ram_bytes,
    totalBytes: total,
    weightsBytes: model.weights_bytes,
    kvBytes: model.kv_pool_bytes,
    otherBytes: Math.max(0, total - model.weights_bytes - model.kv_pool_bytes),
    placement: model.placement,
    gpuLayers: model.gpu_layers,
    layers: model.layers,
    fits,
  };
}

/**
 * The memory of a model-table row: a hosted row runs on someone else's computer ("runs
 * remotely"); a row that names a local model (`llamacpp:` or `vllm:`) uses that model's
 * estimate, found by its spec in `local`.
 */
export function rowEstimate(row: Pick<TableModelConfig, "spec" | "location">, local: readonly LocalModelConfig[]): MemoryEstimate {
  const match = local.find((model) => `${model.runtime}:${model.id}` === row.spec);
  if (match) return localEstimate(match);
  return { kind: "remote" };
}

/** One line for a list or a summary: "≈ 5.3 GiB VRAM + ≈ 1.2 GiB RAM", or "runs remotely". */
export function summaryLine(estimate: MemoryEstimate): string {
  if (estimate.kind === "remote") return "runs remotely";
  if (!estimate.fits) return `too large to load now (needs ${approx(estimate.totalBytes)})`;
  const parts = [
    estimate.vramBytes > 0 ? `${approx(estimate.vramBytes)} VRAM` : null,
    estimate.ramBytes > 0 ? `${approx(estimate.ramBytes)} RAM` : null,
  ].filter((part): part is string => part !== null);
  return parts.length > 0 ? parts.join(" + ") : approx(estimate.totalBytes);
}

/** Where the model would run, in words. */
export function placementText(estimate: Extract<MemoryEstimate, { kind: "local" }>): string {
  switch (estimate.placement) {
    case "none":
      return "Too large for this computer's free memory right now";
    case "cpu":
      return "On the CPU only, in system RAM (slow)";
    case "partial": {
      const layers =
        estimate.gpuLayers !== null && estimate.layers !== null
          ? `${estimate.gpuLayers} of ${estimate.layers} layers on the GPU`
          : "Some layers on the GPU";
      return `${layers}, the rest on the CPU (slower)`;
    }
    case "gpu":
      return estimate.layers !== null ? `All ${estimate.layers} layers on the GPU` : "All layers on the GPU";
  }
}

/** The figure of an estimate row, as drawn: "≈ 5.3 GiB", "none", or "runs remotely". */
export function estimateValue(estimate: MemoryEstimate, key: "vram" | "ram" | "total" | "weights" | "kv" | "placement"): string {
  if (estimate.kind === "remote") return "runs remotely";
  switch (key) {
    case "vram":
      return !estimate.fits ? "does not fit" : estimate.vramBytes > 0 ? approx(estimate.vramBytes) : "none";
    case "ram":
      return !estimate.fits ? "does not fit" : estimate.ramBytes > 0 ? approx(estimate.ramBytes) : "none";
    case "total":
      return approx(estimate.totalBytes);
    case "weights":
      return estimate.weightsBytes > 0 ? approx(estimate.weightsBytes) : "unknown";
    case "kv":
      return approx(estimate.kvBytes);
    case "placement":
      return placementText(estimate);
  }
}
