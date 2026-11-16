// One row of the Settings window: the title and description, then the control under them, all on
// the dialog's one left edge. A sub-setting (one that depends on the row above it) sits closer to
// that row on the same edge, and a note row spans the width under its setting.

import type { ReactNode } from "react";

export function Row({
  label,
  description,
  children,
  badge,
  sub = false,
}: {
  label: ReactNode;
  description?: ReactNode;
  children?: ReactNode;
  badge?: ReactNode;
  /** Sits closer to the setting it depends on, the row above. */
  sub?: boolean;
}) {
  return (
    <div className={`settings-row${sub ? " sub" : ""}`}>
      <div className="settings-row-text">
        <span className="settings-row-label">
          {label} {badge}
        </span>
        {description && <span className="settings-row-description">{description}</span>}
      </div>
      {children !== undefined && <div className="settings-row-control">{children}</div>}
    </div>
  );
}

/** Full-width content under a setting (a status line, a log), indented like a sub-setting. */
export function RowNote({ children, sub = false }: { children: ReactNode; sub?: boolean }) {
  return <div className={`settings-row-note${sub ? " sub" : ""}`}>{children}</div>;
}
