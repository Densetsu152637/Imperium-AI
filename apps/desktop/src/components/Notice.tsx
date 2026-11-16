// An inline message with an icon: a failure, a warning, a success or plain information, with an
// optional title and one action. Failures announce themselves (`role="alert"`); the rest are polite.

import { CircleAlert, CircleCheck, Info, TriangleAlert } from "lucide-react";
import type { ReactNode } from "react";

export type NoticeTone = "info" | "ok" | "warn" | "danger";

const ICONS = { info: Info, ok: CircleCheck, warn: TriangleAlert, danger: CircleAlert } as const;

export function Notice({
  tone = "info",
  title,
  action,
  className,
  children,
}: {
  tone?: NoticeTone;
  title?: ReactNode;
  /** At most one: a button or link. */
  action?: ReactNode;
  className?: string;
  children?: ReactNode;
}) {
  const Icon = ICONS[tone];
  return (
    <div className={`notice-box tone-${tone}${className ? ` ${className}` : ""}`} role={tone === "danger" ? "alert" : "status"}>
      <Icon className="notice-icon" aria-hidden="true" />
      <div className="notice-text">
        {title && <p className="notice-title">{title}</p>}
        {children && <div className="notice-body">{children}</div>}
      </div>
      {action && <div className="notice-action">{action}</div>}
    </div>
  );
}
