// Slash commands typed in a session's composer. A command is sent to the daemon as its own method
// rather than as text, so the daemon owns what it does: `session.builtin` sends the command's
// built-in prompt as an ordinary user message (`/init` writes the project's instruction file,
// `/code-review` and `/security-review` ask for a report of findings and edit nothing).

import type { BuiltinCommand, SessionBuiltinParams } from "@imperium/protocol";

export type SlashCommand = {
  name: "init" | "code-review" | "security-review";
  /** The daemon's name for it (`session.builtin`). */
  builtin: BuiltinCommand;
  /** What may follow the name, for hints and the documentation. */
  argumentHint: string;
  /** One line for hints and the documentation. */
  description: string;
};

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: "init",
    builtin: "init",
    argumentHint: "[guidance]",
    description: "Ask the agent to analyse the project and write or update its AGENTS.md",
  },
  {
    name: "code-review",
    builtin: "code_review",
    argumentHint: "[low|medium|high|max] [PR number | branch | path]",
    description: "Review the pending changes, a branch, a path or a pull request for correctness bugs",
  },
  {
    name: "security-review",
    builtin: "security_review",
    argumentHint: "[low|medium|high|max] [PR number | branch | path]",
    description: "Review the pending changes, a branch, a path or a pull request for security problems",
  },
];

/** A command line the composer recognised: its name and whatever followed it. */
export type ParsedSlash = { command: SlashCommand["name"]; rest: string };

/** `/init`, `/init focus on the tests` or `/code-review high 123`; null for anything else, `/initial` included. */
export function parseSlash(text: string): ParsedSlash | null {
  const match = /^\/([a-z-]+)(?:\s+([\s\S]*))?$/.exec(text.trim());
  if (!match) return null;
  const command = SLASH_COMMANDS.find((candidate) => candidate.name === match[1]);
  return command ? { command: command.name, rest: (match[2] ?? "").trim() } : null;
}

/** The `session.builtin` request for a parsed command line. */
export function builtinParams(sessionId: string, builtin: BuiltinCommand, args: string): SessionBuiltinParams {
  return { session_id: sessionId, command: builtin, ...(args ? { args } : {}) };
}

/** The daemon's name for a command the composer recognised. */
export function builtinOf(name: SlashCommand["name"]): BuiltinCommand {
  return SLASH_COMMANDS.find((candidate) => candidate.name === name)!.builtin;
}
