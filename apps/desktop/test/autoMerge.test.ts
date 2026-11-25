// Turning auto-merge on and off from the card (sessions/autoMerge.ts): on only through the app's own
// command with the commit the card showed, never a challenge; off through an ordinary daemon call
// that sends no enabling flag.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { PrStatus } from "@imperium/protocol";

import { problemOf, turnOffAutoMerge, turnOnAutoMerge } from "../src/sessions/autoMerge.ts";

const status = {
  url: "https://github.com/owner/repo/pull/7",
  head_sha: "abc123",
} as PrStatus;

test("turning it on asks the app's own command with the session, the commit and the method only", async () => {
  const sent: Array<[string, Record<string, unknown>]> = [];
  const outcome = await turnOnAutoMerge(
    async (command, args) => {
      sent.push([command, args]);
      return { enabled: true, cancelled: false, reason: null };
    },
    "s1",
    status,
    "merge",
  );
  assert.deepEqual(sent, [["auto_merge_enable", { session: "s1", head: "abc123", method: "merge" }]]);
  assert.equal(outcome.enabled, true);
  assert.equal(problemOf(outcome), null);
});

test("turning it off is a daemon call with enabled false and no method or challenge", async () => {
  const sent: Array<[string, unknown]> = [];
  const answer = { auto_merge: { enabled: false } } as PrStatus;
  const result = await turnOffAutoMerge(
    {
      call: async <Result>(method: string, params?: unknown) => {
        sent.push([method, params]);
        return answer as Result;
      },
    },
    "s1",
    status,
  );
  assert.equal(result, answer);
  assert.deepEqual(sent, [["pull_requests.set_auto_merge", { session_id: "s1", url: status.url, head_sha: "abc123", enabled: false }]]);
});

test("a cancel says nothing and a refusal says why, as plain text", () => {
  assert.equal(problemOf({ enabled: false, cancelled: true, reason: "cancelled: nothing was asked of GitHub" }), null);
  assert.equal(problemOf({ enabled: false, cancelled: false, reason: "the branch moved\u202e" }), "the branch moved");
  assert.equal(problemOf({ enabled: false, cancelled: false, reason: null }), "Auto-merge could not be turned on.");
});
