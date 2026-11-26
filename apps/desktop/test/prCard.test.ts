// The pull-request card's view-model (sessions/prCard.ts) over every PrStatus combination, and over
// text a stranger wrote on GitHub: markup, Markdown and direction controls must come out inert.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import type { PrStatus } from "@imperium/protocol";

import { autoMergeSettings, chooseMethod, describePr, isFirstRead, nextStatus, oneLine, parseBindInput, refOfUrl, safeLink } from "../src/sessions/prCard.ts";

const status = (over: Partial<PrStatus> = {}): PrStatus => ({
  url: "https://github.com/owner/repo/pull/7",
  repo: "owner/repo",
  number: 7,
  title: "Add the thing",
  state: "open",
  draft: false,
  head_sha: "abc123",
  base: "main",
  branch: "feature",
  checks: { passed: 4, failed: 0, pending: 0, failing_names: [] },
  review: "approved",
  mergeable: "mergeable",
  merge_state: "clean",
  allowed_methods: ["squash", "merge", "rebase"],
  auto_fix: false,
  auto_merge: { enabled: false },
  auto_archive_on_close: false,
  untrusted_handled: false,
  ignored_comments: 0,
  updated_at: 1_700_000_000_000,
  ...over,
});

test("an open, passing, approved pull request", () => {
  const view = describePr(status());
  assert.equal(view.title, "Add the thing");
  assert.equal(view.ref, "owner/repo#7");
  assert.equal(view.link, "https://github.com/owner/repo/pull/7");
  assert.equal(view.branches, "feature into main");
  assert.deepEqual(view.state, { label: "Open", tone: "ok", icon: "open" });
  assert.equal(view.checks.label, "All 4 passing");
  assert.equal(view.checks.ok, true);
  assert.equal(view.review.label, "Approved");
  assert.equal(view.merge.label, "No conflicts");
  assert.equal(view.mergeNote, null);
  assert.equal(view.ended, false);
  assert.equal(view.error, null);
});

test("a draft shows as a draft and says GitHub refuses auto-merge on it", () => {
  for (const view of [describePr(status({ state: "draft", draft: true })), describePr(status({ state: "open", draft: true }))]) {
    assert.equal(view.state.label, "Draft");
    assert.equal(view.state.icon, "draft");
    assert.match(view.autoMerge.hint ?? "", /does not allow auto-merge on a draft/);
  }
});

test("failing checks list their names, capped, with the rest counted", () => {
  const names = ["lint", "build", "unit", "e2e", "audit", "docs", "deploy"];
  const view = describePr(status({ checks: { passed: 3, failed: 7, pending: 1, failing_names: names } }));
  assert.equal(view.checks.label, "7 failing");
  assert.equal(view.checks.detail, "3 passed, 7 failed, 1 running");
  assert.equal(view.checks.tone, "danger");
  assert.equal(view.checks.ok, false);
  assert.deepEqual(view.checks.failing, names.slice(0, 5));
  assert.equal(view.checks.more, 2);
});

test("running and absent checks", () => {
  const running = describePr(status({ checks: { passed: 2, failed: 0, pending: 3, failing_names: [] } }));
  assert.equal(running.checks.label, "3 running");
  assert.equal(running.checks.ok, null);
  const none = describePr(status({ checks: { passed: 0, failed: 0, pending: 0, failing_names: [] } }));
  assert.equal(none.checks.label, "No checks");
  assert.equal(none.checks.ok, undefined);
});

test("review and conflict states", () => {
  assert.deepEqual(describePr(status({ review: "changes_requested" })).review, { label: "Changes requested", tone: "danger" });
  assert.equal(describePr(status({ review: "review_required" })).review.label, "Review required");
  assert.equal(describePr(status({ review: "none" })).review.label, "No review yet");
  const conflicting = describePr(status({ mergeable: "conflicting", merge_state: "dirty" }));
  assert.deepEqual(conflicting.merge, { label: "Has conflicts", tone: "danger" });
  assert.equal(conflicting.mergeNote, null);
  assert.equal(describePr(status({ mergeable: "unknown" })).merge.label, "Checking for conflicts");
  assert.equal(describePr(status({ merge_state: "behind" })).mergeNote, "Behind its base branch");
  assert.equal(describePr(status({ merge_state: "queued" })).mergeNote, "In the merge queue");
});

test("merged and closed are ended, with the reason beside auto-merge", () => {
  const merged = describePr(status({ state: "merged" }));
  assert.equal(merged.ended, true);
  assert.equal(merged.state.label, "Merged");
  assert.match(merged.autoMerge.hint ?? "", /already merged/);
  const closed = describePr(status({ state: "closed" }));
  assert.equal(closed.ended, true);
  assert.equal(closed.state.tone, "danger");
  assert.match(closed.autoMerge.hint ?? "", /closed/);
});

test("an error keeps the last good data and shows the reason", () => {
  const view = describePr(status({ error: "GitHub is rate limiting this token.‮" }));
  assert.equal(view.error, "GitHub is rate limiting this token.");
  assert.equal(view.title, "Add the thing");
  assert.equal(view.checks.label, "All 4 passing");
});

test("auto-fix is offered and says every edit asks; auto-merge is offered and says it lands code unseen", () => {
  const view = describePr(status({ auto_fix: false }));
  assert.match(view.autoFix.reason, /every edit and command asking you first/);
  assert.equal(view.autoFix.canTurnOn, true);
  assert.equal(view.autoFix.canTurnOff, false);
  assert.equal(view.autoFix.on, false);
  assert.equal(view.untrusted, null);
  assert.equal(view.ignored, null);
  const on = describePr(status({ auto_fix: true }));
  assert.equal(on.autoFix.canTurnOn, false);
  assert.equal(on.autoFix.canTurnOff, true);
  assert.equal(describePr(status({ state: "merged" })).autoFix.canTurnOn, false);
  assert.equal(view.autoMerge.on, false);
  assert.equal(view.autoMerge.hint, null);
  assert.equal(view.autoMerge.canTurnOn, true);
  assert.equal(view.autoMerge.canTurnOff, false);
  assert.match(view.autoMerge.reason, /nobody looking at the code again/);
  assert.match(view.autoMerge.reason, /never merges it itself/);
  // A repository's own reason is shown beside it, and the switch cannot be turned on.
  const refused = describePr(status({ auto_merge: { enabled: false, reason_unavailable: "The repository does not allow auto-merge." } }));
  assert.equal(refused.autoMerge.hint, "The repository does not allow auto-merge.");
  assert.equal(refused.autoMerge.canTurnOn, false);
});

test("auto-merge cannot be turned on for a draft, an ended pull request, unreadable methods or when the settings forbid it", () => {
  for (const hinted of [
    describePr(status({ state: "draft", draft: true })),
    describePr(status({ state: "merged" })),
    describePr(status({ state: "closed" })),
    describePr(status({ allowed_methods: [] })),
    describePr(status(), { allowed: false, preferred: null }),
  ]) {
    assert.equal(hinted.autoMerge.canTurnOn, false);
    assert.ok(hinted.autoMerge.hint);
  }
  assert.match(describePr(status({ allowed_methods: [] })).autoMerge.hint ?? "", /could not be read/);
  assert.match(describePr(status(), { allowed: false, preferred: null }).autoMerge.hint ?? "", /turned off in your settings/);
});

test("an auto-merge request that is on can be turned off, and says GitHub merges", () => {
  const view = describePr(status({ auto_merge: { enabled: true, method: "squash" } }));
  assert.equal(view.autoMerge.on, true);
  assert.equal(view.autoMerge.canTurnOn, false);
  assert.equal(view.autoMerge.canTurnOff, true);
  assert.match(view.autoMerge.reason, /GitHub merges it/);
  assert.equal(view.method, "squash");
  // Once the pull request is merged there is nothing left to turn off.
  assert.equal(describePr(status({ state: "merged", auto_merge: { enabled: true, method: "squash" } })).autoMerge.canTurnOff, false);
  // The setting that forbids turning it on does not stop turning it off.
  assert.equal(describePr(status({ auto_merge: { enabled: true, method: "squash" } }), { allowed: false, preferred: null }).autoMerge.canTurnOff, true);
});

test("the preselected method is the preference when allowed, else the first allowed of squash, merge, rebase", () => {
  assert.equal(chooseMethod(["squash", "merge", "rebase"], null), "squash");
  assert.equal(chooseMethod(["merge", "rebase"], null), "merge");
  assert.equal(chooseMethod(["squash", "merge", "rebase"], "rebase"), "rebase");
  assert.equal(chooseMethod(["squash", "merge"], "rebase"), "squash");
  assert.equal(chooseMethod([], "squash"), null);
  assert.equal(describePr(status(), { allowed: true, preferred: "merge" }).method, "merge");
  assert.equal(describePr(status({ allowed_methods: ["squash", "merge"] }), { allowed: true, preferred: "rebase" }).method, "squash");
});

test("the settings come from the daemon's entries, and anything odd is the default", () => {
  assert.deepEqual(autoMergeSettings([]), { allowed: true, preferred: null });
  assert.deepEqual(
    autoMergeSettings([
      { key: "github.automerge.allowed", value: false },
      { key: "github.automerge.default_method", value: "rebase" },
    ]),
    { allowed: false, preferred: "rebase" },
  );
  assert.deepEqual(autoMergeSettings([{ key: "github.automerge.default_method", value: "fast-forward" }]), { allowed: true, preferred: null });
});

test("the method picker offers only the allowed methods", () => {
  const view = describePr(status({ allowed_methods: ["squash", "rebase"] }));
  assert.deepEqual(
    view.methods.map((method) => method.value),
    ["squash", "rebase"],
  );
  assert.equal(view.method, "squash");
  assert.equal(view.methodNote, null);
});

test("a requested method the repository does not allow is flagged, and unreadable methods are said", () => {
  const notAllowed = describePr(status({ allowed_methods: ["squash"], auto_merge: { enabled: false, method: "merge" } }));
  assert.equal(notAllowed.method, "merge");
  assert.match(notAllowed.methodNote ?? "", /not allowed in this repository/);
  assert.deepEqual(
    notAllowed.methods.map((method) => method.value),
    ["squash"],
  );
  const unknown = describePr(status({ allowed_methods: [] }));
  assert.equal(unknown.method, null);
  assert.match(unknown.methodNote ?? "", /could not be read/);
});

test("hostile titles and names come out as plain text", () => {
  const hostile = status({
    title: "<img src=x onerror=alert(1)> [click](javascript:alert(1)) **bold**‮evil⁦​\u0007\n\nsecond line",
    branch: "<script>alert(1)</script>",
    checks: { passed: 0, failed: 1, pending: 0, failing_names: ["lint‮<b>x</b>\r\n![i](http://evil/p.png)"] },
  });
  const view = describePr(hostile);
  // Markup and Markdown stay as literal characters (React draws them as text); direction and control characters are gone.
  assert.equal(view.title, "<img src=x onerror=alert(1)> [click](javascript:alert(1)) **bold**evil second line");
  assert.equal(view.branches, "<script>alert(1)</script> into main");
  assert.deepEqual(view.checks.failing, ["lint<b>x</b> ![i](http://evil/p.png)"]);
  for (const text of [view.title, view.branches, ...view.checks.failing]) {
    assert.doesNotMatch(text, /[\u0000-\u0008\u000b-\u001f\u007f-\u009f​-‏‪-‮⁦-⁩]/);
    assert.doesNotMatch(text, /\n/);
  }
});

test("only the daemon's own pull request address becomes a link", () => {
  assert.equal(safeLink("https://github.com/owner/repo/pull/7"), "https://github.com/owner/repo/pull/7");
  for (const url of [
    "javascript:alert(1)",
    "http://github.com/owner/repo/pull/7",
    "https://evil.example/owner/repo/pull/7",
    "https://github.com.evil.example/owner/repo/pull/7",
    "https://github.com/owner/repo/pull/7/../../x",
    "https://github.com/owner/repo/pull/7?x=1",
    "https://github.com/owner/repo/issues/7",
    "",
  ]) {
    assert.equal(safeLink(url), null, url);
  }
  assert.equal(describePr(status({ url: "javascript:alert(1)" })).link, null);
});

test("a title with only hidden characters is not blank", () => {
  assert.equal(describePr(status({ title: "​‮" })).title, "(untitled)");
  assert.equal(oneLine(null), "");
});

test("the link of an archived session's row", () => {
  assert.equal(refOfUrl("https://github.com/owner/repo/pull/7"), "owner/repo#7");
  assert.equal(refOfUrl("odd‮"), "odd");
});

test("a status answer never replaces a newer notification", () => {
  const old = status({ updated_at: 1_700_000_000_000 });
  const newer = status({ updated_at: 1_700_000_005_000, title: "Newer" });
  assert.equal(nextStatus(null, old, false), old);
  assert.equal(nextStatus(newer, old, false), newer);
  assert.equal(nextStatus(old, newer, false), newer);
  // A notification always wins, and a null one (an unbind) clears.
  assert.equal(nextStatus(newer, old, true), old);
  assert.equal(nextStatus(newer, null, true), null);
  assert.equal(nextStatus(newer, null, false), null);
});

test("a null notification only clears the shown status", () => {
  assert.equal(nextStatus(status(), null, true), null);
});

test("a binding awaiting its first read is not a failure", () => {
  assert.equal(isFirstRead("the pull request has not been read yet"), true);
  assert.equal(isFirstRead("the pull request could not be read: rate limited"), false);
});

test("the bind field accepts a pull request address and refuses everything else", () => {
  assert.deepEqual(parseBindInput(" https://github.com/owner/repo/pull/12/files#diff "), {
    ok: true,
    url: "https://github.com/owner/repo/pull/12",
  });
  assert.deepEqual(parseBindInput("https://github.com/owner/repo/pull/12?tab=checks"), { ok: true, url: "https://github.com/owner/repo/pull/12" });
  for (const text of [
    "",
    "   ",
    "owner/repo#12",
    "http://github.com/owner/repo/pull/12",
    "https://example.com/owner/repo/pull/12",
    "https://github.com/owner/repo/issues/12",
    "https://github.com/owner/repo/pull/abc",
    "javascript:alert(1)",
  ]) {
    assert.equal(parseBindInput(text).ok, false, text);
  }
});

test("the card draws GitHub text only as React text", () => {
  // The card is the only place these strings are drawn; it must never hand one to innerHTML,
  // a Markdown renderer or an href.
  const source = readFileSync(fileURLToPath(new URL("../src/panels/PullRequestCard.tsx", import.meta.url)), "utf8");
  assert.doesNotMatch(source, /dangerouslySetInnerHTML|innerHTML|ReactMarkdown|react-markdown/);
  assert.doesNotMatch(source, /href=/);
  assert.match(source, /openExternal\(view\.link!\)/);
});

test("the card says untrusted text was handled until it is acknowledged, and counts ignored comments", () => {
  const view = describePr(status({ auto_fix: true, untrusted_handled: true, ignored_comments: 2 }));
  assert.match(view.untrusted ?? "", /review text written by other people/);
  assert.equal(view.ignored, "2 comments from people without write access did not wake the session.");
  assert.equal(describePr(status({ ignored_comments: 1 })).ignored, "1 comment from people without write access did not wake the session.");
  assert.equal(describePr(status({ untrusted_handled: false })).untrusted, null);
});
