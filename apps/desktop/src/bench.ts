// Local model benchmark helpers (Settings → Models): merging progress notifications into the
// list of runs, picking the results that belong to this machine, and formatting the numbers.

import type { BenchResult, BenchRun } from "@imperium/protocol";

/** Replaces the run with the same id, or appends it. */
export function mergeRun(runs: BenchRun[], run: BenchRun): BenchRun[] {
  const index = runs.findIndex((other) => other.run_id === run.run_id);
  return index < 0 ? [...runs, run] : runs.map((other, i) => (i === index ? run : other));
}

/** The run that is running now, if any. */
export function activeRun(runs: BenchRun[]): BenchRun | undefined {
  return runs.find((run) => run.state === "running");
}

/** Newest measured result of each model on this machine, whose fingerprints (one per runtime) are given (results arrive newest first). */
export function latestHere(results: BenchResult[], fingerprints: string[]): BenchResult[] {
  const seen = new Set<string>();
  const latest: BenchResult[] = [];
  for (const result of results) {
    if (!fingerprints.includes(result.hardware_fingerprint) || result.error || seen.has(result.model_id)) continue;
    seen.add(result.model_id);
    latest.push(result);
  }
  return latest.sort((a, b) => a.model_id.localeCompare(b.model_id));
}

/** Results not in the table (other machines, older runs), newest first, for the history list. */
export function history(results: BenchResult[], shown: BenchResult[]): BenchResult[] {
  return results.filter((result) => !result.error && !shown.includes(result));
}

export const seconds = (value: number | null | undefined): string => (value == null ? "–" : `${value.toFixed(1)} s`);
export const millis = (value: number | null | undefined): string => (value == null ? "–" : `${Math.round(value)} ms`);
export const tokensPerSecond = (value: number | null | undefined, digits = 0): string =>
  value == null ? "–" : `${value.toFixed(digits)} tok/s`;
export const gibibytes = (bytes: number | null | undefined): string =>
  bytes == null ? "–" : `${(bytes / 1024 ** 3).toFixed(1)} GiB`;
