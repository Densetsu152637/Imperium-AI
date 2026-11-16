// Settings → Keyboard: the rebindable shortcuts (shortcuts.ts) and the fixed ones. "Change" waits
// for the next key combination; a combination another action or a fixed use already holds is
// refused with the action named, and Escape cancels. Only changed bindings are saved.

import { useEffect, useState } from "react";

import { formatShortcut } from "../commands/registry";
import { reservedChords, type AppSettings } from "../settings/settings";
import {
  SHORTCUT_ACTIONS,
  bindingFor,
  chordFromEvent,
  chordProblem,
  findConflict,
  setCapturing as setCapturingFlag,
  withBinding,
  withoutBinding,
  type ShortcutId,
} from "../settings/shortcuts";
import { Kbd } from "./Kbd";

/** Fixed shortcuts: listed for reference, not rebindable. Each is the chords that do it (any one of them), as keys. */
const FIXED: { id: string; chords: string[][]; action: string }[] = [
  { id: "palette", chords: [["↑"], ["↓"], ["Enter"], ["Tab"], ["Esc"]], action: "In the palette: move, run the command, jump to the next group (Shift+Tab: previous), close" },
  { id: "close", chords: [["Esc"]], action: "Close Settings" },
  { id: "send", chords: [["Enter"], ["Ctrl", "Enter"]], action: "Send a message (see Chat)" },
  { id: "newline", chords: [["Shift", "Enter"]], action: "New line in a message" },
];

/** A formatted chord ("Ctrl+K", "⌘⇧K") as one key per chip. */
function chordKeys(text: string): string[] {
  const symbols = /^[⌘⇧⌥⌃]+/.exec(text)?.[0] ?? "";
  const rest = text.slice(symbols.length);
  return [...symbols, ...(rest ? rest.split(/\+(?!$)/) : [])];
}

export function ShortcutsSection({ settings, onChange }: { settings: AppSettings; onChange: (settings: AppSettings) => void }) {
  const [capturing, setCapturing] = useState<ShortcutId | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!capturing) return;
    // The other shortcut handlers stand down while a binding is being chosen.
    setCapturingFlag(true);
    const cancel = () => {
      setCapturing(null);
      setMessage(null);
    };
    const onKey = (event: KeyboardEvent) => {
      // Tab moves on (leaving the button cancels); a key held down from before does not count.
      if (event.key === "Tab" && !event.ctrlKey && !event.metaKey && !event.altKey) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.repeat) return;
      if (event.key === "Escape" && !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey) {
        setCapturing(null);
        setMessage(null);
        return;
      }
      const chord = chordFromEvent(event);
      if (!chord) {
        // Only modifiers so far, unless AltGr is down: say why that cannot be a shortcut.
        if (event.getModifierState("AltGraph") && !["Control", "Alt", "AltGraph", "Meta", "Shift"].includes(event.key)) {
          setMessage("AltGr combinations can't be shortcuts.");
        }
        return;
      }
      const problem = chordProblem(chord);
      const conflict = problem ? null : findConflict(settings.shortcuts, capturing, chord, reservedChords(settings));
      if (problem || conflict) {
        setMessage(problem ?? `${formatShortcut(chord)} is already used by ${conflict!.label}. Press another combination, or Esc.`);
        return;
      }
      onChange({ ...settings, shortcuts: withBinding(settings.shortcuts, capturing, chord) });
      setCapturing(null);
      setMessage(null);
    };
    // Capture, so the shortcut being replaced does not also run.
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("blur", cancel);
    return () => {
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("blur", cancel);
      setCapturingFlag(false);
    };
  }, [capturing, settings, onChange]);

  return (
    <>
      <div className="settings-table-scroll">
        <table className="table settings-table shortcuts-table">
          <thead>
            <tr>
              <th scope="col">Action</th>
              <th scope="col">Shortcut</th>
              <th scope="col">
                <span className="visually-hidden">Change or reset</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {SHORTCUT_ACTIONS.map((action) => {
              const changed = settings.shortcuts[action.id] !== undefined;
              const waiting = capturing === action.id;
              return (
                <tr key={action.id}>
                  <td className="shortcut-action">
                    <span className="settings-row-label">{action.label}</span>
                    <span className="settings-row-description">{action.description}</span>
                  </td>
                  <td className="shortcut-keys">
                    {waiting ? <Kbd>Press the new keys…</Kbd> : <Kbd keys={chordKeys(formatShortcut(bindingFor(settings.shortcuts, action.id)))} />}
                    <span role="status" className="shortcut-status">
                      {waiting && message ? message : ""}
                    </span>
                  </td>
                  <td className="shortcut-buttons">
                    <button
                      onBlur={() => waiting && setCapturing(null)}
                      onClick={() => {
                        setCapturing(waiting ? null : action.id);
                        setMessage(null);
                      }}
                    >
                      {waiting ? "Cancel" : "Change"}
                    </button>
                    {changed && (
                      <button
                        className="ghost reset-link"
                        title={`Back to ${formatShortcut(action.default)}`}
                        onClick={() => onChange({ ...settings, shortcuts: withoutBinding(settings.shortcuts, action.id) })}
                      >
                        Reset
                      </button>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <section className="settings-section" aria-label="Fixed shortcuts">
        <h3 className="settings-subhead">Fixed shortcuts</h3>
        <div className="settings-table-scroll">
          <table className="table settings-table shortcuts-table">
            <tbody>
              {FIXED.map((fixed) => (
                <tr key={fixed.id}>
                  <td className="shortcut-fixed-keys">
                    {fixed.chords.map((chord, index) => (
                      <span key={chord.join("+")} className="shortcut-alternative">
                        {index > 0 && <span className="shortcut-or" aria-hidden="true">/</span>}
                        <Kbd keys={chord} />
                      </span>
                    ))}
                  </td>
                  <td>{fixed.action}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>
    </>
  );
}
