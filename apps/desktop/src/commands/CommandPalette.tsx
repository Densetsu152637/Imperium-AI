// The command palette: Ctrl+K (Cmd+K on macOS) opens a search over every registered command
// (registry.ts) in the main window and in panel windows. Arrow keys move, Enter runs, Tab and
// Shift+Tab jump between groups, Escape closes; the mouse works too. Recently run commands
// lead the list. A panel window sends the chosen command to the main window, which runs it,
// remembers it as recent, and comes forward; see docs/07-desktop-ui.md#command-palette-implemented.

import { Search } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";

import { getLayout, updateLayout } from "../layout/state";
import { focusMainWindow } from "../notify";
import { emitPanelEvent, listenPanelEvents } from "../panelWindows";
import { GROUP_LABELS, groupResults, nextGroupStart, pushRecent, rankCommands } from "./rank";
import { allCommands, findCommand, formatShortcut, registerCommands, type Command, type CommandSource } from "./registry";

/** The most results listed at once; typing narrows the rest. */
const RESULT_LIMIT = 200;

/**
 * Registers a source of commands for as long as the component is mounted. The palette calls
 * `build` when it opens, so it always sees the component's latest state and handlers.
 */
export function useRegisterCommands(name: string, build: CommandSource): void {
  const latest = useRef(build);
  latest.current = build;
  useEffect(() => registerCommands(name, () => latest.current()), [name]);
}

/** Runs a command in this (the main) window and remembers it as recently run. */
function execute(command: Command): void {
  updateLayout({ recentCommands: pushRecent(getLayout().recentCommands ?? [], command.id) });
  command.run();
}

/** Mount once per window. `detached`: this is a panel window, which hands commands to the main window. */
export function CommandPalette({ detached }: { detached: boolean }) {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k") {
        event.preventDefault();
        event.stopPropagation();
        setOpen((now) => !now);
      }
    };
    // Capture, so a text box that handles its own keys cannot swallow the shortcut.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // The main window runs what panel windows' palettes chose.
  useEffect(() => {
    if (detached) return;
    return listenPanelEvents((event) => {
      if (event.kind !== "command") return;
      const command = findCommand(event.command);
      if (!command) return;
      if (!command.stayInWindow) void focusMainWindow();
      execute(command);
    });
  }, [detached]);

  const choose = (command: Command) => {
    setOpen(false);
    if (detached) emitPanelEvent({ kind: "command", command: command.id });
    else execute(command);
  };

  return open ? <PaletteDialog onClose={() => setOpen(false)} onChoose={choose} /> : null;
}

function PaletteDialog({ onClose, onChoose }: { onClose: () => void; onChoose: (command: Command) => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  // What there is to run, as of opening.
  const commands = useMemo(allCommands, []);
  const recent = useMemo(() => getLayout().recentCommands ?? [], []);
  const groups = useMemo(
    () => groupResults(rankCommands(commands, query, recent).slice(0, RESULT_LIMIT), query, recent),
    [commands, query, recent],
  );
  const flat = useMemo(() => groups.flatMap((group) => group.items), [groups]);

  // A modal dialog sits above everything, other dialogs included, and keeps focus inside;
  // closing gives focus back to where it was.
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const element = dialog.current;
    if (element && !element.open) element.showModal();
    input.current?.focus();
    return () => previous?.focus();
  }, []);

  useEffect(() => setActive(0), [query]);

  useEffect(() => {
    list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [active]);

  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    const count = flat.length;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActive((now) => (count ? (now + 1) % count : 0));
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((now) => (count ? (now - 1 + count) % count : 0));
        break;
      case "PageDown":
        event.preventDefault();
        setActive((now) => Math.min(now + 8, Math.max(count - 1, 0)));
        break;
      case "PageUp":
        event.preventDefault();
        setActive((now) => Math.max(now - 8, 0));
        break;
      case "Tab":
        event.preventDefault();
        setActive((now) => nextGroupStart(groups.map((group) => group.items.length), now, event.shiftKey ? -1 : 1));
        break;
      case "Enter":
        event.preventDefault();
        if (flat[active]) onChoose(flat[active]);
        break;
      case "Escape":
        // Only the palette closes, not a window under it that also listens for Escape.
        event.preventDefault();
        event.stopPropagation();
        onClose();
        break;
    }
  };

  let index = 0;
  return (
    <dialog
      ref={dialog}
      className="command-palette"
      aria-label="Command palette"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      // A click on the backdrop (the dialog itself, outside its content) closes it.
      onClick={(event) => event.target === dialog.current && onClose()}
    >
      <div className="command-palette-search">
        <Search aria-hidden="true" />
        <input
          ref={input}
          role="combobox"
          aria-expanded="true"
          aria-controls="command-palette-list"
          aria-activedescendant={flat.length ? `command-palette-${active}` : undefined}
          aria-autocomplete="list"
          placeholder="Search commands, sessions, environments, models, settings…"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={onKeyDown}
          spellCheck={false}
          autoComplete="off"
        />
        <kbd>Esc</kbd>
      </div>
      <div className="command-palette-list" id="command-palette-list" role="listbox" aria-label="Commands" ref={list}>
        {groups.map((group) => (
          <div key={group.group} role="group" aria-labelledby={`command-palette-group-${group.group}`}>
            <div className="command-palette-group" id={`command-palette-group-${group.group}`}>
              {GROUP_LABELS[group.group]}
            </div>
            {group.items.map((command) => {
              const at = index++;
              const Icon = command.icon;
              return (
                <div
                  key={command.id}
                  id={`command-palette-${at}`}
                  data-index={at}
                  role="option"
                  aria-selected={at === active}
                  className={at === active ? "command-palette-item active" : "command-palette-item"}
                  onMouseMove={() => at !== active && setActive(at)}
                  onClick={() => onChoose(command)}
                >
                  <span className="command-palette-icon">{Icon && <Icon aria-hidden="true" />}</span>
                  <span className="command-palette-text">
                    <span className="command-palette-title">{command.title}</span>
                    {command.subtitle && <span className="command-palette-subtitle">{command.subtitle}</span>}
                  </span>
                  {command.shortcut && <kbd>{formatShortcut(command.shortcut)}</kbd>}
                </div>
              );
            })}
          </div>
        ))}
        {flat.length === 0 && <p className="muted command-palette-empty">Nothing matches “{query}”.</p>}
      </div>
      <footer className="command-palette-hints muted">
        <span>
          <kbd>↑</kbd> <kbd>↓</kbd> move
        </span>
        <span>
          <kbd>Enter</kbd> run
        </span>
        <span>
          <kbd>Tab</kbd> next group
        </span>
        <span>
          <kbd>{formatShortcut("Mod+K")}</kbd> open or close
        </span>
      </footer>
    </dialog>
  );
}
