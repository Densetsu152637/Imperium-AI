// Messages waiting for the agent while it works, and what each can still do. Pure, so the rules
// are tested without a window (test/queue.test.ts); SessionPanel draws them and App.tsx sends
// the changes as `session.queue`. Every window (detached panels too) follows `queue_changed`,
// so a change made in one shows in all.

import type { QueuedMessage, SessionQueueParams } from "@imperium/protocol";

/** A change to one waiting message. */
export type QueueChange = { kind: "edit"; text: string } | { kind: "remove" } | { kind: "steer" };

/** What a waiting message offers, in the order the buttons appear. */
export type QueueAction = QueueChange["kind"];

/**
 * The controls of a waiting message. A queued one can be edited, steered into the running turn
 * (only while a turn runs), or removed. One that is steering waits only for the agent's next
 * safe point: it can no longer be edited or steered again, only removed before it lands.
 */
export function queueActions(message: QueuedMessage, busy: boolean): QueueAction[] {
  if (message.steer) return ["remove"];
  return busy ? ["edit", "steer", "remove"] : ["edit", "remove"];
}

/** The label before a waiting message's text. */
export const queueLabel = (message: QueuedMessage): string => (message.steer ? "Steering…" : "Queued");

/** The label of a steering message in the transcript, where the agent read it. */
export function steeredLabel(paused: readonly string[]): string {
  if (paused.length === 0) return "Steered";
  return `Steered — ${paused.length} ${paused.length === 1 ? "subagent" : "subagents"} paused`;
}

/** The `session.queue` request for a change; `agentId` names a running subagent's queue. */
export function queueParams(sessionId: string, id: string, change: QueueChange, agentId?: string): SessionQueueParams {
  const agent = agentId ? { agent_id: agentId } : {};
  switch (change.kind) {
    case "edit":
      return { session_id: sessionId, action: "edit", id, text: change.text, ...agent };
    case "remove":
      return { session_id: sessionId, action: "remove", id, ...agent };
    case "steer":
      return { session_id: sessionId, action: "steer", id, ...agent };
  }
}

/** Whether an edit in progress must stop: its message was delivered, removed, or is steering now. */
export function editStillValid(queue: readonly QueuedMessage[], id: string): boolean {
  const message = queue.find((waiting) => waiting.id === id);
  return message !== undefined && !message.steer;
}
