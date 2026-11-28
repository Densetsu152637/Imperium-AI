// Deleting a session for good, free of React so tests can run it (test/deleteSession.test.ts). The
// session's ⋯ menu offers "Delete…" for every session, running or archived, except an environment's
// persistent agent (its environment turns it off). The dialog names the session and says the delete
// is permanent. Confirming goes only through the app's own `session_erase` command: the daemon
// answers `session.erase` to the desktop app's own process alone, so the webview cannot send it.

import { plain } from "../shared/plainText.ts";

/** What `session_erase` answers (apps/desktop/src-tauri/src/session_erase.rs `Outcome`). */
export type EraseOutcome = { erased: boolean; reason: string | null };

/** The Tauri command bridge, injected so a test can stand in for it. */
export type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

/** What the ⋯ menu shows for the delete item of a session. */
export type DeleteItem = { label: string; danger: true; disabled: boolean; title?: string };

/** The menu item for a session: always there, off only for an environment's persistent agent. */
export function deleteItem(session: { persistent?: boolean }): DeleteItem {
  return session.persistent
    ? { label: "Delete…", danger: true, disabled: true, title: "An environment's agent is turned off in its environment, not deleted" }
    : { label: "Delete…", danger: true, disabled: false };
}

/** The words of the confirmation: it names the session and says the delete cannot be undone. */
export function deleteConfirmation(title: string, archived: boolean): { heading: string; lines: string[]; confirm: string } {
  const name = plain(title).trim() || "Untitled session";
  return {
    heading: `Delete “${name}”?`,
    lines: [
      `This permanently deletes the ${archived ? "archived " : ""}session “${name}”, its transcript and its stored session data. It cannot be undone.`,
      "If the session is running, it is stopped first. Files in your project folders are not touched.",
    ],
    confirm: "Delete session",
  };
}

/** Asks the app to delete the session. Rejects with the reason it was not. */
export async function eraseSession(invoke: Invoke, sessionId: string): Promise<void> {
  const outcome = (await invoke("session_erase", { session: sessionId })) as EraseOutcome;
  if (!outcome.erased) throw new Error(plain(outcome.reason) || "The session could not be deleted.");
}
