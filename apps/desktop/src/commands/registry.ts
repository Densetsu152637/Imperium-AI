// The command registry behind the command palette. Features register a source: a function
// that lists their commands from the state they hold at that moment, so the palette never runs
// a stale handler. Each window has its own registry (each runs its own copy of the app).

import type { LucideIcon } from "lucide-react";

import type { Rankable } from "./rank";

export type Command = Rankable & {
  /** Runs the command; the palette has closed by then. */
  run: () => void;
  icon?: LucideIcon;
  /** A shortcut that does the same, as "Mod+," (Mod: Ctrl, or Cmd on macOS). */
  shortcut?: string;
  /**
   * From a panel window, every command runs in the main window, which comes forward; set this
   * for ones whose effect shows everywhere (the theme), so the panel window stays in front.
   */
  stayInWindow?: boolean;
};

export type CommandSource = () => Command[];

const sources = new Map<string, CommandSource>();

/** Adds a source of commands under `name` (replacing one of that name); returns how to remove it. */
export function registerCommands(name: string, source: CommandSource): () => void {
  sources.set(name, source);
  return () => {
    if (sources.get(name) === source) sources.delete(name);
  };
}

/** Every registered command as of now, in registration order; a failing source is skipped. */
export function allCommands(): Command[] {
  return [...sources.values()].flatMap((source) => {
    try {
      return source();
    } catch (error) {
      console.warn("a command source failed", error);
      return [];
    }
  });
}

export function findCommand(id: string): Command | undefined {
  return allCommands().find((command) => command.id === id);
}

const MAC = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.userAgent);

/** A shortcut as this platform writes it: "Mod+," is "Ctrl+," here and "⌘," on macOS. */
export function formatShortcut(shortcut: string): string {
  return MAC ? shortcut.replace(/Mod\+/g, "⌘").replace(/Shift\+/g, "⇧").replace(/Alt\+/g, "⌥") : shortcut.replace(/Mod\+/g, "Ctrl+");
}
