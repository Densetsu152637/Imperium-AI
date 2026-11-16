// One row of the Settings window: the title and description on the left, left-justified, and the
// control aligned right. A sub-setting (one that depends on the row above it) is indented by the
// same step everywhere, and a note row spans the width under its setting.

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
  /** Indents the row under the setting it belongs to. */
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
