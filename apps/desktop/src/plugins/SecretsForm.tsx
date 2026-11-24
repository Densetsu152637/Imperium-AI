// Secret fields for one plugin: a write-only password input per variable its servers need. A saved
// value is never shown, echoed or returned: the row says only "Saved" or "Missing". Typing a new
// value replaces the saved one; the input is cleared once the daemon has taken it.

import { KeyRound } from "lucide-react";
import { useState } from "react";
import type { InstalledPlugin } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { Field } from "../components/Field";
import { plain } from "./model";

export function SecretsForm({
  plugin,
  disabled,
  onSet,
  onDelete,
}: {
  plugin: InstalledPlugin;
  disabled: boolean;
  /** Resolves true when the daemon took the value. */
  onSet: (variable: string, value: string) => Promise<boolean>;
  onDelete: (variable: string) => Promise<boolean>;
}) {
  const [typed, setTyped] = useState<Record<string, string>>({});
  const missing = new Set(plugin.missing_secrets);
  if (plugin.required_secrets.length === 0) return null;
  return (
    <div className="plugin-secrets" role="group" aria-label={`Secrets for ${plain(plugin.name)}`}>
      {plugin.required_secrets.map((variable) => {
        const value = typed[variable] ?? "";
        const saved = !missing.has(variable);
        return (
          <form
            key={variable}
            className="secret-row"
            onSubmit={(event) => {
              event.preventDefault();
              if (!value) return;
              void onSet(variable, value).then((ok) => ok && setTyped((all) => ({ ...all, [variable]: "" })));
            }}
          >
            <Field
              label={
                <>
                  <KeyRound aria-hidden="true" /> {plain(variable)} {saved ? <Badge tone="ok">Saved</Badge> : <Badge tone="warn">Missing</Badge>}
                </>
              }
              hint={saved ? "A saved value is never shown. Type a new one to replace it." : "The plugin's server needs this and does not start without it."}
            >
              <input
                className="field"
                type="password"
                autoComplete="off"
                spellCheck={false}
                value={value}
                onChange={(event) => setTyped((all) => ({ ...all, [variable]: event.target.value }))}
                aria-label={`Value for ${plain(variable)}`}
              />
            </Field>
            <div className="secret-row-actions">
              <button type="submit" className="small" disabled={disabled || !value}>
                Save
              </button>
              {saved && (
                <button type="button" className="small" disabled={disabled} onClick={() => void onDelete(variable)}>
                  Delete saved
                </button>
              )}
            </div>
          </form>
        );
      })}
    </div>
  );
}
