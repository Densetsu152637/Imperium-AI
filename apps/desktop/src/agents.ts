// Environment agents as the desktop names them (pure, so the rules are tested without a window).

import type {
  AgentPermissions,
  EnvironmentAgent,
  EnvironmentAgentCreated,
  EnvironmentInfo,
  Initiative,
  InitiativeState,
  StartedBy,
  ToolAccess,
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
    return (
      code < 0x20 ||
      (code >= 0x7f && code <= 0x9f) ||
      code === 0x2028 ||
      code === 0x2029 ||
      code === 0x200e ||
      code === 0x200f ||
      code === 0x061c ||
      (code >= 0x202a && code <= 0x202e) ||
      (code >= 0x2066 && code <= 0x2069)
    );
  });
  if (control) return "An agent's name cannot contain line breaks, bidirectional controls or other control characters.";
  return null;
}

/** What to tell the user after adding an agent that did not start (it starts once the cause is fixed). */
export const notStartedNotice = (created: EnvironmentAgentCreated | null, did = "was added"): string | null =>
  created && created.started === false
    ? `“${created.name}” ${did} but is not running yet: ${created.reason ?? "it has not started"}`
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

/** Whether the user changed the permissions in the form (what they would send differs from what it opened with). */
export const permissionsChanged = (initial: PermissionDraft, current: PermissionDraft): boolean =>
  JSON.stringify(permissionsOfDraft(initial)) !== JSON.stringify(permissionsOfDraft(current));

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
