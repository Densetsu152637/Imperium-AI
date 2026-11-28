// The Browser panel's takeover goes only through the app's own commands (sessions/browserTakeover.ts),
// with the session and the action and nothing else.

import assert from "node:assert/strict";
import { test } from "node:test";

import { actInBrowser, controlBrowser } from "../src/sessions/browserTakeover.ts";

test("taking over and giving back ask the app's browser_control command", async () => {
  const sent: Array<[string, Record<string, unknown>]> = [];
  const invoke = async (command: string, args: Record<string, unknown>) => {
    sent.push([command, args]);
    return { view: null };
  };
  await controlBrowser(invoke, "s1", true);
  await controlBrowser(invoke, "s1", false);
  assert.deepEqual(sent, [
    ["browser_control", { session: "s1", user: true }],
    ["browser_control", { session: "s1", user: false }],
  ]);
});

test("an action asks the app's browser_act command and a refusal reaches the panel", async () => {
  const sent: Array<[string, Record<string, unknown>]> = [];
  await actInBrowser(
    async (command, args) => {
      sent.push([command, args]);
      return { view: { url: "https://example.com/", title: "Example", screenshot: "", tabs: [] } };
    },
    "s1",
    { type: "navigate", url: "https://example.com/" },
  );
  assert.deepEqual(sent, [["browser_act", { session: "s1", action: { type: "navigate", url: "https://example.com/" } }]]);
  await assert.rejects(
    actInBrowser(async () => {
      throw new Error("permission denied: only the desktop app can take over or drive a session's browser");
    }, "s1", { type: "reload" }),
    /only the desktop app/,
  );
});
