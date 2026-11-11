// A subagent opened in the main panel (panels/AgentView.tsx): how long it has run, and how a
// message typed to it is delivered. Pure, so the rules are tested without a window
// (test/agentView.test.ts).

import type { SendMode } from "@imperium/protocol";

import type { TaskState } from "./sessions";

/** What a paused subagent is told when the user resumes it without a follow-up of their own. */
export const RESUME_TEXT = "Continue where you left off.";

/** A subagent's name: the title it was spawned with, else the first line of its brief (older
 * journals have no title). */
export function taskTitle(title: string | undefined, brief: string): string {
  const given = title?.trim();
  if (given) return given;
  const first = brief.split("\n").find((line) => line.trim())?.trim() ?? "";
  return first.length > 60 ? `${first.slice(0, 59)}…` : first || "Subagent";
}

/** A duration as `45s`, `3m 05s`, or `1h 02m`. */
export function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${String(seconds % 60).padStart(2, "0")}s`;
  return `${Math.floor(minutes / 60)}h ${String(minutes % 60).padStart(2, "0")}m`;
}

/** How long a subagent's current run has taken (until now while it runs); null when unknown. */
export function taskElapsed(task: Pick<TaskState, "status" | "startedAt" | "endedAt">, now = Date.now()): number | null {
  if (task.startedAt === null) return null;
  const end = task.status === "running" ? now : task.endedAt;
  return end === null ? null : Math.max(0, end - task.startedAt);
}

/**
 * How a message typed in a subagent's view reaches it. While it runs, the message goes to its
 * queue like a message to the root (`session.send` with its `agent_id`; `steer` joins its
 * running turn). A finished, failed, or paused subagent is given the message as a follow-up
 * (`session.agent_resume`), which continues its kept conversation.
 */
export type AgentDelivery =
  | { kind: "send"; mode: SendMode }
  | { kind: "resume" };

export function agentDelivery(status: TaskState["status"], mode: SendMode): AgentDelivery {
  return status === "running" ? { kind: "send", mode } : { kind: "resume" };
}

/** Whether the daemon refused a message because the subagent is no longer running, so it is
 * sent again as a follow-up (it finished between the click and the call). */
export function finishedMeanwhile(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return message.includes("no running agent");
}

/** A follow-up's text with the files attached to it named, since follow-ups carry no files. */
export function followUpText(text: string, attachments: readonly { name: string; path: string }[]): string {
  if (attachments.length === 0) return text;
  const files = attachments.map((file) => `- ${file.name}: ${file.path}`).join("\n");
  return `${text}${text ? "\n\n" : ""}Attached files:\n${files}`;
}

/** The composer's placeholder in a subagent's view. */
export function agentComposerHint(title: string, status: TaskState["status"], enterSends: boolean): string {
  const key = enterSends ? "Enter" : "Ctrl+Enter";
  if (status === "running") {
    return `${key} queues for ${title} after its current turn · Steer a queued message (or Ctrl+Shift+Enter) to send it now`;
  }
  if (status === "paused") return `Message ${title} to resume it (${key})`;
  return `Give ${title} a follow-up; it continues its earlier conversation (${key})`;
}
