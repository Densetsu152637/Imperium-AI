// Asks before a session is deleted for good (a session's ⋯ menu, "Delete…"): names the session and
// says the delete is permanent. It stays open with the reason when the delete fails.

import { useState } from "react";

import { Dialog } from "../components/Dialog";
import { Notice } from "../components/Notice";
import { deleteConfirmation } from "../sessions/deleteSession";
import { message } from "../shared/plainText";

export function DeleteSessionDialog({
  title,
  archived,
  onConfirm,
  onClose,
}: {
  /** The session's title, as the list shows it. */
  title: string;
  archived: boolean;
  /** Deletes the session; rejects with why it could not. */
  onConfirm: () => Promise<void>;
  onClose: () => void;
}) {
  const words = deleteConfirmation(title, archived);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      await onConfirm();
      onClose();
    } catch (failure) {
      setError(message(failure));
      setBusy(false);
    }
  };

  return (
    <Dialog
      label="Delete session"
      title={words.heading}
      notice={error ? <Notice tone="danger">{error}</Notice> : undefined}
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button onClick={onClose} disabled={busy} autoFocus>
            Cancel
          </button>
          <button className="danger" onClick={() => void confirm()} disabled={busy}>
            {busy ? "Deleting…" : words.confirm}
          </button>
        </>
      }
    >
      {words.lines.map((line) => (
        <p key={line}>{line}</p>
      ))}
    </Dialog>
  );
}
