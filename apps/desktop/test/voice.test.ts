// Push-to-talk (src/voice.ts): the state machine, the shortcuts, and where the words go. Run
// with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  MIN_RECORDING_MS,
  formatElapsed,
  insertTranscript,
  isPttPress,
  isPttRelease,
  pttStep,
  type PttState,
} from "../src/voice.ts";

const idle: PttState = { phase: "idle" };

test("hold to record, release to transcribe, then back to idle", () => {
  let step = pttStep(idle, { type: "press", at: 1000 });
  assert.deepEqual(step, { state: { phase: "recording", since: 1000 }, effect: "start" });
  // Key repeat or a second press while recording changes nothing.
  assert.equal(pttStep(step.state, { type: "press", at: 1100 }).effect, null);
  step = pttStep(step.state, { type: "release", at: 2500 });
  assert.deepEqual(step, { state: { phase: "transcribing" }, effect: "stop-and-transcribe" });
  // Pressing while the words are on their way does nothing.
  assert.equal(pttStep(step.state, { type: "press", at: 2600 }).effect, null);
  assert.deepEqual(pttStep(step.state, { type: "done" }).state, idle);
});

test("a tap is discarded and Esc cancels a recording", () => {
  const recording: PttState = { phase: "recording", since: 0 };
  assert.deepEqual(pttStep(recording, { type: "release", at: MIN_RECORDING_MS - 1 }), {
    state: idle,
    effect: "discard",
  });
  assert.deepEqual(pttStep(recording, { type: "cancel" }), { state: idle, effect: "discard" });
  // A release without a recording does nothing.
  assert.deepEqual(pttStep(idle, { type: "release", at: 5 }), { state: idle, effect: null });
});

test("a failure shows until dismissed or a new recording starts", () => {
  const failed = pttStep({ phase: "recording", since: 0 }, { type: "failed", message: "Microphone access was refused." });
  assert.deepEqual(failed, { state: { phase: "error", message: "Microphone access was refused." }, effect: "discard" });
  assert.deepEqual(pttStep(failed.state, { type: "dismiss" }).state, idle);
  assert.deepEqual(pttStep(failed.state, { type: "cancel" }).state, idle);
  assert.equal(pttStep(failed.state, { type: "press", at: 9 }).effect, "start");
  const afterTranscribing = pttStep({ phase: "transcribing" }, { type: "failed", message: "not set up" });
  assert.deepEqual(afterTranscribing, { state: { phase: "error", message: "not set up" }, effect: null });
});

const key = (init: Partial<KeyboardEvent>) => ({
  key: "",
  code: "",
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  shiftKey: false,
  ...init,
});

test("shortcuts match their keys only", () => {
  const ctrlSpace = key({ code: "Space", key: " ", ctrlKey: true });
  assert.equal(isPttPress(ctrlSpace, "Ctrl+Space"), true);
  assert.equal(isPttPress(key({ code: "Space", key: " ", metaKey: true }), "Ctrl+Space"), true, "Cmd on macOS");
  assert.equal(isPttPress(key({ code: "Space", key: " " }), "Ctrl+Space"), false, "plain Space types a space");
  assert.equal(isPttPress(key({ code: "Space", ctrlKey: true, shiftKey: true }), "Ctrl+Space"), false);
  assert.equal(isPttPress(key({ code: "Space", ctrlKey: true, shiftKey: true }), "Ctrl+Shift+Space"), true);
  assert.equal(isPttPress(key({ code: "Space", altKey: true }), "Alt+Space"), true);
  assert.equal(isPttPress(key({ key: "F8" }), "F8"), true);
  assert.equal(isPttPress(ctrlSpace, "off"), false);
  // Letting go of the key or its modifier ends the recording.
  assert.equal(isPttRelease(key({ code: "Space" }), "Ctrl+Space"), true);
  assert.equal(isPttRelease(key({ key: "Control" }), "Ctrl+Space"), true);
  assert.equal(isPttRelease(key({ key: "a", code: "KeyA" }), "Ctrl+Space"), false);
  assert.equal(isPttRelease(key({ key: "Alt" }), "Alt+Space"), true);
});

test("the transcript goes in at the caret with spaces around it", () => {
  assert.deepEqual(insertTranscript("", 0, " hello there "), { text: "hello there", caret: 11 });
  assert.deepEqual(insertTranscript("fix the", 7, "login bug"), { text: "fix the login bug", caret: 17 });
  assert.deepEqual(insertTranscript("please now", 6, "check"), { text: "please check now", caret: 12 });
  assert.deepEqual(insertTranscript("keep", 4, "   "), { text: "keep", caret: 4 });
  assert.equal(formatElapsed(65_400), "1:05");
});
