// Stopping and resuming a session (src/stop.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { canStop, isStopShortcut, isStopped, shownTaskStatus } from "../src/stop.ts";

test("a stopped session is paused with the detail stopped, unlike a usage-limit wait", () => {
  assert.equal(isStopped({ status: "paused", statusDetail: "stopped" }), true);
  assert.equal(isStopped({ status: "paused", statusDetail: null }), false);
  assert.equal(isStopped({ status: "working", statusDetail: null }), false);
});

test("Stop is offered while something runs and the session is not stopped", () => {
  const working = { status: "working" as const, statusDetail: null, busy: true };
  assert.equal(canStop(working, 0), true);
  assert.equal(canStop({ ...working, status: "idle", busy: false }, 0), false);
  assert.equal(canStop({ ...working, status: "idle", busy: false }, 1), true);
  assert.equal(canStop({ status: "paused", statusDetail: "stopped", busy: true }, 1), false);
});

test("Ctrl+. or Cmd+. toggles the stop", () => {
  const key = { key: ".", ctrlKey: true, metaKey: false, altKey: false };
  assert.equal(isStopShortcut(key), true);
  assert.equal(isStopShortcut({ ...key, ctrlKey: false, metaKey: true }), true);
  assert.equal(isStopShortcut({ ...key, ctrlKey: false }), false);
  assert.equal(isStopShortcut({ ...key, altKey: true }), false);
  assert.equal(isStopShortcut({ ...key, key: "," }), false);
});

test("running subagents show as paused while the session is stopped", () => {
  assert.equal(shownTaskStatus("running", true), "paused");
  assert.equal(shownTaskStatus("running", false), "running");
  assert.equal(shownTaskStatus("finished", true), "finished");
});
