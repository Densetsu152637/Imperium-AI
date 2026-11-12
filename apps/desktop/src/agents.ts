// Environment agents as the desktop names them (pure, so the rules are tested without a window).

import type {
  AgentPermissions,
  EnvironmentAgent,
  EnvironmentAgentCreated,
  EnvironmentInfo,
  ToolAccess,
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

/** No extra restriction: what an agent has unless the user limits it. */
export const DEFAULT_PERMISSIONS: AgentPermissions = {
  mcp: "all",
  mcp_servers: [],
  plugins: "all",
  plugin_names: [],
  local_files_only: false,
};

/** Names typed in a list field (commas or line breaks), trimmed, without repeats (ignoring case) or blanks. */
export function parseNames(text: string): string[] {
  const seen = new Set<string>();
  const names: string[] = [];
  for (const part of text.split(/[,\n]/)) {
    const name = part.trim();
    if (name && !seen.has(name.toLowerCase())) {
      seen.add(name.toLowerCase());
      names.push(name);
    }
  }
  return names;
}

/** An agent's permission profile as the editor holds it while the user changes it. */
export type PermissionDraft = {
  mcp: ToolAccess;
  mcpNames: string;
  plugins: ToolAccess;
  pluginNames: string;
  localOnly: boolean;
};

export const draftOfPermissions = (permissions: AgentPermissions | undefined): PermissionDraft => {
  const p = permissions ?? DEFAULT_PERMISSIONS;
  return {
    mcp: p.mcp,
    mcpNames: p.mcp_servers.join(", "),
    plugins: p.plugins,
    pluginNames: p.plugin_names.join(", "),
    localOnly: p.local_files_only,
  };
};

/** The profile to send: names are kept only for "named", and "named" with no name means none. */
export function permissionsOfDraft(draft: PermissionDraft): AgentPermissions {
  const list = (access: ToolAccess, text: string): [ToolAccess, string[]] => {
    if (access !== "named") return [access, []];
    const names = parseNames(text);
    return names.length === 0 ? ["none", []] : ["named", names];
  };
  const [mcp, mcp_servers] = list(draft.mcp, draft.mcpNames);
  const [plugins, plugin_names] = list(draft.plugins, draft.pluginNames);
  return { mcp, mcp_servers, plugins, plugin_names, local_files_only: draft.localOnly };
}

/** What a profile restricts, in a few words for the agent's row; null when it restricts nothing. */
export function describePermissions(permissions: AgentPermissions | undefined): string | null {
  const p = permissions ?? DEFAULT_PERMISSIONS;
  if (p.local_files_only) return "Local files only";
  const part = (label: string, access: ToolAccess, names: string[]) =>
    access === "all" ? null : `${label}: ${access === "none" ? "none" : names.join(", ")}`;
  const parts = [part("MCP", p.mcp, p.mcp_servers), part("Plugins", p.plugins, p.plugin_names)].filter(
    (text): text is string => text !== null,
  );
  return parts.length === 0 ? null : parts.join(" · ");
}
