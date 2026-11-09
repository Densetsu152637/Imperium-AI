// Session state built from the daemon's event stream: the root agent's transcript, each
// subagent's task, the current plan, and the per-call telemetry the panels display.

import type {
  AgentEvent,
  EnvironmentInfo,
  ContextCategory,
  PauseCause,
  PermissionRule,
  Plan,
  QueuedMessage,
  Question,
  ResumePolicy,
  SessionStatus,
  SessionSummary,
  TokenCountMethod,
  TokenUsage,
} from "@imperium/protocol";
import type { AgentSnapshot, SessionSnapshot } from "@imperium/mod-sdk";

export const ROOT = "root";

export type Entry =
  | { kind: "user"; text: string }
  /** A steering message that joined the running turn. */
  | { kind: "steer"; text: string }
  | { kind: "assistant"; text: string }
  | { kind: "tool"; callId: string; summary: string; ok: boolean | null; output: string }
  | {
      kind: "permission";
      agentId: string;
      requestId: number;
      tool: string;
      summary: string;
      /** The rule "Always allow" adds, when the daemon suggests one. */
      suggestedRule?: PermissionRule;
      answered: boolean;
    }
  | { kind: "questions"; requestId: number; questions: Question[]; answered: boolean }
  | { kind: "task"; taskId: string; brief: string; model: string }
  | { kind: "turn"; usage: TokenUsage; costUsd: number | null; steps: number }
  | { kind: "error"; message: string }
  | { kind: "notice"; message: string }
  | SuggestionEntry;

/** A model download suggestion and what became of it (see docs/07-desktop-ui.md). */
export type SuggestionEntry = {
  kind: "suggestion";
  id: string;
  need: string;
  repo: string;
  quant: string;
  /** What to switch to once it is downloaded. */
  model: string;
  sizeBytes: number;
  fit: string;
  reason: string;
  /** `pending` until answered; then as `model_suggestion_update` reports. */
  state: "pending" | "downloading" | "ready" | "failed" | "cancelled" | "dismissed" | "disabled";
  jobId: string | null;
  message: string | null;
};

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

/** What one agent's latest model call read, by category (see docs/03-agent-engine.md). */
export type ContextBreakdown = {
  at: number;
  agentId: string;
  model: string;
  /** Tokens the model reads per request; null when unknown. */
  contextWindow: number | null;
  method: TokenCountMethod;
  /** In a fixed order; `overhead` (chat template and formatting) is last and may be negative. */
  categories: ContextCategory[];
  /** The provider's reported prompt size; the categories add up to it. */
  promptTokens: number | null;
  cachedTokens: number;
};

export type SessionState = {
  summary: SessionSummary;
  /** The root agent's transcript, plus permission requests from any agent. */
  entries: Entry[];
  tasks: Record<string, TaskState>;
  plan: Plan | null;
  calls: ModelCall[];
  /** Each agent's latest context breakdown, by agent id. */
  breakdowns: Record<string, ContextBreakdown>;
  busy: boolean;
  /** Messages waiting for the root agent, in delivery order (steering ones join its turn). */
  queue: QueuedMessage[];
  /** The agent's browser after its latest action. */
  browser: { url: string; title: string; screenshot: string } | null;
  status: SessionStatus;
  /** What a watching or merged session follows, such as `owner/repo#12`. */
  statusDetail: string | null;
  /** While paused: when the session tries again after a usage limit (0 when it waits for a quiet machine), and why. */
  pause: { untilMs: number; reason: string; cause: PauseCause } | null;
  /** When this window last saw the session change, for ordering lists. */
  updatedAt: number;
};

/** The daemon's default, for sessions this window creates before their summary arrives. */
export const DEFAULT_RESUME_POLICY: ResumePolicy = {
  mode: "wait_for_reset",
  at_time: null,
  retry_every_minutes: 5,
  max_wait_hours: 12,
};

export const newSession = (summary: SessionSummary, updatedAt = 0): SessionState => ({
  summary,
  entries: [],
  tasks: {},
  plan: null,
  calls: [],
  breakdowns: {},
  busy: false,
  queue: [],
  browser: null,
  status: summary.status,
  statusDetail: summary.status_detail,
  pause: summary.hold ? { untilMs: 0, reason: summary.hold.reason, cause: summary.hold.cause } : null,
  updatedAt,
});

/** A pause time as `HH:MM`, with the weekday when it is not today. */
export function formatResumeTime(unixMs: number): string {
  const at = new Date(unixMs);
  const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  return at.toDateString() === new Date().toDateString()
    ? time
    : `${at.toLocaleDateString([], { weekday: "short" })} ${time}`;
}

/** The session's name for lists and headers: its title, else its first message. */
export function sessionTitle(session: SessionState): string {
  const title = session.summary.title.trim();
  if (title) return title;
  const first = session.entries.find((entry) => entry.kind === "user")?.text.trim();
  return first ? first.split("\n")[0].slice(0, 80) : "New session";
}

/** Applies one event to a transcript; returns a new array (inputs are never mutated). */
function applyToEntries(entries: Entry[], { event }: AgentEvent): Entry[] {
  const next = [...entries];
  const last = next.at(-1);
  switch (event.type) {
    case "turn_started":
      next.push({ kind: "user", text: event.text });
      break;
    case "steered":
      next.push({ kind: "steer", text: event.text });
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
    case "media_created":
      next.push({ kind: "notice", message: `Created ${event.kind} ${event.path}` });
      break;
    case "memory_used":
      next.push({ kind: "notice", message: `Recalled ${event.names.join(", ")}` });
      break;
    case "memory_saved":
      next.push({ kind: "notice", message: `Saved memory ${event.name} (${event.scope})` });
      break;
    case "context_manifest": {
      const required = event.items.filter((item) => item.required).length;
      next.push({
        kind: "notice",
        message: `Context for ${event.role}: ${required} required, ${event.documentation.length} documentation pointers`,
      });
      break;
    }
    case "context_loaded":
      next.push({ kind: "notice", message: `Loaded ${event.references.join(", ")}` });
      break;
    case "paused":
      next.push({
        kind: "notice",
        message:
          event.cause !== "usage_limit"
            ? `Background work paused: ${event.reason}. Resume now to skip the wait.`
            : event.until_unix_ms > 0
              ? `Usage limit reached; waiting until ${formatResumeTime(event.until_unix_ms)}`
              : "Usage limit reached; waiting",
      });
      break;
    case "resumed":
      next.push({ kind: "notice", message: "Resumed" });
      break;
    case "context_compacted":
      next.push({
        kind: "notice",
        message:
          `Context compacted to fit ${event.model}: ${formatTokens(event.before_tokens)} → ` +
          `${formatTokens(event.after_tokens)} tokens; ${event.summarised_messages} earlier messages ` +
          `summarised by ${event.summary_model}`,
      });
      break;
    case "model_suggestion":
      next.push({
        kind: "suggestion",
        id: event.id,
        need: event.need,
        repo: event.repo,
        quant: event.quant,
        model: event.model,
        sizeBytes: event.size_bytes,
        fit: event.fit,
        reason: event.reason,
        state: "pending",
        jobId: null,
        message: null,
      });
      break;
    case "model_suggestion_update": {
      const index = next.findLastIndex((entry) => entry.kind === "suggestion" && entry.id === event.id);
      if (index >= 0) {
        const suggestion = next[index] as SuggestionEntry;
        next[index] = {
          ...suggestion,
          state: event.state,
          jobId: event.job_id ?? suggestion.jobId,
          model: event.model ?? suggestion.model,
          message: event.message,
        };
      }
      break;
    }
    case "task_resumed":
      next.push({ kind: "notice", message: `Continued ${event.task_id} on ${event.model}: ${event.text.split("\n")[0]}` });
      break;
    case "model_changed":
      next.push({
        kind: "notice",
        message: event.from ? `Switched model from ${event.from} to ${event.to}` : `Model set to ${event.to}`,
      });
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
      {
        kind: "permission",
        agentId,
        requestId: event.request_id,
        tool: event.tool,
        summary: event.summary,
        suggestedRule: event.suggested_rule,
        answered: false,
      },
    ];
  }

  if (event.type === "questions_asked") {
    entries = [...entries, { kind: "questions", requestId: event.request_id, questions: event.questions, answered: false }];
  }
  if (event.type === "questions_answered") {
    const requestId = event.request_id;
    entries = entries.map((entry) =>
      entry.kind === "questions" && entry.requestId === requestId ? { ...entry, answered: true } : entry,
    );
  }

  let { summary, tasks, plan, calls, breakdowns, busy, queue, browser, status, statusDetail, pause } = state;
  if (event.type === "model_changed" && agentId === ROOT) summary = { ...summary, model: event.to };
  if (event.type === "status_changed") {
    status = event.status;
    statusDetail = event.detail;
  }
  if (event.type === "paused") pause = { untilMs: event.until_unix_ms, reason: event.reason, cause: event.cause };
  if (event.type === "resumed") pause = null;
  if (event.type === "browser_view") {
    browser = { url: event.url, title: event.title, screenshot: event.screenshot };
  }
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
  if (event.type === "context_breakdown") {
    breakdowns = {
      ...breakdowns,
      [agentId]: {
        at: Date.now(),
        agentId,
        model: event.model,
        contextWindow: event.context_window,
        method: event.method,
        categories: event.categories,
        promptTokens: event.prompt_tokens,
        cachedTokens: event.cached_tokens,
      },
    };
  }

  if (agentId === ROOT) {
    entries = applyToEntries(entries, agentEvent);
    if (event.type === "plan_updated") plan = event.plan;
    if (event.type === "queue_changed") queue = event.messages;
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
    // A finished subagent given a follow-up works again with its earlier conversation.
    if (event.type === "task_resumed" && tasks[event.task_id]) {
      const task = tasks[event.task_id];
      tasks = { ...tasks, [event.task_id]: { ...task, status: "running", model: event.model, report: "" } };
    }
  } else if (tasks[agentId]) {
    const task = tasks[agentId];
    // A resumed task's cost adds to what it cost before.
    const cost = (more: number | null) => (more === null ? task.costUsd : (task.costUsd ?? 0) + more);
    const updated: TaskState =
      event.type === "task_finished"
        ? { ...task, status: event.ok ? "finished" : "failed", report: event.report, costUsd: cost(event.cost_usd) }
        : { ...task, entries: applyToEntries(task.entries, agentEvent) };
    tasks = { ...tasks, [agentId]: updated };
  }
  return { ...state, summary, entries, tasks, plan, calls, breakdowns, busy, queue, browser, status, statusDetail, pause };
}

/** Tokens a breakdown's request held: the categories, which add up to the reported size. */
export const breakdownUsed = (breakdown: ContextBreakdown): number =>
  breakdown.categories.reduce((sum, category) => sum + category.tokens, 0);

/** A token count as `12.3k` or `1.2M`, or as is below a thousand. */
export function formatTokens(n: number): string {
  const size = Math.abs(n);
  if (size >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (size >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
  return String(n);
}

/** `12.3k / 32.8k tokens`, or `12.3k tokens` when the window is unknown. */
export function describeUse(breakdown: ContextBreakdown): string {
  const used = formatTokens(breakdownUsed(breakdown));
  return breakdown.contextWindow ? `${used} / ${formatTokens(breakdown.contextWindow)} tokens` : `${used} tokens`;
}

export function markAnswered(state: SessionState, requestId: number): SessionState {
  return {
    ...state,
    entries: state.entries.map((entry) =>
      entry.kind === "permission" && entry.requestId === requestId ? { ...entry, answered: true } : entry,
    ),
  };
}

export function markQuestionsAnswered(state: SessionState, requestId: number): SessionState {
  return {
    ...state,
    entries: state.entries.map((entry) =>
      entry.kind === "questions" && entry.requestId === requestId ? { ...entry, answered: true } : entry,
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

/** What mods (`sessions:read`) see of each session: its agents and what each is doing now. */
export function snapshotSessions(sessions: SessionState[], environments: EnvironmentInfo[] = []): SessionSnapshot[] {
  return sessions.map((session) => {
    const waiting = (agentId: string) =>
      session.entries.some((entry) => entry.kind === "permission" && entry.agentId === agentId && !entry.answered);
    const rootStatus: AgentSnapshot["status"] = waiting(ROOT)
      ? "waiting"
      : session.busy || session.status === "working"
        ? "working"
        : "idle";
    const agents: AgentSnapshot[] = [{ agentId: ROOT, parentId: null, brief: "", status: rootStatus }];
    for (const task of Object.values(session.tasks)) {
      agents.push({
        agentId: task.taskId,
        parentId: ROOT,
        brief: task.brief,
        status: task.status === "running" ? (waiting(task.taskId) ? "waiting" : "working") : task.status,
      });
    }
    return {
      sessionId: session.summary.session_id,
      title: sessionTitle(session),
      projectDir: session.summary.project_dir,
      status: session.status,
      environmentId: session.summary.environment_id,
      archived: session.summary.archived,
      persistent: session.summary.persistent,
      pet: environments.find((env) => env.id === session.summary.environment_id)?.pet ?? null,
      createdAt: session.summary.created_at ?? null,
      agents,
    };
  });
}
