// A small status label: text at the extra-small size on the soft colour of its tone. Colour is
// never the only cue: pass an icon or put the state in the words.

import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";

export type BadgeTone = "neutral" | "accent" | "ok" | "warn" | "danger";

export function Badge({
  tone = "neutral",
  icon: Icon,
  title,
  className,
  children,
}: {
  tone?: BadgeTone;
  icon?: LucideIcon;
  title?: string;
  /** Extra classes for layout, and the state words other rules key on (`status`, `working`). */
  className?: string;
  children: ReactNode;
}) {
  return (
    <span className={`badge tone-${tone}${className ? ` ${className}` : ""}`} title={title}>
      {Icon && <Icon aria-hidden="true" />}
      {children}
    </span>
  );
}
