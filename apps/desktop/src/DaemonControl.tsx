// Daemon controls for the desktop app: see whether imperium-core is running, start, stop, or restart
// it, read its log, and choose whether it keeps running after the app closes. The native side
// does the work; absent outside Tauri.

import { Play, RefreshCw, RotateCw, ScrollText, Square } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { Row, RowNote } from "./components/SettingsRow";
import { Switch } from "./components/Switch";
import { insideTauri } from "./daemon/connection";

export type DaemonStatus = {
  running: boolean;
  pid: number | null;
  url: string | null;
  /** Whether the running daemon is the one installed with this app. */
  bundled: boolean;
  /** Whether this app started the running daemon, as its own child process. */
  child: boolean;
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
 */
export function useDaemon(onChanged: () => void, connected: boolean): Daemon | null {
  const [status, setStatus] = useState<DaemonStatus | null>(null);
  const [busy, setBusy] = useState<Action | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    if (!insideTauri()) return;
    invoke<DaemonStatus>("daemon_status")
      .then(setStatus)
      .catch((reason: unknown) => setError(String(reason)));
  }, []);

  // The connection changing is when the daemon most likely changed.
  useEffect(refresh, [refresh, connected]);

  if (!status) return null;
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
  const owner = status.child
    ? "started by Imperium"
    : status.bundled
      ? "installed with Imperium, left from an earlier run"
      : "started outside the app";
  return `Running · process ${status.pid} · ${owner}`;
}

/** The Daemon section of Settings, as rows like every other section. */
export function DaemonSettings({ daemon }: { daemon: Daemon }) {
  const { status, busy } = daemon;
  const [log, setLog] = useState<string | null>(null);
  const loadLog = () =>
    invoke<string>("daemon_log")
      .then((text) => setLog(text || "The log is empty."))
      .catch((reason: unknown) => setLog(String(reason)));

  return (
    <>
      <Row
        label="imperium-core"
        description={
          <span className="daemon-state" data-running={status.running}>
            <span className="daemon-dot" aria-hidden="true" />
            {busy ? BUSY[busy] : describe(status)}
          </span>
        }
      >
        <div className="row-actions">
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
        </div>
      </Row>
      {(daemon.error || !status.can_start) && (
        <RowNote sub>
          {daemon.error && <p className="error">{daemon.error}</p>}
          {!status.can_start && (
            <p className="muted">This build has no bundled imperium-core; start and stop it where you run it.</p>
          )}
        </RowNote>
      )}
      <Row
        label="Keep running after Imperium closes"
        description={
          status.keep_running
            ? "imperium-core outlives the window, so schedules and auto-fix keep working; once Imperium closes it shows as its own process in Task Manager."
            : "Off: imperium-core runs as part of Imperium and exits with it. Sessions are saved and resume when Imperium opens again."
        }
      >
        <Switch
          checked={status.keep_running}
          onChange={daemon.setKeepRunning}
          label="Keep running after Imperium closes"
          disabled={!status.can_start}
        />
      </Row>
      <Row label="Log" description={status.log_path ?? "The daemon's output."}>
        <button className="ghost" onClick={() => (log === null ? void loadLog() : setLog(null))} aria-expanded={log !== null}>
          <ScrollText aria-hidden="true" /> {log === null ? "Show log" : "Hide log"}
        </button>
      </Row>
      {log !== null && (
        <RowNote sub>
          <div className="daemon-log">
            <div className="daemon-log-bar">
              <span className="muted">The last 64 KB</span>
              <button className="ghost" onClick={() => void loadLog()}>
                <RefreshCw aria-hidden="true" /> Refresh
              </button>
            </div>
            <pre>{log}</pre>
          </div>
        </RowNote>
      )}
    </>
  );
}
