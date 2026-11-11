// Why an agent is not answering yet: after a message is sent (or a subagent starts), the core
// may first load a model, make room on the GPU, compact the conversation, install llama.cpp,
// wait for a slot, or wait for the user. This follows those waits per agent from the session's
// events until the agent's first output, and words them for the status line under the sent
// message (SessionPanel) and the Agents panel's rows. Pure, so the rules are tested without a
// window (test/waits.test.ts).

import type { AgentEvent, LocalWaitReason, PauseCause, RuntimeInstallJob } from "@imperium/protocol";

/** What an agent waits on, when the session's events say. */
export type WaitCause =
  /** A local model call waits (`local_model_waiting`); `reason` may be one this version does not know. */
  | { kind: "local"; reason: LocalWaitReason | (string & {}); model: string }
  /** Older messages are being summarised (`compaction_started` until `context_compacted`). */
  | { kind: "compacting"; model: string }
  /** A tool call waits for the user's approval. */
  | { kind: "approval" }
  /** The agent asked the user questions. */
  | { kind: "questions" };

/** An agent that has not produced output since its turn started (or since it began to wait). */
export type AgentWait = { since: number; cause: WaitCause | null };

/** Waits by agent id (`root` or a task id). */
export type Waits = Record<string, AgentWait>;

/** A message this window sent that has not started a turn yet. */
export type Sent = {
  text: string;
  since: number;
  /** A used-up budget asks before it is sent (`-32008` with `ask`): its message. */
  budget: string | null;
};

/** Events that are an agent's output, or end its turn: its wait is over. */
const OUTPUT = new Set<string>([
  "text_delta",
  "tool_started",
  "tool_finished",
  "model_call_completed",
  "turn_completed",
  "turn_failed",
  "task_finished",
  "task_paused",
  "questions_answered",
]);

const waitOn = (waits: Waits, agentId: string, cause: WaitCause | null, now: number): Waits => ({
  ...waits,
  [agentId]: { since: waits[agentId]?.since ?? now, cause },
});

const without = (waits: Waits, agentId: string): Waits => {
  if (!(agentId in waits)) return waits;
  const { [agentId]: _done, ...rest } = waits;
  return rest;
};

/** Applies one event to the waits; returns `waits` itself when nothing changed. */
export function applyWait(waits: Waits, { agent_id: agentId, event }: AgentEvent, now: number): Waits {
  switch (event.type) {
    case "turn_started":
      // Whatever came before the turn (fitting the message in) is done; the turn waits for output.
      return waitOn(waits, agentId, null, now);
    case "task_spawned":
    case "task_resumed":
      return { ...waits, [event.task_id]: { since: now, cause: null } };
    case "local_model_waiting":
      return waitOn(waits, event.agent_id || agentId, { kind: "local", reason: event.reason, model: event.model }, now);
    case "compaction_started":
      return waitOn(waits, agentId, { kind: "compacting", model: event.model }, now);
    case "context_compacted":
    case "notice": {
      // A compaction ended (a notice reports one that failed before a turn).
      const wait = waits[agentId];
      if (wait?.cause?.kind !== "compacting") return waits;
      return { ...waits, [agentId]: { ...wait, cause: null } };
    }
    case "permission_requested":
      return waitOn(waits, agentId, { kind: "approval" }, now);
    case "questions_asked":
      return waitOn(waits, agentId, { kind: "questions" }, now);
    default:
      return OUTPUT.has(event.type) ? without(waits, agentId) : waits;
  }
}

/**
 * The message this window sent, after one event: a turn starting takes it into the transcript,
 * and a message that waits in the queue or for a model is shown there instead.
 */
export function applySent(sent: Sent | null, { agent_id: agentId, event }: AgentEvent): Sent | null {
  if (!sent || agentId !== "root") return sent;
  if (event.type === "turn_started" || event.type === "message_queued") return null;
  if (event.type === "status_changed" && event.status === "needs_model") return null;
  return sent;
}

/** What each local wait reason says; reasons a newer daemon adds are worded from their name. */
const LOCAL_TEXT: Record<string, string> = {
  loading: "Loading model…",
  growing_pool: "Growing context pool…",
  kv_room: "Waiting for room in the context cache…",
  slot: "Waiting for a free slot…",
  other_model: "Waiting for another model…",
  making_room: "Making room on the GPU…",
  saving_context: "Saving context…",
  restoring_context: "Restoring context…",
};

/** A local wait reason in words: `loading` → "Loading model…", `some_new_reason` → "Some new reason…". */
export function localWaitText(reason: string): string {
  const known = LOCAL_TEXT[reason];
  if (known) return known;
  const words = reason.replace(/[_-]+/g, " ").trim();
  return words ? `${words[0].toUpperCase()}${words.slice(1)}…` : "Waiting for the local model…";
}

/** llama.cpp being installed: "Installing llama.cpp (42%)…", without a share until the size is known. */
export function installText(job: RuntimeInstallJob): string {
  if (job.total_bytes <= 0) return "Installing llama.cpp…";
  const percent = Math.min(100, Math.floor((job.done_bytes / job.total_bytes) * 100));
  return `Installing llama.cpp (${percent}%)…`;
}

/** What the status line offers: stop the session's work, resume it, end a usage-limit wait, or answer a budget. */
export type WaitAction = "stop" | "resume" | "resume_now" | "budget" | null;

export type WaitView = {
  text: string;
  /** When the wait began, for the elapsed timer. */
  since: number;
  /** The model it is about, when there is one. */
  model: string | null;
  action: WaitAction;
};

/**
 * The status line for one agent, or `null` when there is nothing to say (the agent is producing
 * output, or its turn runs with no known wait). Stopping, a pause, and the user's own answers
 * come first, since nothing else moves until they are dealt with.
 */
export function describeWait({
  wait,
  sent = null,
  stopped = false,
  pause = null,
  install = null,
}: {
  wait: AgentWait | null | undefined;
  /** For the root: a message this window sent that has not started a turn. */
  sent?: Sent | null;
  stopped?: boolean;
  /** The session's pause, with its end in words when it has one. */
  pause?: { cause: PauseCause; until: string | null } | null;
  /** A llama.cpp installation the daemon reports (`runtime.install_progress`). */
  install?: RuntimeInstallJob | null;
}): WaitView | null {
  const since = Math.min(wait?.since ?? Infinity, sent?.since ?? Infinity);
  if (since === Infinity) return null;
  const view = (text: string, action: WaitAction, model: string | null = null): WaitView => ({ text, since, model, action });
  if (sent?.budget) return view(`A budget is used up: ${sent.budget}`, "budget");
  if (stopped) return view("Stopped — resume to continue", "resume");
  if (pause) {
    if (pause.cause === "load") return view("Paused while the machine is busy…", "resume_now");
    if (pause.cause === "deferred") return view("Waiting for a quiet machine…", "resume_now");
    return view(pause.until ? `Usage limit reached; waiting until ${pause.until}` : "Usage limit reached; waiting…", "resume_now");
  }
  const cause = wait?.cause ?? null;
  switch (cause?.kind) {
    case "approval":
      return view("Waiting for your approval…", null);
    case "questions":
      return view("Waiting for your answers…", null);
    case "compacting":
      return view("Compacting conversation…", "stop", cause.model);
    case "local":
      if (cause.reason === "loading" && install?.state === "running") return view(installText(install), "stop", cause.model);
      return view(localWaitText(cause.reason), "stop", cause.model);
  }
  // Sent, and the core has not said what it is doing yet (preparing the message, measuring it).
  if (sent) return view("Starting…", "stop");
  return null;
}

/** Elapsed time as `0:07`, `12:34`, or `1:02:03`. */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}` : `${minutes}:${seconds}`;
}
