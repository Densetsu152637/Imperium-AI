// Slash commands typed in a session's composer. A command is sent to the daemon as its own method
// rather than as text, so the daemon owns what it does (`/init` is `session.init`, which sends the
// built-in prompt that writes the project's instruction file).

export type SlashCommand = {
  name: "init";
  /** One line for hints and the documentation. */
  description: string;
};

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: "init",
    description: "Ask the agent to analyse the project and write or update its AGENTS.md",
  },
];

/** A command line the composer recognised: its name and whatever followed it. */
export type ParsedSlash = { command: SlashCommand["name"]; rest: string };

/** `/init` or `/init focus on the tests`; null for anything else, `/initial` included. */
export function parseSlash(text: string): ParsedSlash | null {
  const match = /^\/([a-z]+)(?:\s+([\s\S]*))?$/.exec(text.trim());
  if (!match) return null;
  const command = SLASH_COMMANDS.find((candidate) => candidate.name === match[1]);
  return command ? { command: command.name, rest: (match[2] ?? "").trim() } : null;
}
