// What the desktop shows for the daemon's `autofix.event` notifications: only `failed`, as a
// toast and (through `notify`) a system notification. The summary can carry repository names and
// GitHub-derived text, so it is cleaned with `plain` and the views draw it only as text.
// Pure, so the rules are tested without a window (test/failureNotices.test.ts).

import type { AutofixEventParams } from "@imperium/protocol";

import type { Notice } from "./attention";
import { plain } from "./plainText.ts";

/** How long an identical failure stays quiet after it was shown. */
export const REPEAT_QUIET_MS = 10 * 60 * 1000;
/** The longest summary shown. */
const SUMMARY_CHARS = 300;

/** A failure on screen: plain text only. */
export type FailureToast = { id: string; title: string; body: string };

/** When each failure was last shown, by its key; kept by the caller between events. */
export type FailureMemory = Map<string, number>;

/** `repo#12`, or just the repository for a failure that is not about one pull request (number 0). */
function where(params: AutofixEventParams): string {
  const repo = plain(params.repo) || "a repository";
  return params.pull_request > 0 ? `${repo}#${params.pull_request}` : repo;
}

/**
 * What a failed `autofix.event` shows, or null for any other status or for the same failure
 * shown within [`REPEAT_QUIET_MS`]. `memory` records what was shown.
 */
export function failureNotice(
  params: AutofixEventParams,
  memory: FailureMemory,
  now: number,
): { toast: FailureToast; notice: Notice } | null {
  if (params.status !== "failed") return null;
  const title = `GitHub automation failed on ${where(params)}`;
  const summary = plain(params.summary);
  const body = (summary.length > SUMMARY_CHARS ? `${summary.slice(0, SUMMARY_CHARS)}…` : summary) || "No details were given.";
  const key = `${where(params)}\n${body}`;
  for (const [seen, at] of memory) if (now - at >= REPEAT_QUIET_MS) memory.delete(seen);
  if (memory.has(key)) return null;
  memory.set(key, now);
  return {
    toast: { id: `${now}:${key}`, title, body },
    notice: { kind: "failed", title, body, sessionId: params.session_id },
  };
}
