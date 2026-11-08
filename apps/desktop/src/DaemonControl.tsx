// Daemon controls for the desktop app: see whether imperium-core is running, start, stop, or restart
// it, read its log, and choose whether it keeps running after the app closes. The native side
// does the work; absent outside Tauri.

import { Play, RefreshCw, RotateCw, ScrollText, Square } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { insideTauri } from "./daemon/connection";

export type DaemonStatus = {
  running: boolean;
  pid: number | null;
  url: string | null;
  /** Whether the running daemon is the one installed with this app. */
  bundled: boolean;
  /** Whether this install has a daemon it can start. */
  can_start: boolean;
  stopped_by_user: boolean;
  keep_running: boolean;
  log_path: string | null;
};

type Action = "start" | "stop" | "restart";

export type Daemon = {
  status: DaemonStatus;
  /** The action in progress, if any. */
  busy: Action | null;
  error: string | null;
  run: (action: Action) => void;
  setKeepRunning: (keep: boolean) => void;
  refresh: () => void;
};

/**
 * The daemon's status and controls, or null outside the desktop app. `onChanged` runs after a
 * start or restart so the window reconnects; after a stop the window sees its connection close.
 * Detached panel windows pass `enabled: false`: only the main window controls the daemon.
 */
export function useDaemon(onChanged: () => void, connected: boolean, enabled = true): Daemon | null {
  const [status, setStatus] = useState<DaemonStatus | null>(null);
  const [busy, setBusy] = useState<Action | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    if (!insideTauri() || !enabled) return;
    invoke<DaemonStatus>("daemon_status")
      .then(setStatus)
      .catch((reason: unknown) => setError(String(reason)));
  }, [enabled]);

  // The connection changing is when the daemon most likely changed.
  useEffect(refresh, [refresh, connected]);

  if (!status || !enabled) return null;
  return {
    status,
    busy,
    error,
    refresh,
    run: (action) => {
      setBusy(action);
      setError(null);
      invoke<DaemonStatus>(`daemon_${action}`)
        .then((next) => {
          setStatus(next);
          if (action !== "stop") onChanged();
        })
        .catch((reason: unknown) => setError(String(reason)))
        .finally(() => setBusy(null));
    },
    setKeepRunning: (keep) => {
      invoke<DaemonStatus>("daemon_set_keep_running", { keepRunning: keep })
        .then(setStatus)
        .catch((reason: unknown) => setError(String(reason)));
    },
  };
}

const BUSY: Record<Action, string> = { start: "Starting…", stop: "Stopping…", restart: "Restarting…" };

function describe(status: DaemonStatus): string {
  if (!status.running) return status.stopped_by_user ? "Stopped" : "Not running";
  const owner = status.bundled ? "installed with Imperium" : "started outside the app";
  return `Running · process ${status.pid} · ${owner}`;
}

/** The Daemon section of the settings menu. */
export function DaemonSettings({ daemon }: { daemon: Daemon }) {
  const { status, busy } = daemon;
  const [log, setLog] = useState<string | null>(null);
  const loadLog = () =>
    invoke<string>("daemon_log")
      .then((text) => setLog(text || "The log is empty."))
      .catch((reason: unknown) => setLog(String(reason)));

  return (
    <fieldset className="setting daemon-settings">
      <legend>Daemon</legend>
      <p className="muted daemon-state" data-running={status.running}>
        <span className="daemon-dot" aria-hidden="true" />
        {busy ? BUSY[busy] : describe(status)}
      </p>
      {daemon.error && <p className="error">{daemon.error}</p>}
      <div className="daemon-actions">
        {status.running ? (
          <>
            <button onClick={() => daemon.run("stop")} disabled={busy !== null}>
              <Square aria-hidden="true" /> Stop
            </button>
            <button onClick={() => daemon.run("restart")} disabled={busy !== null || !status.can_start}>
              <RotateCw aria-hidden="true" /> Restart
            </button>
          </>
        ) : (
          <button className="primary" onClick={() => daemon.run("start")} disabled={busy !== null || !status.can_start}>
            <Play aria-hidden="true" /> Start
          </button>
        )}
        <button className="ghost" onClick={() => (log === null ? void loadLog() : setLog(null))} aria-expanded={log !== null}>
          <ScrollText aria-hidden="true" /> {log === null ? "Show log" : "Hide log"}
        </button>
      </div>
      <label className="checkbox">
        <input
          type="checkbox"
          checked={status.keep_running}
          onChange={(event) => daemon.setKeepRunning(event.target.checked)}
        />
        Keep running after Imperium closes
      </label>
      {!status.can_start && (
        <p className="muted">This build has no bundled imperium-core; start and stop it where you run it.</p>
      )}
      {log !== null && (
        <div className="daemon-log">
          <div className="daemon-log-bar">
            <span className="muted">{status.log_path}</span>
            <button className="ghost" onClick={() => void loadLog()}>
              <RefreshCw aria-hidden="true" /> Refresh
            </button>
          </div>
          <pre>{log}</pre>
        </div>
      )}
    </fieldset>
  );
}
