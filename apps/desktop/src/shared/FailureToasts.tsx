// Failure toasts in the corner of the window: a daemon-side GitHub automation failure the user
// would otherwise only find in logs. Text only, never markup.

import { useEffect } from "react";
import { X } from "lucide-react";

import { IconButton } from "../components/IconButton";
import { Notice } from "../components/Notice";
import type { FailureToast } from "./failureNotices";

const SHOWN_MS = 15_000;

export function FailureToasts({ toasts, onDismiss }: { toasts: FailureToast[]; onDismiss: (id: string) => void }) {
  return (
    <div className="failure-toasts" aria-live="polite">
      {toasts.map((toast) => (
        <Toast key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function Toast({ toast, onDismiss }: { toast: FailureToast; onDismiss: (id: string) => void }) {
  useEffect(() => {
    const timer = window.setTimeout(() => onDismiss(toast.id), SHOWN_MS);
    return () => window.clearTimeout(timer);
  }, [toast.id, onDismiss]);
  return (
    <Notice
      tone="danger"
      title={toast.title}
      action={
        <IconButton icon={X} label="Dismiss" size="sm" onClick={() => onDismiss(toast.id)} />
      }
    >
      {toast.body}
    </Notice>
  );
}
