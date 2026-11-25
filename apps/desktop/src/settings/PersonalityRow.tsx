// The row that picks the root agent's personality.

import { useEffect, useState } from "react";
import type { ConfigEntry, PersonalitiesListResult } from "@imperium/protocol";
import { Select } from "../components/Select";
import { Row } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";

/** The value of `policy.personality` that turns personalities off. */
const NO_PERSONALITY = "none";

/**
 * `policy.personality` as a select of the personalities the daemon finds (local ones, which win,
 * then the workflow packs' and the built-in ones), the default marked, plus None. A value the
 * daemon does not find stays listed, marked as falling back to the default.
 */
export function PersonalityRow({
  entry,
  client,
  onSet,
}: {
  entry: ConfigEntry;
  client: DaemonClient;
  onSet: (key: string, value: unknown) => void;
}) {
  const [list, setList] = useState<PersonalitiesListResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    client
      .call<PersonalitiesListResult>("personalities.list", {})
      .then(setList)
      .catch((reason: unknown) => setError(String(reason instanceof Error ? reason.message : reason)));
  }, [client]);

  const fallback = list?.default ?? String(entry.default ?? "");
  const raw = typeof entry.value === "string" ? entry.value.trim() : "";
  const value = raw === "" || raw.toLowerCase() === NO_PERSONALITY ? NO_PERSONALITY : raw;
  const options = [
    ...(list?.personalities ?? []).map((personality) => ({
      value: personality.id,
      label: personality.name,
      description: [
        personality.id === fallback ? "Default" : null,
        personality.source === "local" ? (personality.overrides ? "Local, overrides the pack's" : "Local") : null,
      ]
        .filter(Boolean)
        .join(" · ") || undefined,
    })),
    { value: NO_PERSONALITY, label: "None", description: "Plain replies with no personality" },
  ];
  if (list && !options.some((option) => option.value === value)) {
    options.unshift({ value, label: value, description: `Not found: ${fallback} is used` });
  }
  const badge =
    entry.source === "user" ? (
      <button className="ghost reset-link" onClick={() => onSet(entry.key, null)} title="Remove from your config file">
        reset
      </button>
    ) : null;
  const description = `${entry.description} (${entry.key})${error ? ` Could not list personalities: ${error}` : ""}`;
  return (
    <Row label={entry.label} description={description} badge={badge}>
      <Select
        value={value}
        options={options}
        onChange={(next) => onSet(entry.key, next)}
        disabled={!list}
        placeholder={list ? "—" : "Loading…"}
        aria-label={entry.label}
      />
    </Row>
  );
}
