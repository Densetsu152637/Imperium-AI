// Stopping a session: every agent halts at its next safe point and queued messages wait until
// the user resumes (`session.control` `stop` / `resume`). Pure, so the rules are tested without
// a window (test/stop.test.ts); SessionPanel and AgentsPanel draw them.

import type { SessionStatus } from "@imperium/protocol";

/** What the daemon says of a session's status (`status_changed`, or its summary after a restart). */
type Status = { status: SessionStatus; statusDetail: string | null };

/** Whether the user stopped the session: paused with the detail `stopped`, unlike a usage-limit wait. */
export const isStopped = ({ status, statusDetail }: Status): boolean => status === "paused" && statusDetail === "stopped";

/** Whether a Stop button makes sense: something is running and the session is not stopped already. */
export const canStop = (session: Status & { busy: boolean }, runningTasks: number): boolean =>
  !isStopped(session) && (session.busy || runningTasks > 0);

/** The shortcut that stops a running session or resumes a stopped one: Ctrl+. (⌘. on macOS). */
export const isStopShortcut = (event: { key: string; ctrlKey: boolean; metaKey: boolean; altKey: boolean }): boolean =>
  event.key === "." && (event.ctrlKey || event.metaKey) && !event.altKey;

/** A subagent's status as the Agents panel shows it: a running one is paused while the session is stopped. */
export function shownTaskStatus<T extends string>(status: T, stopped: boolean): T | "paused" {
  return stopped && status === "running" ? "paused" : status;
}
