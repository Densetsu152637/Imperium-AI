// Environment agents as the desktop names them (pure, so the rules are tested without a window).

import type { EnvironmentAgent, EnvironmentAgentCreated, EnvironmentInfo } from "@imperium/protocol";

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
    return (
      code < 0x20 ||
      (code >= 0x7f && code <= 0x9f) ||
      code === 0x2028 ||
      code === 0x2029 ||
      (code >= 0x202a && code <= 0x202e) ||
      (code >= 0x2066 && code <= 0x2069)
    );
  });
  if (control) return "An agent's name cannot contain line breaks, bidirectional controls or other control characters.";
  return null;
}

/** What to tell the user after adding an agent that did not start (it starts once the cause is fixed). */
export const notStartedNotice = (created: EnvironmentAgentCreated | null, did = "was added"): string | null =>
  created && !created.started
    ? `“${created.name}” ${did} but is not running yet: ${created.reason ?? "it has not started"}`
    : null;

/** The agent whose session is `sessionId` in `environment`, if any. */
export const agentOfSession = (environment: EnvironmentInfo | null | undefined, sessionId: string) =>
  environment?.agents?.find((agent) => agent.session_id === sessionId) ?? null;

/** How a persistent agent's session is titled in lists: "Work · Planner" (as the daemon titles it). */
export const agentTitle = (environmentName: string, agent: EnvironmentAgent | null) =>
  agent ? `${environmentName} · ${agent.name}` : `${environmentName} agent`;
