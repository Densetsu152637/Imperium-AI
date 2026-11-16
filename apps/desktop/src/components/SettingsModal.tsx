// The shell shared by Settings and the model configuration editor: a centred sheet over a dimmed
// workspace, with a navigation column on the left and, on the right, a header with the title and a
// close button, an optional notice, the scrolling content, and an optional footer for actions.
// Esc or a click on the backdrop closes it. Rendered into the body, above the workspace's own
// stacking layers.

import { X, type LucideIcon } from "lucide-react";
import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

export function SettingsModal({
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
  /** The navigation column: a search field and `ModalNavGroup`s. */
  nav: ReactNode;
  /** Shown between the header and the content (a restart notice, an error). */
  notice?: ReactNode;
  /** Actions under the content (Cancel, Apply, Save); none for settings that save as they change. */
  footer?: ReactNode;
  onClose: () => void;
  /** Keys the modal handles itself (Ctrl+S); runs before Esc closes, and can prevent it. */
  onKeyDown?: (event: KeyboardEvent) => void;
  children: ReactNode;
}) {
  // Kept in refs so the window listener is added once and always calls the latest handlers.
  const close = useRef(onClose);
  const keys = useRef(onKeyDown);
  close.current = onClose;
  keys.current = onKeyDown;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      keys.current?.(event);
      if (event.key === "Escape" && !event.defaultPrevented) close.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return createPortal(
    <div className="settings-backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className="settings-window" role="dialog" aria-modal="true" aria-label={label}>
        <nav className="settings-nav" aria-label={`${label} sections`}>
          {nav}
        </nav>
        <section className="settings-content">
          <header className="settings-content-header">
            <h2>{title}</h2>
            <button className="ghost icon-only" onClick={onClose} aria-label={`Close ${label.toLowerCase()}`} title="Close (Esc)">
              <X />
            </button>
          </header>
          {notice}
          <div className="settings-rows">{children}</div>
          {footer && <footer className="settings-footer">{footer}</footer>}
        </section>
      </div>
    </div>,
    document.body,
  );
}

/** A labelled group of navigation items. */
export function ModalNavGroup({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="settings-group">
      <span className="settings-group-label">{label}</span>
      {children}
    </div>
  );
}

/** One navigation item: an icon, its label, and optional badges under the label. */
export function ModalNavItem({
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
      className={`settings-nav-item${active ? " active" : ""}${dimmed ? " dimmed" : ""}`}
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      title={title}
    >
      <Icon aria-hidden="true" />
      {badges ? (
        <span className="settings-nav-item-text">
          <span className="settings-nav-item-label">{children}</span>
          <span className="settings-nav-badges">{badges}</span>
        </span>
      ) : (
        children
      )}
    </button>
  );
}

/** Every word of the query appears in the texts (case-insensitive): the modals' search fields. */
export function matches(query: string, ...texts: string[]): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const haystack = texts.join(" ").toLowerCase();
  return words.every((word) => haystack.includes(word));
}
