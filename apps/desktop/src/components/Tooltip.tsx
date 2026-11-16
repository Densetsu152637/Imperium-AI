// A tooltip for one element (usually an icon button). It appears after the pointer rests on the element
// for 400 ms, and at once when the element takes keyboard focus; it goes on leaving, on a press and on
// Escape (which does not stop the key, so an open dialog still sees it). It is `role="tooltip"`, linked
// with aria-describedby while it shows, and positioned by shared/floating.ts. Hovering the tooltip
// itself keeps it open. The element is the only child: the wrapper adds no box to the layout.

import { cloneElement, useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type ReactElement, type ReactNode } from "react";
import { createPortal } from "react-dom";

import { placeTooltip } from "../shared/floating";

/** How long the pointer rests on an element before its tooltip shows. */
const HOVER_DELAY_MS = 400;
/** The grace period for moving from the element onto its tooltip. */
const LEAVE_DELAY_MS = 120;

export function Tooltip({
  text,
  describe = true,
  children,
}: {
  text: ReactNode;
  /** Link the tooltip to the element with aria-describedby. Off when the text only repeats the element's accessible name. */
  describe?: boolean;
  /** The one element the tooltip is for. */
  children: ReactElement<{ "aria-describedby"?: string }>;
}) {
  const id = useId();
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const anchor = useRef<HTMLSpanElement>(null);
  const bubble = useRef<HTMLDivElement>(null);
  const timer = useRef<number>(0);

  const clear = () => window.clearTimeout(timer.current);
  const hide = useCallback(() => {
    window.clearTimeout(timer.current);
    setOpen(false);
    setPosition(null);
  }, []);
  const showAfter = (ms: number) => {
    clear();
    timer.current = window.setTimeout(() => setOpen(true), ms);
  };
  const hideAfter = (ms: number) => {
    clear();
    timer.current = window.setTimeout(hide, ms);
  };

  useEffect(() => clear, []);

  // Measure the tooltip once it is in the document, then place it; until then it is invisible.
  useLayoutEffect(() => {
    if (!open) return;
    const target = anchor.current?.firstElementChild;
    const box = bubble.current;
    if (!target || !box) return;
    const placed = placeTooltip(target.getBoundingClientRect(), box.getBoundingClientRect(), { width: window.innerWidth, height: window.innerHeight });
    setPosition({ left: placed.left, top: placed.top });
  }, [open, text]);

  useEffect(() => {
    if (!open) return;
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && hide();
    document.addEventListener("keydown", onKey, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, [open, hide]);

  const described = open && describe ? [children.props["aria-describedby"], id].filter(Boolean).join(" ") : children.props["aria-describedby"];

  return (
    <>
      <span
        ref={anchor}
        className="tooltip-anchor"
        onPointerEnter={(event) => event.pointerType !== "touch" && showAfter(HOVER_DELAY_MS)}
        onPointerLeave={() => hideAfter(LEAVE_DELAY_MS)}
        onPointerDown={hide}
        // Focus from the keyboard shows it at once; focus from a click does not.
        onFocus={(event) => event.target instanceof Element && event.target.matches(":focus-visible") && (clear(), setOpen(true))}
        onBlur={hide}
      >
        {cloneElement(children, { "aria-describedby": described })}
      </span>
      {open &&
        createPortal(
          <div
            ref={bubble}
            id={id}
            role="tooltip"
            className="tooltip"
            style={position ? { left: position.left, top: position.top } : { left: 0, top: 0, visibility: "hidden" }}
            onPointerEnter={clear}
            onPointerLeave={() => hideAfter(LEAVE_DELAY_MS)}
          >
            {text}
          </div>,
          document.body,
        )}
    </>
  );
}
