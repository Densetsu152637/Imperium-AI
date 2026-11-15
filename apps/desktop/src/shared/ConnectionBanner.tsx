// A slim bar above the workspace while the daemon is away: why, and how to reach it.

import { useState } from "react";

import type { DaemonClient, DaemonConnection } from "../daemon/client";
import { insideTauri } from "../daemon/connection";
import type { Daemon } from "./DaemonControl";

export type Status = { kind: "connecting" } | { kind: "connected"; client: DaemonClient } | { kind: "failed"; message: string };

export function ConnectionBanner({
  status,
  onConnect,
  daemon,
}: {
  status: Status;
  onConnect: (connection?: DaemonConnection) => void;
  daemon: Daemon | null;
}) {
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  if (status.kind === "connecting") {
    return (
      <div className="connection-banner" data-state="connecting" role="status">
        <span className="daemon-dot" aria-hidden="true" /> Connecting to imperium-core…
      </div>
    );
  }
  return (
    <div className="connection-banner" data-state="offline" role="status">
      <span className="daemon-dot" aria-hidden="true" />
      <span className="connection-message">
        <strong>imperium-core is not connected.</strong> {status.kind === "failed" ? status.message : ""} Showing what was
        last known.
      </span>
      {daemon?.error && <span className="error">{daemon.error}</span>}
      {daemon?.status.can_start && !daemon.status.running && (
        <button className="primary" onClick={() => daemon.run("start")} disabled={daemon.busy !== null}>
          {daemon.busy ? "Starting…" : "Start imperium-core"}
        </button>
      )}
      {insideTauri() ? (
        <button onClick={() => onConnect()}>Retry</button>
      ) : (
        <form
          className="connection-form"
          onSubmit={(event) => {
            event.preventDefault();
            onConnect({ url: url.trim(), token: token.trim() });
          }}
        >
          <input aria-label="Daemon URL" placeholder="ws://127.0.0.1:port/rpc" value={url} onChange={(e) => setUrl(e.target.value)} />
          <input aria-label="Token" placeholder="token" type="password" value={token} onChange={(e) => setToken(e.target.value)} />
          <button type="submit" className="primary">
            Connect
          </button>
        </form>
      )}
    </div>
  );
}
