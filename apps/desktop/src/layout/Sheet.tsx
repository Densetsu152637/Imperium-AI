// A sheet over the workspace: where a column the window has no room for opens (fold.ts). The column
// is the same cell Columns keeps for it, moved to the window's edge (styles/shell.css, `.sheet-cell`),
// so its panels are never mounted twice and keep their state across opening and resizing. The
// sidebar slides in from the left, a side panel from the right; below 600 px a side panel fills the
// window. Behind it a scrim dims the workspace, and every other column is inert, so focus cannot
// reach what is dimmed; the title bar stays in reach, so the button that opened the sheet can close
// it. The sheet is not modal. It behaves like Dialog (components/Dialog.tsx), which is centred and
// titled and so does not fit a drawer: focus moves in, Esc or a click on the scrim closes, and focus
// goes back to the trigger. A menu or dialog opened from inside it takes the Esc first.

import { X } from "lucide-react";
import { useEffect, useRef, type RefObject } from "react";

import { IconButton } from "../components/IconButton";
import { MODAL_SELECTOR } from "../tour/tourSteps";

const FOCUSABLE = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex]:not([tabindex="-1"])';

/** What Tab can reach inside `root`: enabled and visible. */
function focusable(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (element) => !element.matches(":disabled") && element.tabIndex >= 0 && element.getClientRects().length > 0,
  );
}

/** The top bar of an open sheet: what it is, and its close button. */
export function SheetBar({ label, heading, onClose }: { label: string; heading: string; onClose: () => void }) {
  return (
    <div className="sheet-bar">
      <span className="sheet-heading">{heading}</span>
      <IconButton icon={X} size="sm" className="sheet-close" label={`Close ${label.toLowerCase()}`} tooltip="Close (Esc)" onClick={onClose} />
    </div>
  );
}

/** The dimmed workspace behind a sheet; a click on it closes the sheet. */
export function SheetScrim({ onClose }: { onClose: () => void }) {
  // A click, not a press: the press would move focus to the page after the sheet had given it back.
  return <div className="sheet-scrim" onClick={onClose} aria-hidden="true" />;
}

/**
 * The behaviour of an open sheet (the `[data-sheet]` cell Columns draws): focus moves in when it
 * opens, Esc closes it, and focus goes back to `returnFocus`, or whatever had it when the sheet
 * opened. When that trigger is gone (the window widened and took the button with it) focus goes to
 * the app root instead of falling to the page.
 */
export function useSheet(open: boolean, onClose: () => void, returnFocus?: RefObject<HTMLElement | null>): void {
  const close = useRef(onClose);
  close.current = onClose;
  useEffect(() => {
    if (!open) return;
    const root = document.querySelector<HTMLElement>("[data-sheet]");
    // Taken now: by the time the sheet closes the trigger's ref may already be cleared.
    const opener = returnFocus?.current ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null);
    if (root && !root.contains(document.activeElement)) {
      const reachable = focusable(root);
      // The first control after the close button is the content's own.
      (reachable[1] ?? reachable[0] ?? root).focus();
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      // A dialog above the sheet (Settings, opened from a panel's menu) closes first.
      if (document.querySelector(MODAL_SELECTOR)) return;
      event.preventDefault();
      close.current();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      const target = opener?.isConnected ? opener : document.querySelector<HTMLElement>(".app");
      target?.focus();
    };
  }, [open, returnFocus]);
}

/** `useSheet` for a place that cannot call a hook (App renders it after its early return for panel windows). */
export function SheetBehaviour({ onClose, returnFocus }: { onClose: () => void; returnFocus?: RefObject<HTMLElement | null> }) {
  useSheet(true, onClose, returnFocus);
  return null;
}
