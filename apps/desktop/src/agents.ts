// Environment agents as the desktop names them (pure, so the rules are tested without a window).

import type {
  EnvironmentAgent,
  EnvironmentAgentCreated,
  EnvironmentInfo,
  Initiative,
  InitiativeState,
  StartedBy,
  WorkSuggestion,
} from "@imperium/protocol";


/** The longest agent name, in characters (code points, as the daemon counts them). */
export const MAX_AGENT_NAME = 60;

/** An agent name as compared for uniqueness: padding removed, lower-cased with Unicode rules. */
const fold = (name: string) => name.trim().toLowerCase();

/** Whether `name` repeats an agent's name (ignoring case and padding), other than `except`'s. */
export const nameTaken = (agents: EnvironmentAgent[], name: string, except?: string) =>
  agents.some((agent) => agent.id !== except && fold(agent.name) === fold(name));

/** A name's length in characters as the daemon counts it (an emoji is one, not two). */
export const nameLength = (name: string) => [...name.trim()].length;

/** Why `name` cannot be an agent's name (before the daemon checks it too), or null. */
export function nameProblem(name: string): string | null {
  const length = nameLength(name);
  if (length === 0) return "An agent needs a name.";
  if (length > MAX_AGENT_NAME) return `An agent's name is at most ${MAX_AGENT_NAME} characters.`;
  const control = [...name].some((c) => {
    const code = c.codePointAt(0) ?? 0;
    return code < 0x20 || (code >= 0x7f && code <= 0x9f);
  });
  if (control) return "An agent's name cannot contain line breaks or control characters.";
  return null;
}

/** What to tell the user after adding an agent that did not start (it starts once the cause is fixed). */
export const notStartedNotice = (created: EnvironmentAgentCreated | null): string | null =>
  created && !created.started
    ? `“${created.name}” was added but is not running yet: ${created.reason ?? "it has not started"}`
    : null;

/** The agent whose session is `sessionId` in `environment`, if any. */
export const agentOfSession = (environment: EnvironmentInfo | null | undefined, sessionId: string) =>
  environment?.agents?.find((agent) => agent.session_id === sessionId) ?? null;

/** How a persistent agent's session is titled in lists: "Work · Planner" (as the daemon titles it). */
export const agentTitle = (environmentName: string, agent: EnvironmentAgent | null) =>
  agent ? `${environmentName} · ${agent.name}` : `${environmentName} agent`;

/** What each initiative means, for the select beside an agent. */
export const INITIATIVE_OPTIONS: { value: Initiative; label: string; description: string }[] = [
  { value: "off", label: "Off", description: "The agent does nothing unless you message it." },
  {
    value: "suggest",
    label: "Suggest",
    description: "When the machine and this environment are idle it reviews the roadmap, plans and sessions and suggests work to start with one click, and reminds you what is left.",
  },
  {
    value: "act",
    label: "Act",
    description: "As Suggest, and it may start a few sessions itself, worded as you would, in default permission mode, and tells you each time.",
  },
];

/** The suggestions waiting for the user, newest first (the daemon sends open ones first). */
export const openSuggestions = (state: InitiativeState | null): WorkSuggestion[] =>
  (state?.suggestions ?? []).filter((suggestion) => suggestion.status === "open");

/** How long ago, in words: "just now", "5 min ago", "2 h ago", "3 d ago". (Imports stay type-only here so
 * the module loads in the tests.) */
function since(at: number, now: number): string {
  const minutes = Math.max(0, Math.floor((now - at) / 60_000));
  if (minutes < 2) return "just now";
  if (minutes < 120) return `${minutes} min ago`;
  if (minutes < 2880) return `${Math.floor(minutes / 60)} h ago`;
  return `${Math.floor(minutes / 1440)} d ago`;
}

/** One line on where an agent's initiative stands: reviewing now, waiting and why, or when it last looked. */
export function initiativeHeadline(state: InitiativeState, now = Date.now()): string {
  if (state.mode === "off") return "Initiative is off.";
  if (state.reviewing) return "Reviewing now…";
  const last = state.last_review_at ? `Last review ${since(state.last_review_at, now)}.` : "No review yet.";
  const waiting = state.waiting ? ` Waiting: ${state.waiting}.` : "";
  const failed = state.last_error ? ` The last review failed: ${state.last_error}` : "";
  return `${last}${waiting}${failed}`;
}

/** The limits in act mode: "Started today 1 of 2 · running 0 of 1". */
export const actLimits = (state: InitiativeState): string =>
  `Started today ${state.started_today} of ${state.max_per_day} · running ${state.running} of ${state.max_concurrent}`;

/** The note on a session an agent started (or the user started from its suggestion). */
export function startedByNote(by: StartedBy): string {
  const how =
    by.kind === "initiative" ? "on its own initiative" : by.kind === "suggestion" ? "from its suggestion" : "while working for you";
  const source = by.source ? ` (from ${by.source})` : "";
  return `Started by ${by.agent_name} in ${by.environment_name} ${how}: ${by.reason}${source}`;
}
