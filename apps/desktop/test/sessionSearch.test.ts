// Session search in the app (src/sessionSearch.ts): the call, highlighted snippets, and which
// transcript entry a hit opens at.

import assert from "node:assert/strict";
import { test } from "node:test";

import { entryForEvent, highlightSegments, hitKindLabel, searchSessions } from "../src/sessionSearch.ts";

test("searchSessions calls session.search with the query and options", async () => {
  const calls: [string, unknown][] = [];
  const client = {
    call<Result>(method: string, params?: unknown): Promise<Result> {
      calls.push([method, params]);
      return Promise.resolve({ hits: [], semantic: false } as Result);
    },
  };
  const result = await searchSessions(client, "websocket", { environment_id: "env", limit: 5 });
  assert.deepEqual(result, { hits: [], semantic: false });
  assert.deepEqual(calls, [["session.search", { environment_id: "env", limit: 5, query: "websocket" }]]);
});

test("highlightSegments marks the matched ranges", () => {
  const segments = highlightSegments("fix the websocket test", [
    { start: 18, end: 22 },
    { start: 8, end: 17 },
  ]);
  assert.deepEqual(segments, [
    { text: "fix the ", match: false },
    { text: "websocket", match: true },
    { text: " ", match: false },
    { text: "test", match: true },
  ]);
  // Ranges past the text or overlapping an earlier one are ignored.
  assert.deepEqual(highlightSegments("abc", [{ start: 1, end: 9 }]), [{ text: "abc", match: false }]);
  // Offsets are UTF-16, as JavaScript strings index.
  assert.deepEqual(highlightSegments("é😀 cat", [{ start: 4, end: 7 }]), [
    { text: "é😀 ", match: false },
    { text: "cat", match: true },
  ]);
});

test("entryForEvent finds the entry that holds an event", () => {
  const starts = [0, 1, 4, undefined, 9];
  assert.equal(entryForEvent(starts, 0), 0);
  assert.equal(entryForEvent(starts, 3), 1, "a later part of the reply that started at 1");
  assert.equal(entryForEvent(starts, 4), 2);
  assert.equal(entryForEvent(starts, 100), 4);
  assert.equal(entryForEvent([5], 2), null);
});

test("hit kinds read as labels", () => {
  assert.equal(hitKindLabel("user"), "You");
  assert.equal(hitKindLabel("report"), "Subagent report");
  assert.equal(hitKindLabel("other"), "other");
});
