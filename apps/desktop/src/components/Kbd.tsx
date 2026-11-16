// A key or key chord in small type on a sunken chip: `<Kbd>Esc</Kbd>` for one key, or
// `<Kbd keys={["Ctrl", "K"]} />` for a chord, one chip per key.

import type { ReactNode } from "react";

export function Kbd({ keys, children, className }: { keys?: string[]; children?: ReactNode; className?: string }) {
  if (keys) {
    return (
      <span className={`kbd-chord${className ? ` ${className}` : ""}`}>
        {keys.map((key, index) => (
          <kbd key={`${index}:${key}`} className="kbd">
            {key}
          </kbd>
        ))}
      </span>
    );
  }
  return <kbd className={`kbd${className ? ` ${className}` : ""}`}>{children}</kbd>;
}
