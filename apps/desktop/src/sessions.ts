// Session state built from the daemon's event stream: a transcript per session and the
// per-call telemetry that the session and telemetry panels display.

import type { SessionEvent, SessionSummary, TokenUsage } from "@imperium/protocol";

export type Entry =
  | { kind: "user"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; callId: string; summary: string; ok: boolean | null; output: string }
  | { kind: "permission"; requestId: number; tool: string; summary: string; answered: boolean }
  | { kind: "turn"; usage: TokenUsage; costUsd: number | null; steps: number }
  | { kind: "error"; message: string };

export type ModelCall = {
  at: number;
  model: string;
  usage: TokenUsage;
  durationMs: number;
  ttftMs: number | null;
  tokensPerSec: number | null;
  costUsd: number | null;
};

export type SessionState = {
  summary: SessionSummary;
  entries: Entry[];
  calls: ModelCall[];
  busy: boolean;
};

export const newSession = (summary: SessionSummary): SessionState => ({
  summary,
  entries: [],
  calls: [],
  busy: false,
});

/** Applies one event; returns a new state (inputs are never mutated). */
export function applyEvent(state: SessionState, event: SessionEvent): SessionState {
  // Requests are answered one at a time, so anything after a request means it was answered.
  const entries =
    event.type === "permission_requested" || event.type === "text_delta"
      ? [...state.entries]
      : state.entries.map((entry) => (entry.kind === "permission" && !entry.answered ? { ...entry, answered: true } : entry));
  const last = entries.at(-1);
  switch (event.type) {
    case "turn_started":
      return { ...state, busy: true, entries: [...entries, { kind: "user", text: event.text }] };
    case "text_delta":
      if (last?.kind === "assistant") entries[entries.length - 1] = { ...last, text: last.text + event.text };
      else entries.push({ kind: "assistant", text: event.text });
      return { ...state, entries };
    case "permission_requested":
      entries.push({
        kind: "permission",
        requestId: event.request_id,
        tool: event.tool,
        summary: event.summary,
        answered: false,
      });
      return { ...state, entries };
    case "tool_started":
      entries.push({ kind: "tool", callId: event.call_id, summary: event.summary, ok: null, output: "" });
      return { ...state, entries };
    case "tool_finished": {
      const index = entries.findLastIndex((entry) => entry.kind === "tool" && entry.callId === event.call_id);
      if (index >= 0) {
        const tool = entries[index] as Extract<Entry, { kind: "tool" }>;
        entries[index] = { ...tool, ok: event.ok, output: event.output };
      } else {
        // Denied or invalid calls finish without starting.
        entries.push({ kind: "tool", callId: event.call_id, summary: event.output, ok: event.ok, output: "" });
      }
      return { ...state, entries };
    }
    case "model_call_completed":
      return {
        ...state,
        calls: [
          ...state.calls,
          {
            at: Date.now(),
            model: event.model,
            usage: event.usage,
            durationMs: event.duration_ms,
            ttftMs: event.time_to_first_token_ms,
            tokensPerSec: event.tokens_per_sec,
            costUsd: event.cost_usd,
          },
        ],
      };
    case "turn_completed":
      entries.push({ kind: "turn", usage: event.usage, costUsd: event.cost_usd, steps: event.steps });
      return { ...state, busy: false, entries };
    case "turn_failed":
      entries.push({ kind: "error", message: event.message });
      return { ...state, busy: false, entries };
  }
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
