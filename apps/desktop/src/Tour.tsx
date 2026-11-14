// The first-run tour: a card that walks through TOUR_STEPS (tourSteps.ts), pointing at each step's
// element with a highlight. It is a modal dialog: focus moves to the card and stays inside it,
// Right Arrow (or Enter on the focused card) goes on, Left Arrow goes back, Esc skips, and each
// step after the first is announced to screen readers. Steps whose element is not on screen are
// skipped, and the highlight follows the element as the window scrolls or the layout changes.
// While another dialog is open (the palette, an approval or confirmation, Settings) the tour
// steps aside, hidden and not handling keys, and comes back when that closes: it never blocks one.
// It makes no network requests.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { cardPosition, nextStep, previousStep, settleStep, TOUR_STEPS, type Box, type TourOutcome, type TourStep } from "./tourSteps";

const exists = (selector: string): boolean => {
  try {
    const element = document.querySelector<HTMLElement>(selector);
    return !!element && element.getClientRects().length > 0;
  } catch {
    return false;
  }
};

/** Dialogs other than the tour's own card: modal sheets, the (native) palette, confirmations. */
const OTHER_DIALOGS = '[aria-modal="true"]:not(.tour-card), dialog[open], [role="alertdialog"]';

const FOCUSABLE = "button:not(:disabled), [href], input, select, textarea, [tabindex]:not([tabindex='-1'])";

export function Tour({ steps = TOUR_STEPS, onEnd }: { steps?: TourStep[]; onEnd: (outcome: TourOutcome) => void }) {
  const [currentId, setCurrentId] = useState(steps[0]?.id ?? "");
  const [box, setBox] = useState<Box | null>(null);
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null);
  const [paused, setPaused] = useState(() => !!document.querySelector(OTHER_DIALOGS));
  const [announcement, setAnnouncement] = useState("");
  // Bumped when the page changed in a way that can change which steps are available.
  const [, setTick] = useState(0);
  const card = useRef<HTMLDivElement>(null);
  // Where focus was before the tour opened, captured on the first render, before the card takes it.
  const before = useRef<HTMLElement | null>(document.activeElement as HTMLElement | null);
  const end = useRef(onEnd);
  end.current = onEnd;
  const pausedRef = useRef(paused);
  pausedRef.current = paused;

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

  // Step aside while another dialog is open, and come back when it closes.
  useEffect(() => {
    const check = () => setPaused(!!document.querySelector(OTHER_DIALOGS));
    const observer = new MutationObserver(check);
    observer.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["open", "aria-modal", "role"] });
    return () => observer.disconnect();
  }, []);

  // Point at the step's element and keep the highlight on it: as the window resizes, any
  // container scrolls, the element resizes, or the layout changes. If the element goes away,
  // the step is dropped (the next available one is shown).
  useLayoutEffect(() => {
    if (!step) return;
    const element = step.target ? document.querySelector<HTMLElement>(step.target) : null;
    element?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    const measure = () => {
      const rect = element?.getBoundingClientRect();
      const next = rect && rect.width > 0 && element?.isConnected ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height } : null;
      setBox(next);
      const size = card.current?.getBoundingClientRect();
      setPosition(cardPosition(next, { width: size?.width ?? 360, height: size?.height ?? 200 }, { width: window.innerWidth, height: window.innerHeight }));
    };
    let frame = 0;
    const refresh = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (step.target && !exists(step.target)) setTick((tick) => tick + 1);
        else measure();
      });
    };
    measure();
    window.addEventListener("resize", refresh);
    window.addEventListener("scroll", refresh, true);
    const resize = element && typeof ResizeObserver !== "undefined" ? new ResizeObserver(refresh) : null;
    if (element) resize?.observe(element);
    const mutations = step.target ? new MutationObserver(refresh) : null;
    mutations?.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["class", "style", "hidden"] });
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("resize", refresh);
      window.removeEventListener("scroll", refresh, true);
      resize?.disconnect();
      mutations?.disconnect();
    };
  }, [step]);

  // Focus the card on each step (and again when the tour comes back from another dialog). The
  // first step is read from the dialog's own name and description; later ones are announced below.
  const first = useRef(true);
  useEffect(() => {
    if (paused) return;
    card.current?.focus();
  }, [step?.id, paused]);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    if (step) setAnnouncement(`Step ${index + 1} of ${total}: ${step.title}. ${step.body}`);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [step?.id]);

  // Keys: the tour is modal, so it handles them before the rest of the app sees them, except
  // while another dialog is open, which keeps its own keys.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (pausedRef.current) return;
      const consume = () => {
        event.preventDefault();
        event.stopPropagation();
      };
      if (event.key === "Escape") {
        consume();
        end.current("skipped");
      } else if (event.key === "ArrowRight") {
        consume();
        go(1);
      } else if (event.key === "ArrowLeft") {
        consume();
        go(-1);
      } else if (event.key === "Enter" && document.activeElement === card.current) {
        consume();
        go(1);
      } else if (event.key === "Tab" && card.current) {
        const items = [...card.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
        if (items.length === 0) return;
        const firstItem = items[0];
        const lastItem = items[items.length - 1];
        const active = document.activeElement;
        if (!card.current.contains(active) || (event.shiftKey && (active === firstItem || active === card.current))) {
          event.preventDefault();
          (event.shiftKey ? lastItem : firstItem).focus();
        } else if (!event.shiftKey && active === lastItem) {
          event.preventDefault();
          firstItem.focus();
        }
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [go]);

  // Hand focus back to where it was when the tour closes.
  useEffect(() => {
    const previous = before.current;
    return () => previous?.focus?.();
  }, []);

  if (!step) return null;
  return createPortal(
    <div className="tour-backdrop" hidden={paused} onMouseDown={(event) => event.preventDefault()}>
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
      </div>
      <span className="tour-sr" role="status" aria-live="polite">
        {announcement}
      </span>
    </div>,
    document.body,
  );
}
