// What the desktop shows for the daemon's `autofix.event` notifications: only `failed`, as a
// toast and (through `notify`) a system notification. The summary can carry repository names and
// GitHub-derived text, so it is cleaned with `plain` and the views draw it only as text.
// Pure, so the rules are tested without a window (test/failureNotices.test.ts).

import type { AutofixEventParams } from "@imperium/protocol";

import type { Notice } from "./attention";
import { plain } from "./plainText.ts";

/** How long an identical failure stays quiet after it was shown. */
export const REPEAT_QUIET_MS = 10 * 60 * 1000;
/** The most distinct failures remembered; past it the oldest is forgotten. */
export const MAX_REMEMBERED = 50;
/** The least time between two system notifications, whatever the failures say. */
export const NOTICE_MIN_GAP_MS = 5000;
/** The longest summary shown. */
const SUMMARY_CHARS = 300;

/** A failure on screen: plain text only. */
export type FailureToast = { id: string; title: string; body: string };

/**
 * When each failure was last shown, by its key, oldest first; kept by the caller between events.
 * `lastNoticeAt` is when a system notification was last allowed.
 */
export type FailureMemory = Map<string, number> & { lastNoticeAt?: number };

/** `repo#12`, or just the repository for a failure that is not about one pull request (number 0). */
function where(params: AutofixEventParams): string {
  const repo = plain(params.repo) || "a repository";
  return params.pull_request > 0 ? `${repo}#${params.pull_request}` : repo;
}

/**
 * What a failed `autofix.event` shows, or null for any other status or for the same failure
 * shown within [`REPEAT_QUIET_MS`]. `memory` records what was shown, at most [`MAX_REMEMBERED`]
 * failures. The toast always shows; `notice` is null when the last system notification was
 * under [`NOTICE_MIN_GAP_MS`] ago, so summaries that differ every time cannot flood the desktop.
 */
export function failureNotice(
  params: AutofixEventParams,
  memory: FailureMemory,
  now: number,
): { toast: FailureToast; notice: Notice | null } | null {
  if (params.status !== "failed") return null;
  const title = `GitHub automation failed on ${where(params)}`;
  const summary = plain(params.summary);
  const body = (summary.length > SUMMARY_CHARS ? `${summary.slice(0, SUMMARY_CHARS)}…` : summary) || "No details were given.";
  const key = `${where(params)}\n${body}`;
  for (const [seen, at] of memory) if (now - at >= REPEAT_QUIET_MS) memory.delete(seen);
  if (memory.has(key)) return null;
  while (memory.size >= MAX_REMEMBERED) {
    const oldest = memory.keys().next();
    if (oldest.done) break;
    memory.delete(oldest.value);
  }
  memory.set(key, now);
  const quiet = memory.lastNoticeAt !== undefined && now - memory.lastNoticeAt < NOTICE_MIN_GAP_MS;
  if (!quiet) memory.lastNoticeAt = now;
  return {
    toast: { id: `${now}:${key}`, title, body },
    notice: quiet ? null : { kind: "failed", title, body, sessionId: params.session_id },
  };
}
