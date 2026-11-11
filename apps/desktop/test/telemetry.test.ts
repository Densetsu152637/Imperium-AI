// Live telemetry rows and their formatting (src/telemetry.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { ModelRow } from "@imperium/protocol";

import type { ModelCall, SessionState, TaskState } from "../src/sessions.ts";
import {
  LINGER_MS,
  formatSpeed,
  formatTelemetryCost,
  formatTokenCount,
  isLocalModel,
  liveTelemetry,
  modelLabel,
  telemetryTable,
  withLingering,
  type Lingering,
  type TelemetryRow,
} from "../src/telemetry.ts";

const tableRow = (id: string, spec: string, location: "local" | "cloud"): ModelRow => ({
  id,
  spec,
  location,
  alias: null,
  tier: "standard",
  tags: [],
  input_usd_per_mtok: null,
  output_usd_per_mtok: null,
  endpoint_configured: true,
  context_window: null,
  enabled: true,
});

const MODELS = [tableRow("claude-sonnet", "anthropic:claude-sonnet-4-5", "cloud"), tableRow("box", "ollama:qwen3:4b", "local")];

const call = (agentId: string, model: string, prompt: number, completion: number, at: number, extra: Partial<ModelCall> = {}): ModelCall => ({
  at,
  agentId,
  model,
  usage: { prompt_tokens: prompt, completion_tokens: completion, cached_tokens: 0 } as ModelCall["usage"],
  durationMs: 1000,
  ttftMs: 100,
  tokensPerSec: 50,
  costUsd: null,
  ...extra,
});

const task = (taskId: string, model: string, status: TaskState["status"], callsFrom = 0): TaskState => ({
  taskId,
  brief: "",
  model,
  status,
  entries: [],
  report: "",
  costUsd: null,
  callsFrom,
});

const session = (over: Partial<SessionState>): SessionState =>
  ({
    summary: { model: "anthropic:claude-sonnet-4-5" },
    entries: [],
    tasks: {},
    plan: null,
    calls: [],
    turnCallsFrom: 0,
    breakdowns: {},
    busy: false,
    queue: [],
    browser: null,
    status: "working",
    statusDetail: null,
    pause: null,
    updatedAt: 0,
    eventCount: 0,
    waits: {},
    sent: null,
    ...over,
  }) as SessionState;

test("tok/s has one decimal, or an em dash before a measurement", () => {
  assert.equal(formatSpeed(120), "120.0");
  assert.equal(formatSpeed(59.96), "60.0");
  assert.equal(formatSpeed(null), "—");
});

test("token counts are plain under a thousand, else two decimals with k or M", () => {
  assert.equal(formatTokenCount(0), "0");
  assert.equal(formatTokenCount(999), "999");
  assert.equal(formatTokenCount(1000), "1.00k");
  assert.equal(formatTokenCount(1600), "1.60k");
  assert.equal(formatTokenCount(54_000), "54.00k");
  assert.equal(formatTokenCount(999_999), "1.00M");
  assert.equal(formatTokenCount(1_000_000), "1.00M");
  assert.equal(formatTokenCount(12_345_678), "12.35M");
});

test("cost is dollars with two decimals, an em dash for local models or no reported cost", () => {
  assert.equal(formatTelemetryCost(1.6, false), "$1.60");
  assert.equal(formatTelemetryCost(0, false), "$0.00");
  assert.equal(formatTelemetryCost(1.6, true), "—");
  assert.equal(formatTelemetryCost(null, false), "—");
});

test("models go by their alias, else their table id, else the spec without its endpoint", () => {
  assert.equal(modelLabel("anthropic:claude-sonnet-4-5", MODELS), "claude-sonnet");
  assert.equal(modelLabel("llamacpp:Qwen3-4B-Q4_K_M", MODELS), "Qwen3-4B-Q4_K_M");
  assert.equal(modelLabel("bare", MODELS), "bare");
  // A row's own alias, then a published one (a local model's) for the spec or the row.
  const aliased = [{ ...MODELS[0], alias: "Sonnet" }, MODELS[1]];
  assert.equal(modelLabel("anthropic:claude-sonnet-4-5", aliased), "Sonnet");
  const aliases = new Map([
    ["llamacpp:Qwen3-4B-Q4_K_M", "Qwen 3 (4B)"],
    ["box", "Box Qwen"],
  ]);
  assert.equal(modelLabel("llamacpp:Qwen3-4B-Q4_K_M", MODELS, aliases), "Qwen 3 (4B)");
  assert.equal(modelLabel("ollama:qwen3:4b", MODELS, aliases), "Box Qwen");
  assert.equal(modelLabel("bare", MODELS, aliases), "bare");
});

test("local runtimes and table rows marked local are local", () => {
  assert.equal(isLocalModel("llamacpp:qwen", MODELS), true);
  assert.equal(isLocalModel("vllm:qwen", MODELS), true);
  assert.equal(isLocalModel("ollama:qwen3:4b", MODELS), true);
  assert.equal(isLocalModel("anthropic:claude-sonnet-4-5", MODELS), false);
  assert.equal(isLocalModel("openai:gpt", MODELS), false);
});

test("the table has the fixed header and one row per model, pipes escaped", () => {
  const rows: TelemetryRow[] = [
    { model: "llamacpp:q", label: "Qwen 3 (4B)", tokensPerSec: 120, tokensIn: 1_000_000, tokensOut: 1600, costUsd: null, local: true },
    { model: "a:s", label: "Claude (Sonnet)", tokensPerSec: 60, tokensIn: 54_000, tokensOut: 2000, costUsd: 1.6, local: false },
    { model: "x:y", label: "a|b", tokensPerSec: null, tokensIn: 0, tokensOut: 0, costUsd: null, local: false },
  ];
  assert.equal(
    telemetryTable(rows),
    [
      "| Model | tok/s | in | out | cost |",
      "|---|---|---|---|---|",
      "| Qwen 3 (4B) | 120.0 | 1.00M | 1.60k | — |",
      "| Claude (Sonnet) | 60.0 | 54.00k | 2.00k | $1.60 |",
      "| a\\|b | — | 0 | 0 | — |",
    ].join("\n"),
  );
});

test("only running work counts: the root's current turn and running subagents", () => {
  const calls = [
    call("root", "anthropic:claude-sonnet-4-5", 9000, 900, 1, { costUsd: 5 }), // an earlier turn
    call("root", "anthropic:claude-sonnet-4-5", 50_000, 1000, 2, { costUsd: 1, tokensPerSec: 55 }),
    call("t1", "llamacpp:qwen", 400_000, 800, 3, { tokensPerSec: 110 }),
    call("root", "anthropic:claude-sonnet-4-5", 4000, 1000, 4, { costUsd: 0.6, tokensPerSec: 60 }),
    call("t1", "llamacpp:qwen", 600_000, 800, 5, { tokensPerSec: 120 }),
    call("t2", "llamacpp:qwen", 7, 7, 6), // finished subagent
  ];
  const rows = liveTelemetry(
    [
      session({
        busy: true,
        turnCallsFrom: 1,
        calls,
        tasks: { t1: task("t1", "llamacpp:qwen", "running", 2), t2: task("t2", "llamacpp:qwen", "finished", 5) },
      }),
      session({ busy: false, calls: [call("root", "openai:gpt", 1, 1, 1)] }),
    ],
    MODELS,
  );
  assert.deepEqual(rows, [
    { model: "anthropic:claude-sonnet-4-5", label: "claude-sonnet", tokensPerSec: 60, tokensIn: 54_000, tokensOut: 2000, costUsd: 1.6, local: false },
    { model: "llamacpp:qwen", label: "qwen", tokensPerSec: 120, tokensIn: 1_000_000, tokensOut: 1600, costUsd: null, local: true },
  ]);
});

test("a turn's model is listed before its first call; stopped sessions list nothing", () => {
  const busy = session({ busy: true, summary: { model: "openai:gpt" } as SessionState["summary"] });
  assert.deepEqual(
    liveTelemetry([busy], MODELS).map((row) => [row.label, row.tokensPerSec, row.tokensIn]),
    [["gpt", null, 0]],
  );
  assert.deepEqual(liveTelemetry([{ ...busy, status: "paused", statusDetail: "stopped" }], MODELS), []);
});

test("idle rows linger for a minute with their last values, then drop off", () => {
  const row = (model: string, tokensIn: number): TelemetryRow => ({
    model,
    label: model,
    tokensPerSec: 1,
    tokensIn,
    tokensOut: 0,
    costUsd: null,
    local: true,
  });
  let state: { rows: TelemetryRow[]; lingering: Lingering } = { rows: [], lingering: new Map() };
  state = withLingering([row("a", 1), row("b", 2)], state, 0);
  assert.deepEqual(state.rows.map((r) => r.model), ["a", "b"]);
  state = withLingering([row("b", 3)], state, 1000);
  assert.deepEqual(state.rows, [row("b", 3), row("a", 1)]);
  state = withLingering([row("b", 3)], state, 1000 + LINGER_MS - 1);
  assert.deepEqual(state.rows.map((r) => r.model), ["b", "a"]);
  state = withLingering([row("b", 3)], state, 1000 + LINGER_MS);
  assert.deepEqual(state.rows.map((r) => r.model), ["b"]);
  // Active again: listed once, live.
  state = withLingering([], state, 0);
  state = withLingering([row("b", 9)], state, 10);
  assert.deepEqual(state.rows, [row("b", 9)]);
});
