// Environment agents as the desktop names them (pure, so the rules are tested without a window).

import type { EnvironmentAgent, EnvironmentInfo } from "@imperium/protocol";

/** Whether `name` repeats an agent's name (ignoring case and padding), other than `except`'s. */
export const nameTaken = (agents: EnvironmentAgent[], name: string, except?: string) =>
  agents.some((agent) => agent.id !== except && agent.name.trim().toLowerCase() === name.trim().toLowerCase());

/** The agent whose session is `sessionId` in `environment`, if any. */
export const agentOfSession = (environment: EnvironmentInfo | null | undefined, sessionId: string) =>
  environment?.agents?.find((agent) => agent.session_id === sessionId) ?? null;

/** How a persistent agent's session is titled in lists: "Work · Planner". */
export const agentTitle = (environmentName: string, agent: EnvironmentAgent | null) =>
  agent ? `${environmentName} · ${agent.name}` : `${environmentName} agent`;
