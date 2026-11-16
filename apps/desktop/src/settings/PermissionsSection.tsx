// The Permissions section: the rules and the permission mode.

import { useEffect, useState } from "react";
import type { PermissionSet, PermissionsResult } from "@imperium/protocol";
import { PermissionsEditor, samePermissions } from "../components/PermissionsEditor";
import { Row } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";

/** Global path grants and permission rules (`permissions.get` / `permissions.set`), saved to the config file. */
export function PermissionsSection({ client }: { client: DaemonClient }) {
  const [saved, setSaved] = useState<PermissionsResult | null>(null);
  const [draft, setDraft] = useState<PermissionSet>({ grants: [], rules: [] });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    client
      .call<PermissionsResult>("permissions.get", {})
      .then((result) => {
        if (!live) return;
        setSaved(result);
        setDraft(result.global);
      })
      .catch((failure: unknown) => live && setError(String(failure instanceof Error ? failure.message : failure)));
    return () => {
      live = false;
    };
  }, [client]);

  if (!saved) return error ? <p className="error">{error}</p> : <p className="muted">Loading…</p>;
  const changed = !samePermissions(draft, saved.global);

  const save = async () => {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const result = await client.call<PermissionsResult>("permissions.set", {
        grants: draft.grants,
        rules: draft.rules.filter((rule) => rule.tool.trim()),
      });
      setSaved(result);
      setDraft(result.global);
      setNotice("Saved. Rules apply to running sessions from their next tool call; paths apply to sessions started from now on.");
    } catch (failure) {
      setError(String(failure instanceof Error ? failure.message : failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Row
        label="Every environment"
        description="Paths and rules here apply to every session, beside its environment's own (edit an environment to add those). Environments in a container, WSL, or over SSH get only their own paths, since these are paths on this computer."
      />
      <PermissionsEditor value={draft} onChange={setDraft} presets={saved.presets} disabled={busy} />
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {notice && !changed && <p className="muted">{notice}</p>}
      <div className="permissions-actions">
        <button onClick={() => setDraft(saved.global)} disabled={busy || !changed}>
          Revert
        </button>
        <button className="primary" onClick={() => void save()} disabled={busy || !changed}>
          Save
        </button>
      </div>
    </>
  );
}
