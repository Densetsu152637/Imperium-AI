// The loading state of a view or row: a spinner and a line of text, shown only after 300 ms so a
// fast load never flashes. `inline` is the small variant for inside a row or beside a control.
// `Spinner` is the one spinner everything else shares.

import { LoaderCircle } from "lucide-react";
import { useEffect, useState } from "react";

/** How long a load may take before anything shows. */
const SHOW_AFTER_MS = 300;

export function Spinner({ className }: { className?: string }) {
  return <LoaderCircle className={`spin${className ? ` ${className}` : ""}`} aria-hidden="true" />;
}

export function LoadingState({ label = "Loading…", inline = false, className }: { label?: string; inline?: boolean; className?: string }) {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(true), SHOW_AFTER_MS);
    return () => window.clearTimeout(timer);
  }, []);
  return (
    <div role="status" className={`loading-state${inline ? " inline" : ""}${className ? ` ${className}` : ""}`}>
      {visible && (
        <>
          <Spinner />
          <span>{label}</span>
        </>
      )}
    </div>
  );
}
