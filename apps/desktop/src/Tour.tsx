// The first-run tour: a card that walks through TOUR_STEPS (tour.ts), pointing at each step's
// element with a highlight. It is a modal dialog: focus moves to the card and stays inside it,
// Right/Enter go on, Left goes back, Esc skips, and each step is announced to screen readers.
// Steps whose element is not on screen are skipped. It makes no network requests.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { cardPosition, nextStep, previousStep, settleStep, TOUR_STEPS, type Box, type TourOutcome, type TourStep } from "./tour";

const exists = (selector: string): boolean => {
  try {
    const element = document.querySelector<HTMLElement>(selector);
    return !!element && element.getClientRects().length > 0;
  } catch {
    return false;
  }
};

const FOCUSABLE = "button:not(:disabled), [href], input, select, textarea, [tabindex]:not([tabindex='-1'])";

export function Tour({ steps = TOUR_STEPS, onEnd }: { steps?: TourStep[]; onEnd: (outcome: TourOutcome) => void }) {
  const [currentId, setCurrentId] = useState(steps[0]?.id ?? "");
  const [box, setBox] = useState<Box | null>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const card = useRef<HTMLDivElement>(null);
  const end = useRef(onEnd);
  end.current = onEnd;

  const settled = settleStep(steps, currentId, exists);
  const step = settled?.steps[settled.index] ?? null;
  const total = settled?.steps.length ?? 0;
  const index = settled?.index ?? 0;
  const last = !step || !nextStep(steps, step.id, exists);

  // No step can be shown at all (nothing to point at and no general step): end quietly.
  useEffect(() => {
    if (!step) end.current("skipped");
  }, [step]);

  const go = useCallback(
    (direction: 1 | -1) => {
      if (!step) return;
      const target = direction === 1 ? nextStep(steps, step.id, exists) : previousStep(steps, step.id, exists);
      if (target) setCurrentId(target.id);
      else if (direction === 1) end.current("finished");
    },
    [steps, step],
  );

  // Point at the step's element, and keep the highlight on it as the window changes.
  useLayoutEffect(() => {
    if (!step) return;
    const element = step.target ? document.querySelector<HTMLElement>(step.target) : null;
    element?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    const measure = () => {
      const rect = element?.getBoundingClientRect();
      const next = rect && rect.width > 0 ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height } : null;
      setBox(next);
      const size = card.current?.getBoundingClientRect();
      setPosition(cardPosition(next, { width: size?.width ?? 360, height: size?.height ?? 200 }, { width: window.innerWidth, height: window.innerHeight }));
    };
    measure();
    window.addEventListener("resize", measure);
    const observer = element && typeof ResizeObserver !== "undefined" ? new ResizeObserver(measure) : null;
    if (element) observer?.observe(element);
    return () => {
      window.removeEventListener("resize", measure);
      observer?.disconnect();
    };
  }, [step]);

  // Focus the card on each step; its heading is read out with the dialog's name and description.
  useEffect(() => {
    card.current?.focus();
  }, [step?.id]);

  // Keys: the tour is modal, so it handles them before the rest of the app sees them.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        end.current("skipped");
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        event.stopPropagation();
        go(1);
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        event.stopPropagation();
        go(-1);
      } else if (event.key === "Tab" && card.current) {
        const items = [...card.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
        if (items.length === 0) return;
        const first = items[0];
        const lastItem = items[items.length - 1];
        const active = document.activeElement;
        if (!card.current.contains(active) || (event.shiftKey && (active === first || active === card.current))) {
          event.preventDefault();
          (event.shiftKey ? lastItem : first).focus();
        } else if (!event.shiftKey && active === lastItem) {
          event.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [go]);

  // Hand focus back to where it was when the tour closes.
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    return () => before?.focus?.();
  }, []);

  if (!step) return null;
  return createPortal(
    <div className="tour-backdrop" onMouseDown={(event) => event.preventDefault()}>
      {box ? (
        <div
          className="tour-spotlight"
          aria-hidden="true"
          style={{ left: box.left - 4, top: box.top - 4, width: box.width + 8, height: box.height + 8 }}
        />
      ) : (
        <div className="tour-dim" aria-hidden="true" />
      )}
      <div
        ref={card}
        className="tour-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="tour-title"
        aria-describedby="tour-body"
        tabIndex={-1}
        style={position ? { left: position.left, top: position.top } : { visibility: "hidden" }}
      >
        <p className="tour-progress muted">
          Step {index + 1} of {total}
        </p>
        <h2 id="tour-title">{step.title}</h2>
        <p id="tour-body">{step.body}</p>
        <div className="tour-actions">
          <button className="ghost" onClick={() => end.current("skipped")}>
            Skip tour
          </button>
          <span className="spacer" />
          <button onClick={() => go(-1)} disabled={index === 0}>
            Back
          </button>
          <button className="primary" onClick={() => go(1)}>
            {last ? "Finish" : "Next"}
          </button>
        </div>
        <span className="tour-sr" role="status" aria-live="polite">
          Step {index + 1} of {total}: {step.title}
        </span>
      </div>
    </div>,
    document.body,
  );
}
