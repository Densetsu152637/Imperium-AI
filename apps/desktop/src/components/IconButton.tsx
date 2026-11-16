// A button that holds only an icon. `label` is required (the type is the guard): it becomes the
// accessible name and the tooltip, so an icon button can never be nameless. Pass `tooltip` for a longer
// explanation (it is then also the accessible description). Layout classes only in `className`.

import type { LucideIcon } from "lucide-react";
import type { ButtonHTMLAttributes, ReactNode, Ref } from "react";

import { Tooltip } from "./Tooltip";

export type IconButtonProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children" | "title" | "aria-label"> & {
  icon: LucideIcon;
  /** The accessible name and, unless `tooltip` says more, the tooltip. */
  label: string;
  /** A fuller tooltip than the label. */
  tooltip?: ReactNode;
  /** `sm` is the 24 px row-tool size; the default is the control height. */
  size?: "sm" | "md";
  /** `ghost` (the default) is bare until hovered. */
  variant?: "ghost" | "default" | "primary" | "danger";
  ref?: Ref<HTMLButtonElement>;
};

const VARIANT = { ghost: "ghost", default: "", primary: "primary", danger: "ghost danger" } as const;

export function IconButton({ icon: Icon, label, tooltip, size = "md", variant = "ghost", className, type = "button", ref, ...rest }: IconButtonProps) {
  const classes = ["icon-only", VARIANT[variant], size === "sm" ? "icon-sm" : "", className].filter(Boolean).join(" ");
  return (
    <Tooltip text={tooltip ?? label} describe={tooltip !== undefined}>
      <button ref={ref} type={type} className={classes} aria-label={label} {...rest}>
        <Icon aria-hidden="true" />
      </button>
    </Tooltip>
  );
}
