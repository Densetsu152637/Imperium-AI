// Finding a model-table row by id, or by spec then id (src/models/spec.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { rowById, rowBySpecOrId } from "../src/models/spec.ts";

const rows = [
  { id: "fast", spec: "openai:gpt-fast" },
  { id: "openai:gpt-fast", spec: "other:thing" },
  { id: "slow", spec: "openai:gpt-slow" },
];

test("a row is found by its id, never by its spec", () => {
  assert.equal(rowById(rows, "slow")?.spec, "openai:gpt-slow");
  assert.equal(rowById(rows, "openai:gpt-slow"), undefined);
  assert.equal(rowById([], "slow"), undefined);
});

test("a name is a spec first and an id second", () => {
  assert.equal(rowBySpecOrId(rows, "openai:gpt-slow")?.id, "slow");
  assert.equal(rowBySpecOrId(rows, "slow")?.id, "slow");
  // A spec that is also some row's id: the row with that spec wins.
  assert.equal(rowBySpecOrId(rows, "openai:gpt-fast")?.id, "fast");
  assert.equal(rowBySpecOrId(rows, "nothing"), undefined);
});
