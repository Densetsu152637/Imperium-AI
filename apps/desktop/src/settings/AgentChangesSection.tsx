// The Agent changes section: what agents changed in the app config, and undoing it.

import { Undo2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import type { ConfigChange, ConfigChangesResult, ConfigUndoResult } from "@imperium/protocol";
import { formatAgo } from "../shared/ago";
import { changeDetails, undoLabel } from "./configChanges";
import { Badge } from "../components/Badge";
import { LoadingState } from "../components/LoadingState";
import { Notice } from "../components/Notice";
import { Row } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";

/** What agents changed in the app config (`config.changes`), newest first, each with Undo (`config.undo`). */
export function AgentChangesSection({ client, onChanged }: { client: DaemonClient; onChanged?: () => void }) {
  const [changes, setChanges] = useState<ConfigChange[] | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const load = useCallback(() => {
    client
      .call<ConfigChangesResult>("config.changes", {})
      .then((result) => setChanges(result.changes))
      .catch((failure: unknown) => setError(String(failure instanceof Error ? failure.message : failure)));
  }, [client]);
  useEffect(load, [load]);

  const undo = async (change: ConfigChange) => {
    setBusy(change.id);
    setError(null);
    setNotice(null);
    try {
      const result = await client.call<ConfigUndoResult>("config.undo", { id: change.id });
      setNotice(result.message);
      load();
      onChanged?.();
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    } finally {
      setBusy(null);
    }
  };

  if (!changes) return error ? <Notice tone="danger">{error}</Notice> : <LoadingState />;
  return (
    <>
      <Row
        label="Changes by agents"
        description="Model cards agents wrote and mods, plugins, and skills they installed or removed. Permission rules for set_model_card, install_extension, and uninstall_extension decide whether agents ask first."
      />
      {changes.length === 0 && <p className="muted">No agent has changed the app config.</p>}
      {changes.map((change) => {
        const details = changeDetails(change);
        const undone = change.undone_at_unix_ms !== null;
        return (
          <Row
            key={change.id}
            label={change.summary}
            badge={undone ? <Badge>undone</Badge> : undefined}
            description={[formatAgo(change.at_unix_ms) && `${formatAgo(change.at_unix_ms)} ago`, ...details].filter(Boolean).join(" · ")}
          >
            <button onClick={() => void undo(change)} disabled={undone || busy !== null} title={undoLabel(change)}>
              <Undo2 aria-hidden="true" /> Undo
            </button>
          </Row>
        );
      })}
      {error && (
        <Notice tone="danger">
          {error}
        </Notice>
      )}
      {notice && <Notice>{notice}</Notice>}
    </>
  );
}
