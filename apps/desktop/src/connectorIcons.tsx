// Icons for connectors. The catalogue names a Lucide icon per entry (as data, in kebab case);
// names come from the app's icon registry (icons.ts), and anything else shows a plug.

import { Plug } from "lucide-react";

import { iconFor } from "./icons";

/** A connector's icon on a raised tile. Decorative; the card supplies the name. */
export function ConnectorIcon({ icon, size = "normal" }: { icon?: string | null; size?: "normal" | "large" }) {
  const Icon = iconFor(icon) ?? Plug;
  return (
    <span className={`connector-icon ${size}`} aria-hidden="true">
      <Icon />
    </span>
  );
}
