// Session state built from the daemon's event stream: the root agent's transcript, each
// subagent's task, the current plan, and the per-call telemetry the panels display.

import type { AgentEvent, Plan, SessionSummary, TokenUsage } from "@imperium/protocol";

export const ROOT = "root";

export type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; callId: string; summary: string; ok: boolean | null; output: string }
  | { kind: "permission"; agentId: string; requestId: number; tool: string; summary: string; answered: boolean }
  | { kind: "task"; taskId: string; brief: string; model: string }
  | { kind: "turn"; usage: TokenUsage; costUsd: number | null; steps: number }
  | { kind: "error"; message: string }
  | { kind: "notice"; message: string };

export type TaskState = {
  taskId: string;
  brief: string;
  model: string;
  status: "running" | "finished" | "failed";
  entries: Entry[];
  report: string;
  costUsd: number | null;
};

export type ModelCall = {
  at: number;
  agentId: string;
  model: string;
  usage: TokenUsage;
  durationMs: number;
  ttftMs: number | null;
  tokensPerSec: number | null;
  costUsd: number | null;
};

export type SessionState = {
  summary: SessionSummary;
  /** The root agent's transcript, plus permission requests from any agent. */
  entries: Entry[];
  tasks: Record<string, TaskState>;
  plan: Plan | null;
  calls: ModelCall[];
  busy: boolean;
};

export const newSession = (summary: SessionSummary): SessionState => ({
  summary,
  entries: [],
  tasks: {},
  plan: null,
  calls: [],
  busy: false,
});

/** Applies one event to a transcript; returns a new array (inputs are never mutated). */
function applyToEntries(entries: Entry[], { event }: AgentEvent): Entry[] {
  const next = [...entries];
  const last = next.at(-1);
  switch (event.type) {
    case "turn_started":
      next.push({ kind: "user", text: event.text });
      break;
    case "text_delta":
      if (last?.kind === "assistant") next[next.length - 1] = { ...last, text: last.text + event.text };
      else next.push({ kind: "assistant", text: event.text });
      break;
    case "tool_started":
      next.push({ kind: "tool", callId: event.call_id, summary: event.summary, ok: null, output: "" });
      break;
    case "tool_finished": {
      const index = next.findLastIndex((entry) => entry.kind === "tool" && entry.callId === event.call_id && entry.ok === null);
      if (index >= 0) {
        const tool = next[index] as Extract<Entry, { kind: "tool" }>;
        next[index] = { ...tool, ok: event.ok, output: event.output };
      } else {
        // Denied, invalid, and planning/delegation calls finish without a start event.
        next.push({ kind: "tool", callId: event.call_id, summary: event.output, ok: event.ok, output: "" });
      }
      break;
    }
    case "task_spawned":
      next.push({ kind: "task", taskId: event.task_id, brief: event.brief, model: event.model });
      break;
    case "turn_completed":
      next.push({ kind: "turn", usage: event.usage, costUsd: event.cost_usd, steps: event.steps });
      break;
    case "turn_failed":
      next.push({ kind: "error", message: event.message });
      break;
    case "notice":
      next.push({ kind: "notice", message: event.message });
      break;
    default:
      break;
  }
  return next;
}

/** Applies one event; returns a new state (inputs are never mutated). */
export function applyEvent(state: SessionState, agentEvent: AgentEvent): SessionState {
  const { agent_id: agentId, event } = agentEvent;
  let entries = state.entries;

  // A request is answered once its agent moves on to running or finishing a tool.
  if (event.type === "tool_started" || event.type === "tool_finished") {
    entries = entries.map((entry) =>
      entry.kind === "permission" && entry.agentId === agentId && !entry.answered ? { ...entry, answered: true } : entry,
    );
  }
  if (event.type === "permission_requested") {
    entries = [
      ...entries,
      { kind: "permission", agentId, requestId: event.request_id, tool: event.tool, summary: event.summary, answered: false },
    ];
  }

  let { tasks, plan, calls, busy } = state;
  if (event.type === "model_call_completed") {
    calls = [
      ...calls,
      {
        at: Date.now(),
        agentId,
        model: event.model,
        usage: event.usage,
        durationMs: event.duration_ms,
        ttftMs: event.time_to_first_token_ms,
        tokensPerSec: event.tokens_per_sec,
        costUsd: event.cost_usd,
      },
    ];
  }

  if (agentId === ROOT) {
    entries = applyToEntries(entries, agentEvent);
    if (event.type === "plan_updated") plan = event.plan;
    if (event.type === "turn_started") busy = true;
    if (event.type === "turn_completed" || event.type === "turn_failed") busy = false;
    if (event.type === "task_spawned") {
      tasks = {
        ...tasks,
        [event.task_id]: {
          taskId: event.task_id,
          brief: event.brief,
          model: event.model,
          status: "running",
          entries: [],
          report: "",
          costUsd: null,
        },
      };
    }
  } else if (tasks[agentId]) {
    const task = tasks[agentId];
    const updated: TaskState =
      event.type === "task_finished"
        ? { ...task, status: event.ok ? "finished" : "failed", report: event.report, costUsd: event.cost_usd }
        : { ...task, entries: applyToEntries(task.entries, agentEvent) };
    tasks = { ...tasks, [agentId]: updated };
  }
  return { ...state, entries, tasks, plan, calls, busy };
}

export function markAnswered(state: SessionState, requestId: number): SessionState {
  return {
    ...state,
    entries: state.entries.map((entry) =>
      entry.kind === "permission" && entry.requestId === requestId ? { ...entry, answered: true } : entry,
    ),
  };
}

export type Totals = { prompt: number; cached: number; completion: number; costUsd: number | null };

export function totals(calls: ModelCall[]): Totals {
  return calls.reduce<Totals>(
    (sum, call) => ({
      prompt: sum.prompt + call.usage.prompt_tokens,
      cached: sum.cached + call.usage.cached_tokens,
      completion: sum.completion + call.usage.completion_tokens,
      costUsd: call.costUsd === null ? sum.costUsd : (sum.costUsd ?? 0) + call.costUsd,
    }),
    { prompt: 0, cached: 0, completion: 0, costUsd: null },
  );
}

export const formatCost = (usd: number | null): string => (usd === null ? "—" : `$${usd.toFixed(4)}`);
export const formatCount = (n: number): string => n.toLocaleString();
