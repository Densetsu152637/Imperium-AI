// The Connectors window's search and name rules (connectors/common.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import { count, isHttpUrl, matches, missingSecretIndex, nameProblem, NAME_PATTERN, urlProblem } from "../src/connectors/common.ts";

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

test("only absolute http and https addresses pass the URL check", () => {
  assert.ok(isHttpUrl("https://example.com/mcp"));
  assert.ok(isHttpUrl(" http://localhost:8080 "));
  assert.ok(!isHttpUrl("example.com/mcp"));
  assert.ok(!isHttpUrl("ftp://example.com"));
  assert.ok(!isHttpUrl("javascript:alert(1)"));
  assert.ok(!isHttpUrl("http//broken"));
});

test("a URL field is required only when it must be, and malformed text is always a problem", () => {
  assert.match(urlProblem("", true) ?? "", /URL/);
  assert.equal(urlProblem("  ", false), null);
  assert.match(urlProblem("not a url", false) ?? "", /http/);
  assert.equal(urlProblem("https://example.com", true), null);
});

test("a name problem is the pattern, then a clash with an existing connector for new ones only", () => {
  const taken = new Set(["github"]);
  assert.match(nameProblem("Bad Name", taken, false) ?? "", /lowercase/);
  assert.match(nameProblem("github", taken, false) ?? "", /already exists/);
  assert.equal(nameProblem("github", taken, true), null);
  assert.equal(nameProblem("slack", taken, false), null);
});

test("the missing secret is the first named row with no typed and no saved value", () => {
  const row = (key: string, value: string, existing: string | null) => ({ key, value, existing });
  assert.equal(missingSecretIndex([]), -1);
  assert.equal(missingSecretIndex([row("A", "x", null), row("B", "", "saved"), row("", "", null)]), -1);
  assert.equal(missingSecretIndex([row("A", "x", null), row(" B ", "", null), row("C", "", null)]), 1);
});
