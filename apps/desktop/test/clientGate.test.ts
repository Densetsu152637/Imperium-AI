// A mod frame's request waits for the daemon client when the app is not connected yet (src/mods/clientGate.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { Gate } from "../src/mods/clientGate.ts";

test("a request made before the client is set gets it when it arrives", async () => {
  const gate = new Gate<string>();
  const early = gate.get(1000, "not connected");
  const other = gate.get(1000, "not connected");
  gate.set("client");
  assert.equal(await early, "client");
  assert.equal(await other, "client");
  assert.equal(await gate.get(1000, "not connected"), "client");
});

test("a request fails after the timeout when the client never comes", async () => {
  const gate = new Gate<string>();
  await assert.rejects(gate.get(20, "not connected to imperiumd"), /not connected to imperiumd/);
  // A client that arrives afterwards still serves later requests.
  gate.set("client");
  assert.equal(await gate.get(20, "x"), "client");
});

test("a cleared client makes new requests wait again", async () => {
  const gate = new Gate<string>();
  gate.set("a");
  gate.set(null);
  const next = gate.get(1000, "x");
  gate.set("b");
  assert.equal(await next, "b");
});
