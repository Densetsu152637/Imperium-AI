// The app's dialog: a centred sheet over a dimmed workspace with an optional navigation column on the
// left and, on the right, a header with the title and a close button, an optional notice, the
// scrolling body, and an optional footer for actions. Settings, the environment window and the model
// configuration editor use it. It traps focus (Tab and Shift+Tab wrap inside), closes on Esc or a
// click on the scrim, and gives focus back to the element that opened it. Below 600 px wide it is a
// full-screen sheet (dialog.css). Rendered into the body, above the workspace's own stacking layers.
//
// One left edge: the header, the notice, the body and the footer share the gutter `--dialog-gutter`,
// so a `Field` or any other content starts at the same x as the title.

import { X, type LucideIcon } from "lucide-react";
import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";

import { IconButton } from "./IconButton";

const FOCUSABLE = 'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex]:not([tabindex="-1"])';

/** What Tab can reach inside `root`: enabled and visible. */
function focusable(root: HTMLElement): HTMLElement[] {
  return [...root.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
    (element) => !element.matches(":disabled") && element.tabIndex >= 0 && element.getClientRects().length > 0,
  );
}

export function Dialog({
  label,
  title,
  nav,
  notice,
  footer,
  onClose,
  onKeyDown,
  children,
}: {
  /** The dialog's accessible name ("Settings", "Models"). */
  label: string;
  /** The heading over the content: usually the selected section. */
  title: ReactNode;
  /** The navigation column: a search field and `DialogNavGroup`s. Without it the dialog is one column. */
  nav?: ReactNode;
  /** Shown between the header and the content (a restart notice, an error). */
  notice?: ReactNode;
  /** Actions under the content (Cancel, Apply, Save); none for settings that save as they change. */
  footer?: ReactNode;
  onClose: () => void;
  /** Keys the dialog handles itself (Ctrl+S); runs before Esc closes, and can prevent it. */
  onKeyDown?: (event: KeyboardEvent) => void;
  children: ReactNode;
}) {
  // Kept in refs so the window listener is added once and always calls the latest handlers.
  const close = useRef(onClose);
  const keys = useRef(onKeyDown);
  close.current = onClose;
  keys.current = onKeyDown;
  const panel = useRef<HTMLDivElement>(null);
  // Read while rendering, before anything in the dialog (an autofocused field) takes focus.
  const [opener] = useState(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      keys.current?.(event);
      if (event.key === "Escape" && !event.defaultPrevented) close.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // Focus moves in, and back to the opener when the dialog goes (unless that is gone too).
  useEffect(() => {
    const root = panel.current;
    if (root && !root.contains(document.activeElement)) (focusable(root)[0] ?? root).focus();
    return () => {
      if (opener?.isConnected) opener.focus();
    };
  }, [opener]);

  const trapTab = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const root = event.currentTarget;
    // A popover rendered into the body (a Select's list) is not inside; it handles its own Tab.
    if (event.key !== "Tab" || !root.contains(event.target as Node)) return;
    const reachable = focusable(root);
    if (reachable.length === 0) {
      event.preventDefault();
      return;
    }
    const first = reachable[0];
    const last = reachable[reachable.length - 1];
    if (event.shiftKey && (document.activeElement === first || document.activeElement === root)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  return createPortal(
    <div className="settings-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div ref={panel} className={`settings-window${nav ? "" : " no-nav"}`} role="dialog" aria-modal="true" aria-label={label} tabIndex={-1} onKeyDown={trapTab}>
        {nav && <DialogNav label={`${label} sections`}>{nav}</DialogNav>}
        <section className="settings-content">
          <header className="settings-content-header">
            <h2>{title}</h2>
            <IconButton icon={X} label={`Close ${label.toLowerCase()}`} tooltip="Close (Esc)" onClick={onClose} />
          </header>
          {notice}
          <div className="settings-rows dialog-body">{children}</div>
          {footer && <footer className="settings-footer">{footer}</footer>}
        </section>
      </div>
    </div>,
    document.body,
  );
}

/** The navigation column of a dialog: Dialog draws it from its `nav` prop. */
export function DialogNav({ label, children }: { label: string; children: ReactNode }) {
  return (
    <nav className="settings-nav" aria-label={label}>
      {children}
    </nav>
  );
}

/** A labelled group of navigation items. */
export function DialogNavGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="settings-group">
      <span className="settings-group-label">{label}</span>
      {children}
    </div>
  );
}

/** One navigation item: an icon, its label, and optional badges under the label. Every item is one row tall (two with badges), with its icon in a fixed column. */
export function DialogNavItem({
  icon: Icon,
  active,
  dimmed = false,
  onClick,
  badges,
  title,
  children,
}: {
  icon: LucideIcon;
  active: boolean;
  /** Shown faded, such as a hidden model. */
  dimmed?: boolean;
  onClick: () => void;
  badges?: ReactNode;
  title?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className={`settings-nav-item${active ? " active" : ""}${dimmed ? " dimmed" : ""}${badges ? " has-badges" : ""}`}
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      title={title}
    >
      <Icon aria-hidden="true" />
      <span className="settings-nav-item-text">
        <span className="settings-nav-item-label">{children}</span>
        {badges && <span className="settings-nav-badges">{badges}</span>}
      </span>
    </button>
  );
}

/** Every word of the query appears in the texts (case-insensitive): the dialogs' search fields. */
export function matches(query: string, ...texts: string[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}
