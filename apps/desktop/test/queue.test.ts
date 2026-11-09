// What a waiting message offers, and the requests its buttons send (src/queue.ts). Run with
// `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { editStillValid, queueActions, queueLabel, queueParams, steeredLabel } from "../src/queue.ts";

const queued = { id: "q1", text: "then add tests", steer: false };
const steering = { id: "q2", text: "use tabs", steer: true };

test("a queued message can be edited, steered while a turn runs, or removed", () => {
  assert.deepEqual(queueActions(queued, true), ["edit", "steer", "remove"]);
  assert.deepEqual(queueActions(queued, false), ["edit", "remove"]);
  assert.equal(queueLabel(queued), "Queued");
});

test("a steering message can only be removed before it lands", () => {
  assert.deepEqual(queueActions(steering, true), ["remove"]);
  assert.equal(queueLabel(steering), "Steering…");
});

test("each change becomes its session.queue request", () => {
  assert.deepEqual(queueParams("s1", "q1", { kind: "steer" }), { session_id: "s1", action: "steer", id: "q1" });
  assert.deepEqual(queueParams("s1", "q1", { kind: "remove" }), { session_id: "s1", action: "remove", id: "q1" });
  assert.deepEqual(queueParams("s1", "q1", { kind: "edit", text: "x" }), {
    session_id: "s1",
    action: "edit",
    id: "q1",
    text: "x",
  });
});

test("an edit stops when its message is steered, delivered, or removed elsewhere", () => {
  assert.equal(editStillValid([queued, steering], "q1"), true);
  assert.equal(editStillValid([queued, steering], "q2"), false);
  assert.equal(editStillValid([steering], "q1"), false);
});

test("the transcript says how many subagents a steering message paused", () => {
  assert.equal(steeredLabel([]), "Steered");
  assert.equal(steeredLabel(["t1"]), "Steered — 1 subagent paused");
  assert.equal(steeredLabel(["t1", "t2"]), "Steered — 2 subagents paused");
});
