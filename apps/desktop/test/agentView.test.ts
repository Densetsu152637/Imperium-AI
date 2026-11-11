// A subagent opened in the main panel: its title, how long it has run, and how a message typed
// to it is delivered (src/agentView.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  agentComposerHint,
  agentDelivery,
  finishedMeanwhile,
  followUpText,
  formatElapsed,
  taskElapsed,
  taskTitle,
} from "../src/agentView.ts";

test("a subagent is named by its title, else by the first line of its brief", () => {
  assert.equal(taskTitle("Fix login redirect test", "anything"), "Fix login redirect test");
  // Journals written before titles carry none.
  assert.equal(taskTitle(undefined, "\n  Audit the auth flow\nthen report"), "Audit the auth flow");
  assert.equal(taskTitle("  ", "Write docs"), "Write docs");
  assert.equal(taskTitle("", ""), "Subagent");
  const long = taskTitle(undefined, "x".repeat(100));
  assert.equal(long.length, 60);
  assert.ok(long.endsWith("…"));
});

test("elapsed time counts up while it runs and stops when it ends", () => {
  assert.equal(formatElapsed(0), "0s");
  assert.equal(formatElapsed(45_400), "45s");
  assert.equal(formatElapsed(185_000), "3m 05s");
  assert.equal(formatElapsed(3_720_000), "1h 02m");
  const started = 1_000_000;
  assert.equal(taskElapsed({ status: "running", startedAt: started, endedAt: null }, started + 7_000), 7_000);
  assert.equal(taskElapsed({ status: "finished", startedAt: started, endedAt: started + 2_000 }, started + 9_000), 2_000);
  assert.equal(taskElapsed({ status: "paused", startedAt: started, endedAt: started + 3_000 }, started + 9_000), 3_000);
  // Older journals have no times.
  assert.equal(taskElapsed({ status: "running", startedAt: null, endedAt: null }), null);
  assert.equal(taskElapsed({ status: "failed", startedAt: started, endedAt: null }), null);
});

test("a running subagent is queued for or steered; a finished or paused one is given a follow-up", () => {
  assert.deepEqual(agentDelivery("running", "queue"), { kind: "send", mode: "queue" });
  assert.deepEqual(agentDelivery("running", "steer"), { kind: "send", mode: "steer" });
  for (const status of ["finished", "failed", "paused"] as const) {
    assert.deepEqual(agentDelivery(status, "steer"), { kind: "resume" });
  }
  // One that finished between the click and the call is sent the message as a follow-up.
  assert.ok(finishedMeanwhile(new Error('no running agent "t1" in this session')));
  assert.ok(!finishedMeanwhile(new Error("the message is empty")));
});

test("a follow-up names the files attached to it", () => {
  assert.equal(followUpText("look", []), "look");
  assert.equal(
    followUpText("look", [{ name: "a.png", path: "/tmp/a.png" }]),
    "look\n\nAttached files:\n- a.png: /tmp/a.png",
  );
  assert.equal(followUpText("", [{ name: "a.png", path: "/p" }]), "Attached files:\n- a.png: /p");
});

test("the composer says what sending does for the subagent", () => {
  assert.match(agentComposerHint("Audit auth", "running", true), /^Enter queues for Audit auth/);
  assert.match(agentComposerHint("Audit auth", "paused", false), /resume it \(Ctrl\+Enter\)/);
  assert.match(agentComposerHint("Audit auth", "finished", true), /follow-up/);
});
