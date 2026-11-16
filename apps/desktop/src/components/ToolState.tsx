// A tool call's state as an icon: a spinner while it runs, a check or a cross when it ends.

import { Check, LoaderCircle, X } from "lucide-react";

export function ToolState({ ok }: { ok: boolean | null }) {
  if (ok === null) return <LoaderCircle className="tool-icon spin" role="img" aria-label="running" />;
  return ok ? (
    <Check className="tool-icon ok" role="img" aria-label="succeeded" />
  ) : (
    <X className="tool-icon failed" role="img" aria-label="failed" />
  );
}
