// The behaviour faults a mod frame reports (src/mods/faults.ts): checked and clipped, since a
// frame is untrusted, deduplicated, and kept short. Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { clearFaults, getFaults, recordFault, subscribeFaults } from "../src/mods/faults.ts";

const report = (extra: Record<string, unknown> = {}) => ({ name: "wander:pet", phase: "tick", reason: "trapped: unreachable", ...extra });

test("a well-formed report is kept with the mod it came from", () => {
  clearFaults();
  const fault = recordFault("nursery", report(), 5);
  assert.deepEqual(fault, { modId: "nursery", name: "wander:pet", phase: "tick", reason: "trapped: unreachable", at: 5 });
  assert.equal(getFaults().length, 1);
});

test("malformed reports are dropped and text from the frame is clipped", () => {
  clearFaults();
  for (const bad of [null, "text", 3, {}, report({ phase: "later" }), report({ name: 5 }), report({ reason: "" }), report({ name: "   " })]) {
    assert.equal(recordFault("m", bad), null);
  }
  assert.equal(getFaults().length, 0);
  const long = recordFault("m", report({ reason: `${"x ".repeat(500)}` }));
  assert.ok(long && long.reason.length <= 200);
});

test("the same report twice in a row is kept once", () => {
  clearFaults();
  assert.ok(recordFault("m", report()));
  assert.equal(recordFault("m", report()), null);
  assert.ok(recordFault("m", report({ reason: "another" })));
  assert.ok(recordFault("other", report()));
  assert.equal(getFaults().length, 3);
});

test("a mod cannot flood the list: ten kept per mod, fifty overall", () => {
  clearFaults();
  for (let i = 0; i < 40; i++) recordFault("noisy", report({ reason: `r${i}` }));
  assert.equal(getFaults().filter((f) => f.modId === "noisy").length, 10);
  assert.equal(getFaults().at(-1)?.reason, "r39");
  for (let m = 0; m < 20; m++) for (let i = 0; i < 10; i++) recordFault(`mod${m}`, report({ reason: `r${i}` }));
  assert.ok(getFaults().length <= 50);
});

test("listeners hear changes, the snapshot is a new array each time, and faults can be cleared per mod", () => {
  clearFaults();
  let heard = 0;
  const stop = subscribeFaults(() => heard++);
  const before = getFaults();
  recordFault("a", report());
  recordFault("b", report());
  assert.notEqual(getFaults(), before);
  assert.equal(heard, 2);
  clearFaults("a");
  assert.deepEqual(getFaults().map((f) => f.modId), ["b"]);
  stop();
  clearFaults();
  assert.equal(heard, 3, "a stopped listener hears nothing more");
});
