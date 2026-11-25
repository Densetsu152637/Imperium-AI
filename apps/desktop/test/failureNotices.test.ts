// The failure notices the desktop shows for `autofix.event` (src/shared/failureNotices.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { AutofixEventParams } from "@imperium/protocol";

import { failureNotice, REPEAT_QUIET_MS, type FailureMemory } from "../src/shared/failureNotices.ts";

const event = (over: Partial<AutofixEventParams> = {}): AutofixEventParams => ({
  repo: "owner/repo",
  pull_request: 7,
  status: "failed",
  session_id: null,
  summary: "auto-merge on owner/repo#7 could not be turned off yet; Imperium will try again",
  trigger: "ci",
  comment_ids: [],
  ...over,
});

test("a failed event shows a title and the summary, and notifies as a failure", () => {
  const shown = failureNotice(event(), new Map(), 1000);
  assert.ok(shown);
  assert.equal(shown.toast.title, "GitHub automation failed on owner/repo#7");
  assert.match(shown.toast.body, /could not be turned off yet/);
  assert.equal(shown.notice.kind, "failed");
  assert.equal(shown.notice.body, shown.toast.body);
});

test("a failure that is not about one pull request names only the repository", () => {
  const shown = failureNotice(event({ pull_request: 0 }), new Map(), 0);
  assert.equal(shown?.toast.title, "GitHub automation failed on owner/repo");
});

test("other statuses show nothing", () => {
  for (const status of ["started", "awaiting_approval", "pushed", "declined", "nothing", "stopped", "FAILED", ""]) {
    assert.equal(failureNotice(event({ status }), new Map(), 0), null, status);
  }
});

test("text stays plain: markup is kept as characters and hidden control characters are removed", () => {
  const summary = "<img src=x onerror=alert(1)> **bold** \u202eevil\u200b\u0007";
  const shown = failureNotice(event({ summary, repo: "o/<b>r</b>" }), new Map(), 0);
  assert.ok(shown);
  assert.equal(shown.toast.body, "<img src=x onerror=alert(1)> **bold** evil");
  assert.ok(shown.toast.title.includes("<b>r</b>"));
  assert.equal(typeof shown.toast.body, "string");
  assert.doesNotMatch(shown.toast.body, /[\u202e\u200b\u0007]/);
});

test("a long summary is cut and an empty one gets a placeholder", () => {
  const long = failureNotice(event({ summary: "x".repeat(1000) }), new Map(), 0);
  assert.ok(long && long.toast.body.length <= 301);
  assert.equal(failureNotice(event({ summary: "  " }), new Map(), 0)?.toast.body, "No details were given.");
});

test("an identical failure is quiet for a while, a different one is not", () => {
  const memory: FailureMemory = new Map();
  assert.ok(failureNotice(event(), memory, 0));
  assert.equal(failureNotice(event(), memory, 1000), null);
  assert.equal(failureNotice(event(), memory, REPEAT_QUIET_MS - 1), null);
  assert.ok(failureNotice(event({ pull_request: 8 }), memory, 1000));
  assert.ok(failureNotice(event({ summary: "another reason" }), memory, 1000));
  assert.ok(failureNotice(event(), memory, REPEAT_QUIET_MS));
});

test("only shown failures are remembered, and old ones are forgotten", () => {
  const memory: FailureMemory = new Map();
  failureNotice(event({ status: "pushed" }), memory, 0);
  assert.equal(memory.size, 0);
  failureNotice(event(), memory, 0);
  failureNotice(event({ pull_request: 9 }), memory, REPEAT_QUIET_MS + 1);
  assert.equal(memory.size, 1);
});
