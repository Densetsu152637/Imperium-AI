// The environment window's Configuration file section: what the environment's own
// `.imperium/config.toml` (in its first folder) changes for sessions started there, and what it
// asked for that was refused. The file can only narrow the user's settings, so this is a report,
// not an editor: the file belongs to the project.

import { RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";

import type { EnvironmentConfigResult, EnvironmentInfo } from "@imperium/protocol";

import { Row, RowNote } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";

type Props = {
  environment: EnvironmentInfo;
  client: DaemonClient | null;
};

export function EnvironmentConfigSection({ environment, client }: Props) {
  const [config, setConfig] = useState<EnvironmentConfigResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloads, setReloads] = useState(0);
  const folder = environment.folders[0] ?? "";

  useEffect(() => {
    if (!client) return;
    let live = true;
    client
      .call<EnvironmentConfigResult>("environments.config", { id: environment.id })
      .then((result) => {
        if (!live) return;
        setConfig(result);
        setError(null);
      })
      .catch((failure: unknown) => {
        if (live) setError(failure instanceof Error ? failure.message : String(failure));
      });
    return () => {
      live = false;
    };
  }, [client, environment.id, folder, reloads]);

  const empty = config !== null && config.files.length === 0 && config.refused.length === 0;

  return (
    <>
      <Row
        label="Configuration file"
        description={
          <>
            <code>.imperium/config.toml</code> in the first folder can add instructions and make sessions stricter: a
            stricter permission mode, ask and deny rules, a stricter isolation tier, local-only routing rules, budgets. It
            can never run a program or widen what you set. Servers and plugins it names start only for a folder in{" "}
            <code>mcp.trusted_projects</code> (Settings). Applies to sessions started afterwards.
          </>
        }
      >
        <button type="button" onClick={() => setReloads((count) => count + 1)} disabled={!client}>
          <RefreshCw aria-hidden="true" /> Reload
        </button>
      </Row>
      <RowNote>
        <div className="environment-config">
          {error && (
            <p className="error" role="alert">
              {error}
            </p>
          )}
          {config && config.folder === null && <span className="muted">This environment has no folder on this machine.</span>}
          {config && config.folder !== null && empty && (
            <span className="muted">
              No config.toml or config.local.toml in <code className="path">{config.folder}/.imperium</code>.
            </span>
          )}
          {config && !empty && (
            <>
              {config.files.map((file) => (
                <code className="path working-folder-path" key={file} title={file}>
                  {file}
                </code>
              ))}
              <span className="muted">Folder {config.trusted ? "is" : "is not"} listed in mcp.trusted_projects.</span>
              {config.applied.length > 0 && (
                <ul className="environment-config-lines" aria-label="Applied">
                  {config.applied.map((line) => (
                    <li key={`applied-${line}`}>Applied: {line}</li>
                  ))}
                </ul>
              )}
              {config.refused.length > 0 && (
                <ul className="environment-config-lines refused" aria-label="Refused">
                  {config.refused.map((line) => (
                    <li key={`refused-${line}`}>Refused: {line}</li>
                  ))}
                </ul>
              )}
            </>
          )}
          {!config && !error && <span className="muted">Loading…</span>}
        </div>
      </RowNote>
    </>
  );
}
