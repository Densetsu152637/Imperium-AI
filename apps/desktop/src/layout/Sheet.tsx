// A sheet over the workspace: where a column the window has no room for opens (fold.ts). The sidebar
// slides in from the left, a side panel from the right; below 600 px a side panel fills the window.
// Behind it a scrim dims the workspace, and the title bar stays in reach, so the button that opened
// the sheet can close it. It behaves like Dialog (components/Dialog.tsx), which is centred and
// titled and so does not fit a drawer: focus moves in and Tab wraps inside, Esc or a click on the
// scrim closes, and focus goes back to the element that opened it. A menu or dialog opened from
// inside the sheet takes the Esc first.

import { X } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent, type ReactNode, type RefObject } from "react";

import { IconButton } from "../components/IconButton";
import { MODAL_SELECTOR } from "../tour/tourSteps";

const FOCUSABLE = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex]:not([tabindex="-1"])';

/** What Tab can reach inside `root`: enabled and visible. */
function focusable(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (element) => !element.matches(":disabled") && element.tabIndex >= 0 && element.getClientRects().length > 0,
  );
}

export function Sheet({
  label,
  heading,
  side,
  full,
  onClose,
  returnFocus,
  onClick,
  children,
}: {
  /** The sheet's accessible name ("Sidebar", the panel's title). */
  label: string;
  /** The words in its top bar, beside the close button. */
  heading: string;
  /** The edge it opens from. */
  side: "left" | "right";
  /** It fills the window's width (a side panel below 600 px); otherwise a drawer with the scrim beside it. */
  full?: boolean;
  onClose: () => void;
  /** Where focus goes back to when the sheet closes, for a trigger that may not hold focus when clicked (macOS buttons); else whatever had focus when it opened. */
  returnFocus?: RefObject<HTMLElement | null>;
  /** Clicks inside the content, for a sheet that closes on a choice (the sidebar's rows). */
  onClick?: (event: MouseEvent<HTMLElement>) => void;
  children: ReactNode;
}) {
  const close = useRef(onClose);
  close.current = onClose;
  const root = useRef<HTMLDivElement>(null);
  // Read while rendering, before anything in the sheet takes focus.
  const [opener] = useState(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.defaultPrevented) return;
      // A dialog above the sheet (Settings, opened from a panel's menu) closes first.
      if ([...document.querySelectorAll(MODAL_SELECTOR)].some((element) => !root.current?.contains(element))) return;
      event.preventDefault();
      close.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Focus moves in, and back to the opener when the sheet goes (unless that is gone too).
  useEffect(() => {
    const element = root.current;
    if (element && !element.contains(document.activeElement)) {
      const reachable = focusable(element);
      // The first control after the close button is the content's own.
      (reachable[1] ?? reachable[0] ?? element).focus();
    }
    return () => {
      const target = returnFocus?.current ?? opener;
      if (target?.isConnected) target.focus();
    };
  }, [opener, returnFocus]);

  const trapTab = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const element = event.currentTarget;
    // A menu rendered into the body is not inside; it handles its own Tab.
    if (event.key !== "Tab" || !element.contains(event.target as Node)) return;
    const reachable = focusable(element);
    if (reachable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = reachable[0];
    const last = reachable[reachable.length - 1];
    if (event.shiftKey && (document.activeElement === first || document.activeElement === element)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return (
    <div className={`sheet sheet-${side}${full ? " sheet-full" : ""}`}>
      {/* A click, not a press: the press would move focus to the page after the sheet had given it back. */}
      <div className="sheet-scrim" onClick={onClose} aria-hidden="true" />
      <div ref={root} className="sheet-drawer" role="dialog" aria-modal="true" aria-label={label} tabIndex={-1} onKeyDown={trapTab} onClick={onClick}>
        <div className="sheet-bar">
          <span className="sheet-heading">{heading}</span>
          <IconButton icon={X} size="sm" className="sheet-close" label={`Close ${label.toLowerCase()}`} tooltip="Close (Esc)" onClick={onClose} />
        </div>
        <div className="sheet-content">{children}</div>
      </div>
    </div>
  );
}
