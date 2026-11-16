// One daemon setting from `config.describe`, with the control for its kind.

import { useEffect, useState, type ReactNode } from "react";
import type { ConfigEntry } from "@imperium/protocol";
import { Badge } from "../components/Badge";
import { Select } from "../components/Select";
import { Row } from "../components/SettingsRow";
import { Switch as Toggle } from "../components/Switch";

const SOURCE_LABEL: Record<ConfigEntry["source"], string> = {
  default: "",
  user: "",
  app: "set in the model configuration",
  project: "set by the project",
  env: "set by an environment variable",
};

/** One daemon setting; text fields save when they lose focus or on Enter. */
export function ConfigRow({ entry, disabled, onSet }: { entry: ConfigEntry; disabled: boolean; onSet: (key: string, value: unknown) => void }) {
  const text = (value: unknown) =>
    Array.isArray(value) ? value.join("\n") : value === null || value === undefined ? "" : String(value);
  const [draft, setDraft] = useState(text(entry.value));
  useEffect(() => setDraft(text(entry.value)), [entry.value]);

  const commit = () => {
    if (draft === text(entry.value)) return;
    const trimmed = draft.trim();
    switch (entry.kind) {
      case "integer":
      case "number": {
        if (trimmed === "") return onSet(entry.key, null);
        const number = Number(trimmed);
        if (Number.isNaN(number)) return setDraft(text(entry.value));
        return onSet(entry.key, entry.kind === "integer" ? Math.round(number) : number);
      }
      case "path_list":
      case "string_list":
        return onSet(
          entry.key,
          draft
            .split("\n")
            .map((line) => line.trim())
            .filter(Boolean),
        );
      default:
        return onSet(entry.key, trimmed === "" ? null : trimmed);
    }
  };

  const overridden = SOURCE_LABEL[entry.source];
  const badge = (
    <>
      {overridden && <Badge>{overridden}</Badge>}
      {entry.source === "user" && (
        <button className="ghost reset-link" onClick={() => onSet(entry.key, null)} title="Remove from your config file">
          reset
        </button>
      )}
    </>
  );
  const description = `${entry.description} (${entry.key})`;

  let control: ReactNode;
  if (entry.kind === "bool") {
    control = <Toggle checked={entry.value === true} onChange={(next) => onSet(entry.key, next)} label={entry.label} disabled={disabled} />;
  } else if (entry.kind === "enum") {
    control = (
      <Select
        value={text(entry.value)}
        options={entry.options.map((option) => ({
          value: option,
          label: humanize(option),
          description: option === text(entry.default) ? "Default" : undefined,
        }))}
        onChange={(next) => onSet(entry.key, next)}
        disabled={disabled}
        placeholder="—"
        aria-label={entry.label}
      />
    );
  } else if (entry.kind === "path_list" || entry.kind === "string_list") {
    control = (
      <textarea
        className="field"
        value={draft}
        rows={Math.min(Math.max(draft.split("\n").length, 2), 6)}
        placeholder="One per line"
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        disabled={disabled}
        aria-label={entry.label}
      />
    );
  } else {
    control = (
      <input
        className={`field${entry.kind === "integer" || entry.kind === "number" ? " numeric" : ""}`}
        type={entry.kind === "integer" || entry.kind === "number" ? "number" : "text"}
        spellCheck={false}
        value={draft}
        placeholder={text(entry.default) || (entry.kind === "secret_ref" ? "secret name" : "not set")}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => event.key === "Enter" && (event.target as HTMLInputElement).blur()}
        disabled={disabled}
        aria-label={entry.label}
      />
    );
  }
  return (
    <Row label={entry.label} description={description} badge={badge}>
      {control}
    </Row>
  );
}

/** `wait_for_reset` → "Wait for reset"; values with dots or digits (q8_0, cuda12) stay as they are. */
function humanize(option: string): string {
  if (/[0-9.]/.test(option) || option.length <= 3) return option;
  const words = option.replace(/[_-]+/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}
