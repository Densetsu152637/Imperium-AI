// The row that moves the working folders' place.

import { useEffect, useState } from "react";
import type { ConfigEntry, WorkspacesProgress } from "@imperium/protocol";
import { Row } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";
import { pickFolder } from "../daemon/connection";

/**
 * `workspaces.root`: the folder that holds every environment's working folder. A folder picker
 * beside the path; saving moves what is there (the daemon refuses a folder that is not empty, or
 * while a session is working), so the row shows the move's progress and keeps the old value when it
 * fails.
 */
export function WorkspacesRootRow({
  entry,
  client,
  onSet,
}: {
  entry: ConfigEntry;
  client: DaemonClient;
  onSet: (key: string, value: unknown) => Promise<void>;
}) {
  const shown = typeof entry.value === "string" && entry.value ? entry.value : String(entry.default ?? "");
  const [draft, setDraft] = useState(shown);
  const [progress, setProgress] = useState<string | null>(null);
  useEffect(() => setDraft(shown), [shown]);
  useEffect(
    () =>
      client.onNotification((notification) => {
        if (notification.method !== "workspaces.progress") return;
        const step = notification.params as WorkspacesProgress;
        setProgress(step.files_total > 0 ? `${step.phase} ${step.files_done} of ${step.files_total} files…` : `${step.phase}…`);
      }),
    [client],
  );
  const move = async (value: string | null) => {
    setProgress("Moving the working folders…");
    try {
      await onSet(entry.key, value);
    } finally {
      setProgress(null);
    }
  };
  const commit = () => {
    const trimmed = draft.trim();
    if (trimmed === shown || (trimmed === "" && entry.source !== "user")) return setDraft(shown);
    void move(trimmed === "" ? null : trimmed);
  };
  const badge =
    entry.source === "user" ? (
      <button className="ghost reset-link" disabled={progress !== null} onClick={() => void move(null)} title="Move them back to the default folder">
        reset
      </button>
    ) : null;
  return (
    <Row label={entry.label} description={`${entry.description} (${entry.key})`} badge={badge}>
      <div className="field-with-action">
        <input
          className="field"
          spellCheck={false}
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => event.key === "Enter" && (event.target as HTMLInputElement).blur()}
          disabled={progress !== null}
          aria-label={entry.label}
        />
        <button
          disabled={progress !== null}
          onClick={() =>
            void pickFolder("Move the working folders to").then((folder) => folder && void move(folder))
          }
        >
          Choose folder…
        </button>
      </div>
      {progress && <span className="muted" role="status">{progress}</span>}
    </Row>
  );
}
