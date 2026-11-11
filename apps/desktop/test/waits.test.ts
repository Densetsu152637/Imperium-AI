// What an agent waits on before its first output (src/waits.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { AgentEvent, ModelResidency, RuntimeInstallJob, SessionEvent } from "@imperium/protocol";

import {
  applyResidency,
  applySent,
  applyWait,
  describeWait,
  formatElapsed,
  installText,
  localWaitText,
  residencyText,
  type Residency,
  type Sent,
  type Waits,
} from "../src/waits.ts";

const on = (agent_id: string, event: SessionEvent): AgentEvent => ({ agent_id, event });
const run = (events: AgentEvent[], start: Waits = {}): Waits =>
  events.reduce((waits, event, index) => applyWait(waits, event, 1000 + index), start);

test("every local wait reason is one sentence about its model; new reasons are worded from their name", () => {
  assert.equal(localWaitText("loading", "Qwen"), "Loading Qwen…");
  assert.equal(localWaitText("loading"), "Loading the model…");
  assert.equal(localWaitText("growing_pool", "Qwen"), "Growing Qwen's context pool…");
  assert.equal(localWaitText("kv_room", "Qwen"), "Waiting for room in Qwen's context cache…");
  assert.equal(localWaitText("slot", "Qwen"), "Waiting for a free slot on Qwen…");
  assert.equal(localWaitText("other_model", "Qwen"), "Waiting for another model before Qwen…");
  assert.equal(localWaitText("making_room", "Qwen"), "Making room on the GPU for Qwen…");
  assert.equal(localWaitText("saving_context", "Qwen"), "Saving other agents' context to make room for Qwen…");
  assert.equal(localWaitText("restoring_context", "Qwen"), "Restoring this agent's context on Qwen…");
  assert.equal(localWaitText("warming_up_cache"), "Warming up cache…");
  assert.equal(localWaitText(""), "Waiting for the local model…");
});

test("an install shows its share once the size is known", () => {
  const job: RuntimeInstallJob = { pack: "cuda-13", state: "running", done_bytes: 42, total_bytes: 100, message: null };
  assert.equal(installText(job), "Installing llama.cpp (42%)…");
  assert.equal(installText({ ...job, total_bytes: 0 }), "Installing llama.cpp…");
});

test("a turn waits from its start, through local waits, until its first output", () => {
  let waits = run([on("root", { type: "turn_started", text: "hi" })]);
  assert.deepEqual(waits.root, { since: 1000, cause: null });
  waits = run([on("root", { type: "local_model_waiting", agent_id: "root", model: "qwen", reason: "loading" })], waits);
  // The wait keeps its start; only the cause changes.
  assert.deepEqual(waits.root, { since: 1000, cause: { kind: "local", reason: "loading", model: "qwen" } });
  assert.equal(describeWait({ wait: waits.root })?.text, "Loading qwen…");
  waits = run([on("root", { type: "text_delta", text: "Hel" })], waits);
  assert.equal(waits.root, undefined);
  assert.equal(describeWait({ wait: waits.root }), null);
});

test("a tool call, a finished turn, or a failure clears the wait too; other agents keep theirs", () => {
  for (const output of [
    { type: "tool_started", call_id: "c", tool: "read", summary: "read" },
    { type: "turn_failed", message: "no" },
    { type: "model_call_completed" },
  ] as SessionEvent[]) {
    const waits = run([on("root", { type: "turn_started", text: "x" }), on("t1", { type: "turn_started", text: "y" }), on("root", output)]);
    assert.equal(waits.root, undefined, output.type);
    assert.ok(waits.t1);
  }
});

test("a subagent waits from when it is spawned, and its local waits are its own", () => {
  const waits = run([
    on("root", { type: "task_spawned", task_id: "t1", brief: "b", model: "m" } as SessionEvent),
    on("t1", { type: "local_model_waiting", agent_id: "t1", model: "gemma", reason: "slot" }),
  ]);
  assert.deepEqual(waits.t1, { since: 1000, cause: { kind: "local", reason: "slot", model: "gemma" } });
  assert.equal(waits.root, undefined);
});

test("compaction shows until it ends, then the turn still waits for output", () => {
  let waits = run([on("root", { type: "compaction_started", model: "a:m" })]);
  assert.equal(describeWait({ wait: waits.root })?.text, "Compacting conversation…");
  waits = run([on("root", { type: "context_compacted", model: "a:m" } as SessionEvent)], waits);
  assert.deepEqual(waits.root, { since: 1000, cause: null });
  // A compaction that failed before a turn ends with a notice.
  waits = run([on("root", { type: "compaction_started", model: "a:m" }), on("root", { type: "notice", message: "could not" })]);
  assert.equal(waits.root.cause, null);
});

test("approval and questions are waits on the user", () => {
  let waits = run([on("root", { type: "turn_started", text: "x" }), on("root", { type: "permission_requested" } as SessionEvent)]);
  assert.equal(describeWait({ wait: waits.root })?.text, "Waiting for your approval…");
  assert.equal(describeWait({ wait: waits.root })?.action, null);
  waits = run([on("root", { type: "questions_asked", request_id: 1, questions: [] })]);
  assert.equal(describeWait({ wait: waits.root })?.text, "Waiting for your answers…");
});

test("a sent message waits until its turn starts, or until it is queued", () => {
  const sent: Sent = { text: "hi", since: 500, budget: null };
  assert.deepEqual(describeWait({ wait: undefined, sent }), { text: "Starting…", since: 500, model: null, action: "stop" });
  assert.equal(applySent(sent, on("root", { type: "turn_started", text: "hi" })), null);
  assert.equal(applySent(sent, on("root", { type: "message_queued", id: "q1", text: "hi", steer: false })), null);
  assert.equal(applySent(sent, on("root", { type: "status_changed", status: "needs_model", detail: null })), null);
  assert.equal(applySent(sent, on("t1", { type: "turn_started", text: "x" })), sent);
  assert.equal(applySent(sent, on("root", { type: "local_model_waiting", agent_id: "root", model: "m", reason: "loading" })), sent);
  // The wait before the turn and the sent message share the earlier start.
  const view = describeWait({ wait: { since: 900, cause: { kind: "local", reason: "loading", model: "m" } }, sent });
  assert.equal(view?.since, 500);
  assert.equal(view?.text, "Loading m…");
});

test("a budget question, a stop, and a pause come before the core's own waits", () => {
  const wait = { since: 1, cause: { kind: "local" as const, reason: "loading", model: "m" } };
  assert.equal(describeWait({ wait, sent: { text: "x", since: 1, budget: "Daily budget used up" } })?.action, "budget");
  assert.equal(describeWait({ wait, stopped: true })?.action, "resume");
  const pause = describeWait({ wait, pause: { cause: "usage_limit", until: "14:00" } });
  assert.equal(pause?.text, "Usage limit reached; waiting until 14:00");
  assert.equal(pause?.action, "resume_now");
  assert.equal(describeWait({ wait, pause: { cause: "deferred", until: null } })?.text, "Waiting for a quiet machine…");
});

test("loading while llama.cpp installs names the install", () => {
  const install: RuntimeInstallJob = { pack: "vulkan", state: "running", done_bytes: 1, total_bytes: 4, message: null };
  const wait = { since: 1, cause: { kind: "local" as const, reason: "loading", model: "m" } };
  assert.equal(describeWait({ wait, install })?.text, "Installing llama.cpp (25%)…");
  assert.equal(describeWait({ wait, install: { ...install, state: "done" } })?.text, "Loading m…");
});

const move = (fields: Partial<ModelResidency>): ModelResidency => ({
  action: "unload",
  model_id: "qwen",
  reason: "room",
  ok: true,
  seconds: 0,
  bytes: 0,
  ...fields,
});

test("a move made for the waiting model since the wait began says what is happening, by the model's alias", () => {
  const wait = { since: 1000, cause: { kind: "local" as const, reason: "loading", model: "qwen" } };
  const name = (id: string) => ({ qwen: "Qwen 3", gemma: "Gemma" })[id] ?? id;
  let residency: Residency = applyResidency({}, move({ reason: "crashed", detail: "the server exited" }), 1500);
  assert.equal(describeWait({ wait, residency, name })?.text, "Restoring Qwen 3 after a crash…");
  // An eviction counts for the model it made room for.
  residency = applyResidency({}, move({ model_id: "gemma", reason: "more_gpu", detail: "made room for qwen" }), 1500);
  assert.deepEqual(Object.keys(residency), ["qwen"]);
  assert.equal(describeWait({ wait, residency, name })?.text, "Swapping out Gemma so Qwen 3 fits on the GPU…");
  // Its own load is what the wait reason says already; an older move says nothing about this wait.
  residency = applyResidency(residency, move({ action: "load", reason: "call" }), 1600);
  assert.equal(describeWait({ wait, residency, name })?.text, "Loading Qwen 3…");
  residency = applyResidency({}, move({ reason: "crashed" }), 900);
  assert.equal(describeWait({ wait, residency, name })?.text, "Loading Qwen 3…");
  // The budget concerns no model.
  assert.deepEqual(applyResidency({}, move({ action: "set_budget", model_id: undefined, reason: "budget" }), 1), {});
});

test("every residency reason has one sentence; failed moves and unknown models fall back", () => {
  const said = (fields: Partial<ModelResidency>) => residencyText(move(fields), "Qwen");
  assert.equal(said({ reason: "crashed" }), "Restoring Qwen after a crash…");
  assert.equal(said({ model_id: "b", reason: "room" }), "Swapping out b to make room for Qwen…");
  assert.equal(said({ reason: "gpu_rule" }), "Making room on the GPU for Qwen…");
  assert.equal(said({ action: "resize_pool", reason: "grow" }), "Restarting Qwen with a larger context pool…");
  assert.equal(said({ reason: "promote" }), "Moving Qwen onto the GPU…");
  assert.equal(said({ reason: "config" }), "Restarting Qwen with its new settings…");
  assert.equal(said({ action: "restore_slot", reason: "resume" }), "Bringing Qwen back for this agent…");
  assert.equal(said({ reason: "brand_new" }), "Brand new…");
  assert.equal(said({ reason: "crashed", ok: false }), null);
  assert.equal(said({ action: "load", reason: "call" }), null);
});

test("elapsed time reads as minutes and seconds, with hours when long", () => {
  assert.equal(formatElapsed(0), "0:00");
  assert.equal(formatElapsed(7_400), "0:07");
  assert.equal(formatElapsed(754_000), "12:34");
  assert.equal(formatElapsed(3_723_000), "1:02:03");
  assert.equal(formatElapsed(-5), "0:00");
});
