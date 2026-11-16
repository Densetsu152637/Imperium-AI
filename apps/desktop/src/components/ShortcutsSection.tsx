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
import { Row } from "./SettingsRow";

/** Fixed shortcuts: listed for reference, not rebindable. */
const FIXED: [string, string][] = [
  ["↑ / ↓, Enter, Tab, Esc", "In the palette: move, run the command, jump to the next group (Shift+Tab: previous), close"],
  ["Esc", "Close Settings"],
  ["Enter / Ctrl+Enter", "Send a message (see Chat)"],
  ["Shift+Enter", "New line in a message"],
];

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
      {SHORTCUT_ACTIONS.map((action) => {
        const changed = settings.shortcuts[action.id] !== undefined;
        const waiting = capturing === action.id;
        return (
          <Row key={action.id} label={action.label} description={action.description}>
            <kbd>{waiting ? "Press the new keys…" : formatShortcut(bindingFor(settings.shortcuts, action.id))}</kbd>
            <span role="status" className="muted">
              {waiting && message ? message : ""}
            </span>
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
          </Row>
        );
      })}
      <table className="shortcuts">
        <tbody>
          {FIXED.map(([keys, action]) => (
            <tr key={keys}>
              <td>
                <kbd>{keys}</kbd>
              </td>
              <td>{action}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}
