// Which events notify, and how (src/attention.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { deliveryFor, downloadNotice, sessionNotice, updateNotice, type Attention } from "../src/attention.ts";
import type { AppSettings } from "../src/settings.ts";

// The notification fields of DEFAULT_SETTINGS (settings.ts imports React, so is not loaded here).
const defaults = {
  soundOnApproval: true,
  soundOnFinish: false,
  titleBadge: true,
  notifyOnlyInBackground: true,
  notifyApproval: true,
  notifyFinished: true,
  notifyFailed: true,
  notifyBudget: true,
  notifyDownload: true,
  notifyUpdate: true,
} as AppSettings;

const ALL: Attention[] = ["approval", "finished", "failed", "budget", "download", "update"];

test("every kind shows a system notification in the background by default", () => {
  for (const kind of ALL) assert.equal(deliveryFor(kind, defaults, false).system, true, kind);
});

test("no system notification while Imperium has focus, unless asked for", () => {
  for (const kind of ALL) assert.equal(deliveryFor(kind, defaults, true).system, false, kind);
  const always = { ...defaults, notifyOnlyInBackground: false };
  for (const kind of ALL) assert.equal(deliveryFor(kind, always, true).system, true, kind);
});

test("each kind follows its own switch", () => {
  assert.equal(deliveryFor("failed", { ...defaults, notifyFailed: false }, false).system, false);
  assert.equal(deliveryFor("finished", { ...defaults, notifyFailed: false }, false).system, true);
  assert.equal(deliveryFor("download", { ...defaults, notifyDownload: false }, false).system, false);
  assert.equal(deliveryFor("update", { ...defaults, notifyUpdate: false }, false).system, false);
  assert.equal(deliveryFor("budget", { ...defaults, notifyBudget: false }, false).system, false);
  assert.equal(deliveryFor("approval", { ...defaults, notifyApproval: false }, false).system, false);
});

test("sounds and the title badge keep their own settings", () => {
  assert.equal(deliveryFor("approval", defaults, true).sound, true);
  assert.equal(deliveryFor("finished", defaults, false).sound, false);
  assert.equal(deliveryFor("failed", { ...defaults, soundOnFinish: true }, false).sound, true);
  assert.equal(deliveryFor("download", { ...defaults, soundOnFinish: true }, false).sound, false);
  assert.equal(deliveryFor("approval", defaults, false).badge, true);
  assert.equal(deliveryFor("approval", defaults, true).badge, false);
  assert.equal(deliveryFor("update", defaults, false).badge, false);
});

test("session events that need the user notify; others do not", () => {
  const approval = sessionNotice("s1", "Fix CI", "worker-2", { type: "permission_requested", request_id: 1, tool: "shell", summary: "cargo test" });
  assert.deepEqual(approval, { kind: "approval", sessionId: "s1", title: "Fix CI needs approval", body: "cargo test" });
  const question = sessionNotice("s1", "Fix CI", "root", {
    type: "questions_asked",
    request_id: 2,
    questions: [{ question: "Which branch?", options: [], recommended: "" }],
  });
  assert.equal(question?.kind, "approval");
  assert.equal(question?.body, "Which branch?");
  assert.equal(sessionNotice("s1", "Fix CI", "root", { type: "turn_failed", message: "rate limited" })?.kind, "failed");
  assert.equal(sessionNotice("s1", "Fix CI", "worker-2", { type: "turn_failed", message: "x" }), null);
  assert.equal(sessionNotice("s1", "Fix CI", "root", { type: "text_delta", text: "hi" }), null);
  assert.equal(sessionNotice("s1", "Fix CI", "root", { type: "notice", message: "hi" }), null);
});

test("a root turn that completes notifies once as finished", () => {
  const notice = sessionNotice("s1", "Fix CI", "root", {
    type: "turn_completed",
    usage: { input_tokens: 1, output_tokens: 1, cached_tokens: 0 },
    cost_usd: null,
    steps: 1,
  } as never);
  assert.equal(notice?.kind, "finished");
  assert.equal(notice?.sessionId, "s1");
});

test("only finished downloads notify", () => {
  const job = { job_id: "j", model: "llamacpp:Qwen3-8B-Q4_K_M", state: "running" } as never;
  assert.equal(downloadNotice(job), null);
  assert.equal(downloadNotice({ ...(job as object), state: "done" } as never)?.kind, "download");
  assert.equal(downloadNotice({ ...(job as object), state: "failed" } as never), null);
});

test("an update notice names the version", () => {
  assert.match(updateNotice("0.2.0").body, /0\.2\.0/);
});
