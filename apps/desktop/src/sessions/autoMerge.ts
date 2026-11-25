// Turning auto-merge on and off from the pull-request card, free of React so tests can run it
// (test/autoMerge.test.ts). Turning it on goes only through the app's own `auto_merge_enable`
// command: the app's native shell shows a confirmation dialog written from the daemon's own
// status, asks the daemon for the single-use challenge and answers it. The webview passes the
// session, the commit the card showed and the method, and never holds a challenge. Turning it off
// is an ordinary daemon call that needs no challenge.

import type { PrMergeMethod, PrStatus } from "@imperium/protocol";

import { plain } from "../shared/plainText.ts";

/** What `auto_merge_enable` answers (apps/desktop/src-tauri/src/auto_merge.rs `Outcome`). */
export type AutoMergeOutcome = { enabled: boolean; cancelled: boolean; reason: string | null };

/** The Tauri command bridge, injected so a test can stand in for it. */
export type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

/** The daemon client's call, which the turn-off goes through. */
export type Caller = { call<Result>(method: string, params?: unknown): Promise<Result> };

/**
 * Asks the app to turn auto-merge on for the session's pull request, for the commit the card
 * showed and `method`. The user confirms in a native dialog first.
 */
export async function turnOnAutoMerge(invoke: Invoke, sessionId: string, status: PrStatus, method: PrMergeMethod): Promise<AutoMergeOutcome> {
  return (await invoke("auto_merge_enable", { session: sessionId, head: status.head_sha, method })) as AutoMergeOutcome;
}

/** Turns it off: no challenge, any local caller may. Answers the new status. */
export function turnOffAutoMerge(client: Caller, sessionId: string, status: PrStatus): Promise<PrStatus> {
  return client.call<PrStatus>("pull_requests.set_auto_merge", { session_id: sessionId, url: status.url, head_sha: status.head_sha, enabled: false });
}

/** What to tell the user after `turnOnAutoMerge`: null when it worked or the user cancelled. */
export function problemOf(outcome: AutoMergeOutcome): string | null {
  if (outcome.enabled || outcome.cancelled) return null;
  return plain(outcome.reason) || "Auto-merge could not be turned on.";
}
