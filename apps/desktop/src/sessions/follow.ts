// Following a transcript as an agent writes (roadmap item 20). While following, new output keeps
// the view at the bottom; any scroll up stops following and leaves the reader's place alone until
// they jump back or scroll to the bottom again. The rules live here as pure functions so they are
// tested without a DOM; `useFollowScroll` wires them to the conversation element.

/** How close to the bottom (in CSS pixels) still counts as at the bottom. */
export const FOLLOW_THRESHOLD = 32;

/** The scroll position of a scrolling element. */
export type ScrollMetrics = { scrollTop: number; scrollHeight: number; clientHeight: number };

/** Pixels between the bottom of the view and the end of the content (never negative). */
export function distanceFromBottom(metrics: ScrollMetrics): number {
  return Math.max(0, metrics.scrollHeight - metrics.clientHeight - metrics.scrollTop);
}

/** Whether the view is at the bottom, within `threshold` pixels. */
export function atBottom(metrics: ScrollMetrics, threshold = FOLLOW_THRESHOLD): boolean {
  return distanceFromBottom(metrics) <= threshold;
}

/** Keys that scroll a page up (Shift+Space included). */
const UP_KEYS = new Set(["ArrowUp", "PageUp", "Home"]);

/** Whether a key press scrolls the view up. */
export function keyScrollsUp(key: string, shiftKey = false): boolean {
  return UP_KEYS.has(key) || (shiftKey && key === " ");
}

/** Whether a wheel turn scrolls the view up. */
export function wheelScrollsUp(deltaY: number): boolean {
  return deltaY < 0;
}

/** Whether a touch drag scrolls the view up: the finger moving down the screen. */
export function touchScrollsUp(startY: number, currentY: number, slop = 2): boolean {
  return currentY - startY > slop;
}

/**
 * Whether following: `leftAt` is the transcript's item count when following stopped (null while
 * following), and `fresh` is set once new output arrives after that.
 */
export type FollowState = { following: boolean; leftAt: number | null; fresh: boolean };

export const FOLLOWING: FollowState = { following: true, leftAt: null, fresh: false };

export type FollowEvent =
  /** The reader scrolled up (wheel, key, touch, or dragging the scroll bar): stop following now. */
  | { type: "up"; count: number }
  /** The view scrolled: back at the bottom resumes following, moving up stops it. */
  | { type: "scrolled"; metrics: ScrollMetrics; previousTop: number; count: number; threshold?: number }
  /** The transcript gained output (an entry or streamed text). */
  | { type: "output" }
  /** Jump to latest, a sent message, or another transcript: follow again. */
  | { type: "resume" };

function leave(state: FollowState, count: number): FollowState {
  return state.following ? { following: false, leftAt: count, fresh: false } : state;
}

/** The next follow state after an event. */
export function nextFollow(state: FollowState, event: FollowEvent): FollowState {
  switch (event.type) {
    case "up":
      return leave(state, event.count);
    case "scrolled":
      if (atBottom(event.metrics, event.threshold)) return state.following ? state : FOLLOWING;
      // A view that moved up without reaching the bottom was scrolled by the reader (or to a
      // search hit); growth while following never moves it up, since it is pinned to the end.
      return event.metrics.scrollTop < event.previousTop - 1 ? leave(state, event.count) : state;
    case "output":
      return state.following || state.fresh ? state : { ...state, fresh: true };
    case "resume":
      return state.following ? state : FOLLOWING;
  }
}

/** Transcript items added since following stopped. */
export function unseenCount(state: FollowState, count: number): number {
  return state.leftAt === null ? 0 : Math.max(0, count - state.leftAt);
}

/** The Jump to latest button's accessible name. */
export function jumpLabel(unseen: number, fresh: boolean): string {
  if (unseen > 0) return `Jump to latest (${unseen} new)`;
  return fresh ? "Jump to latest (new output)" : "Jump to latest";
}
