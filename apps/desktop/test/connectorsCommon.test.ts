// The Connectors window's search and name rules (connectors/common.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { count, matches, NAME_PATTERN } from "../src/connectors/common.ts";

test("a search matches when every word appears in any of the texts, ignoring case", () => {
  assert.ok(matches("git hub", "GitHub", null, undefined));
  assert.ok(matches("", "anything"));
  assert.ok(!matches("git lab", "GitHub"));
  assert.ok(matches("code review", "Code", "review tools"));
});

test("a count reads as a phrase, or nothing when zero", () => {
  assert.equal(count(0, "skill"), null);
  assert.equal(count(1, "skill"), "1 skill");
  assert.equal(count(3, "hook"), "3 hooks");
});

test("server names are lowercase letters, digits, underscore and hyphen, up to forty", () => {
  assert.ok(NAME_PATTERN.test("github"));
  assert.ok(NAME_PATTERN.test("my_server-2"));
  assert.ok(!NAME_PATTERN.test("My Server"));
  assert.ok(!NAME_PATTERN.test("-lead"));
  assert.ok(!NAME_PATTERN.test("a".repeat(41)));
});
