// Follows a scrolling transcript as an agent writes, with free scroll (roadmap item 20). The
// session panel and the subagent view share it; the rules are in `follow.ts`.
//
// - While following, any growth (new entries, streamed text, a tool group opening, Markdown
//   re-rendering) keeps the view pinned to the bottom.
// - A scroll up by wheel, key, touch, or the scroll bar stops following at once, even mid-stream,
//   and growth no longer moves the view (the browser's scroll anchoring keeps the reader's place).
// - Scrolling back to within `FOLLOW_THRESHOLD` of the bottom, `jump()`, or `resume()` (a sent
//   message) follows again.

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

import {
  FOLLOWING,
  atBottom,
  keyScrollsUp,
  nextFollow,
  touchScrollsUp,
  unseenCount,
  wheelScrollsUp,
  type FollowEvent,
  type FollowState,
  type ScrollMetrics,
} from "./follow";

type Options = {
  /** Transcript items shown, for the count of new ones since following stopped. */
  count: number;
  /** Changes whenever new output arrives (an entry or streamed text). */
  activity: string | number;
  /** The *Follow new output* setting: off, the view never follows on its own. */
  enabled: boolean;
  /** Another transcript (session or subagent): follow it from its end. */
  resetKey: string;
};

export type FollowScroll = {
  /** Goes on the scrolling element. */
  ref: RefObject<HTMLDivElement | null>;
  /** Whether the Jump to latest button shows. */
  showJump: boolean;
  /** Items added since following stopped. */
  unseen: number;
  /** Whether output arrived since following stopped. */
  fresh: boolean;
  /** Scrolls to the end and follows again. */
  jump: () => void;
  /** Follows again from the next render (a sent message). */
  resume: () => void;
  /** Stops following before a programmatic scroll away from the end (a search hit). */
  release: () => void;
};

const metricsOf = (element: HTMLElement): ScrollMetrics => ({
  scrollTop: element.scrollTop,
  scrollHeight: element.scrollHeight,
  clientHeight: element.clientHeight,
});

const isEditable = (target: EventTarget | null) =>
  target instanceof HTMLElement &&
  (target.isContentEditable || target.closest("input, textarea, select, [role=listbox], [role=menu]") !== null);

export function useFollowScroll({ count, activity, enabled, resetKey }: Options): FollowScroll {
  const ref = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<FollowState>(FOLLOWING);
  const [bottom, setBottom] = useState(true);
  // Event handlers and observers read the latest values without re-subscribing.
  const follow = useRef(state);
  const live = useRef({ count, enabled });
  live.current = { count, enabled };
  const lastTop = useRef(0);
  const dragging = useRef(false);

  const dispatch = useCallback((event: FollowEvent) => {
    const next = nextFollow(follow.current, event);
    if (next === follow.current) return;
    follow.current = next;
    setState(next);
  }, []);

  const pinned = () => live.current.enabled && follow.current.following && !dragging.current;

  const snap = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    element.scrollTop = element.scrollHeight;
    lastTop.current = element.scrollTop;
    setBottom(true);
  }, []);

  const settle = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    if (pinned()) snap();
    else setBottom(atBottom(metricsOf(element)));
  }, [snap]);

  // Another transcript starts followed, at its end.
  useLayoutEffect(() => {
    dispatch({ type: "resume" });
    snap();
  }, [resetKey, dispatch, snap]);

  // After every render: pinned views stay at the end; others keep their place.
  useLayoutEffect(settle);

  // New output while not following lights the button's marker.
  const lastActivity = useRef(activity);
  useLayoutEffect(() => {
    if (lastActivity.current === activity) return;
    lastActivity.current = activity;
    dispatch({ type: "output" });
  }, [activity, dispatch]);

  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const up = () => dispatch({ type: "up", count: live.current.count });

    const onScroll = () => {
      const metrics = metricsOf(element);
      dispatch({ type: "scrolled", metrics, previousTop: lastTop.current, count: live.current.count });
      lastTop.current = metrics.scrollTop;
      setBottom(atBottom(metrics));
    };
    const onWheel = (event: WheelEvent) => {
      if (!event.ctrlKey && wheelScrollsUp(event.deltaY)) up();
    };
    const onKey = (event: KeyboardEvent) => {
      if (!isEditable(event.target) && keyScrollsUp(event.key, event.shiftKey)) up();
    };
    let touchY: number | null = null;
    const onTouchStart = (event: TouchEvent) => {
      touchY = event.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (event: TouchEvent) => {
      const y = event.touches[0]?.clientY;
      if (touchY !== null && y !== undefined && touchScrollsUp(touchY, y)) up();
    };
    // A press on the element itself (not its content) is on the scroll bar or the padding: while
    // it is held the view is not pinned, so a drag up is not pulled back down.
    const onPointerDown = (event: PointerEvent) => {
      if (event.target === element) dragging.current = true;
    };
    const onPointerUp = () => {
      if (!dragging.current) return;
      dragging.current = false;
      settle();
    };

    element.addEventListener("scroll", onScroll, { passive: true });
    element.addEventListener("wheel", onWheel, { passive: true });
    element.addEventListener("keydown", onKey);
    element.addEventListener("touchstart", onTouchStart, { passive: true });
    element.addEventListener("touchmove", onTouchMove, { passive: true });
    element.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);

    // Growth that does not re-render this component (a tool group opening, Markdown settling,
    // images loading) is seen by watching each item's size.
    const sizes = new ResizeObserver(settle);
    const watch = () => {
      sizes.disconnect();
      sizes.observe(element);
      for (const child of Array.from(element.children)) sizes.observe(child);
    };
    const children = new MutationObserver(watch);
    children.observe(element, { childList: true });
    watch();

    return () => {
      element.removeEventListener("scroll", onScroll);
      element.removeEventListener("wheel", onWheel);
      element.removeEventListener("keydown", onKey);
      element.removeEventListener("touchstart", onTouchStart);
      element.removeEventListener("touchmove", onTouchMove);
      element.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
      sizes.disconnect();
      children.disconnect();
    };
  }, [dispatch, settle]);

  const jump = useCallback(() => {
    dispatch({ type: "resume" });
    snap();
  }, [dispatch, snap]);
  const resume = useCallback(() => dispatch({ type: "resume" }), [dispatch]);
  const release = useCallback(() => dispatch({ type: "up", count: live.current.count }), [dispatch]);

  const following = enabled && state.following;
  return {
    ref,
    showJump: !following && !bottom,
    unseen: unseenCount(state, count),
    fresh: state.fresh,
    jump,
    resume,
    release,
  };
}
