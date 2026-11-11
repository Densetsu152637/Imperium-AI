// Following a transcript while an agent writes, and free scroll away from it (src/follow.ts).
// Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  FOLLOWING,
  FOLLOW_THRESHOLD,
  atBottom,
  distanceFromBottom,
  jumpLabel,
  keyScrollsUp,
  nextFollow,
  touchScrollsUp,
  unseenCount,
  wheelScrollsUp,
  type FollowState,
} from "../src/follow.ts";

// A 400px view over 1000px of content: the bottom is at scrollTop 600.
const at = (scrollTop: number) => ({ scrollTop, scrollHeight: 1000, clientHeight: 400 });

test("the distance from the bottom is never negative", () => {
  assert.equal(distanceFromBottom(at(600)), 0);
  assert.equal(distanceFromBottom(at(500)), 100);
  // Overscroll (elastic scrolling) past the end.
  assert.equal(distanceFromBottom(at(640)), 0);
});

test("within the threshold of the end counts as at the bottom", () => {
  assert.equal(atBottom(at(600)), true);
  assert.equal(atBottom(at(600 - FOLLOW_THRESHOLD)), true);
  assert.equal(atBottom(at(600 - FOLLOW_THRESHOLD - 1)), false);
  assert.equal(atBottom(at(590), 5), false);
  // Content shorter than the view is always at the bottom.
  assert.equal(atBottom({ scrollTop: 0, scrollHeight: 200, clientHeight: 400 }), true);
});

test("upward wheel turns, keys, and touch drags are scrolls up", () => {
  assert.equal(wheelScrollsUp(-40), true);
  assert.equal(wheelScrollsUp(40), false);
  assert.equal(wheelScrollsUp(0), false);
  for (const key of ["ArrowUp", "PageUp", "Home"]) assert.equal(keyScrollsUp(key), true, key);
  for (const key of ["ArrowDown", "PageDown", "End", " ", "a"]) assert.equal(keyScrollsUp(key), false, key);
  assert.equal(keyScrollsUp(" ", true), true);
  // A finger moving down the screen scrolls the content up; a tap's jitter does not.
  assert.equal(touchScrollsUp(100, 130), true);
  assert.equal(touchScrollsUp(100, 101), false);
  assert.equal(touchScrollsUp(100, 60), false);
});

test("a scroll up stops following at once and remembers how much was shown", () => {
  const left = nextFollow(FOLLOWING, { type: "up", count: 7 });
  assert.deepEqual(left, { following: false, leftAt: 7, fresh: false });
  // Further scrolling up keeps the first mark.
  assert.equal(nextFollow(left, { type: "up", count: 9 }), left);
});

test("the view moving up without reaching the bottom stops following (a scroll bar drag)", () => {
  const state = nextFollow(FOLLOWING, { type: "scrolled", metrics: at(300), previousTop: 600, count: 4 });
  assert.deepEqual(state, { following: false, leftAt: 4, fresh: false });
});

test("growth and moves down while following keep following", () => {
  // Pinned to the end after growth: the view moved down.
  assert.equal(nextFollow(FOLLOWING, { type: "scrolled", metrics: at(600), previousTop: 500, count: 4 }), FOLLOWING);
  // A one-pixel rounding wobble is not a scroll up.
  assert.equal(nextFollow(FOLLOWING, { type: "scrolled", metrics: at(400), previousTop: 400.5, count: 4 }), FOLLOWING);
});

test("content growth while not following does not change where the reader is", () => {
  const left: FollowState = { following: false, leftAt: 3, fresh: false };
  // Content grew below the view; the browser keeps the place, so scrollTop moves down or not at all.
  assert.equal(nextFollow(left, { type: "scrolled", metrics: at(320), previousTop: 300, count: 5 }), left);
  // A tool group collapsing above the view moves it up, but it was already not following.
  assert.equal(nextFollow(left, { type: "scrolled", metrics: at(200), previousTop: 300, count: 5 }), left);
});

test("scrolling back to within the threshold of the bottom follows again", () => {
  const left: FollowState = { following: false, leftAt: 3, fresh: true };
  assert.equal(
    nextFollow(left, { type: "scrolled", metrics: at(600 - FOLLOW_THRESHOLD + 2), previousTop: 400, count: 6 }),
    FOLLOWING,
  );
  assert.equal(nextFollow(left, { type: "scrolled", metrics: at(500), previousTop: 400, count: 6 }), left);
});

test("new output while not following is marked; while following it is not", () => {
  assert.equal(nextFollow(FOLLOWING, { type: "output" }), FOLLOWING);
  const left: FollowState = { following: false, leftAt: 3, fresh: false };
  const fresh = nextFollow(left, { type: "output" });
  assert.deepEqual(fresh, { following: false, leftAt: 3, fresh: true });
  assert.equal(nextFollow(fresh, { type: "output" }), fresh);
});

test("Jump to latest, a sent message, or another transcript resumes following", () => {
  const left: FollowState = { following: false, leftAt: 3, fresh: true };
  assert.equal(nextFollow(left, { type: "resume" }), FOLLOWING);
  assert.equal(nextFollow(FOLLOWING, { type: "resume" }), FOLLOWING);
});

test("the button counts items added since following stopped", () => {
  assert.equal(unseenCount(FOLLOWING, 10), 0);
  assert.equal(unseenCount({ following: false, leftAt: 4, fresh: true }, 7), 3);
  // Items can merge into a tool group, so the count never goes below zero.
  assert.equal(unseenCount({ following: false, leftAt: 4, fresh: false }, 3), 0);
});

test("the button's name says what is new", () => {
  assert.equal(jumpLabel(0, false), "Jump to latest");
  assert.equal(jumpLabel(0, true), "Jump to latest (new output)");
  assert.equal(jumpLabel(2, true), "Jump to latest (2 new)");
});
