// What a view shows when it has nothing to list: an optional icon, a short title, one line of
// explanation and at most one action, centred.

import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

export function EmptyState({
  icon: Icon,
  title,
  body,
  action,
  className,
}: {
  icon?: LucideIcon;
  title: ReactNode;
  body?: ReactNode;
  /** At most one: a button or link. */
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div className={`empty-state${className ? ` ${className}` : ""}`}>
      {Icon && <Icon className="empty-state-icon" aria-hidden="true" />}
      <p className="empty-state-title">{title}</p>
      {body && <p className="empty-state-body">{body}</p>}
      {action && <div className="empty-state-action">{action}</div>}
    </div>
  );
}
