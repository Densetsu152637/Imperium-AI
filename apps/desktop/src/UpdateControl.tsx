// Self-update: the native side checks the chosen channel on the user's schedule and downloads
// newer builds; this module shows its status, switches channel, sets the schedule, and checks or
// installs on request. Absent outside Tauri.

import { Download, RefreshCw, RotateCw } from "lucide-react";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { Row, RowNote } from "./components/SettingsRow";
import { Select } from "./components/Select";
import { Switch } from "./components/Switch";
import { insideTauri } from "./daemon/connection";
import type { AppSettings, UpdateInterval } from "./settings";

export type Channel = "main" | "experimental";

export type UpdateStatus = {
  current_version: string;
  channel: Channel;
  automatic: boolean;
  /** Days between automatic checks. */
  interval_days: number;
  /** Whether the app checks (and installs) when the computer starts. */
  at_login: boolean;
  last_checked_ms: number | null;
  next_check_ms: number | null;
} & (
  | { state: "idle" | "checking" | "up_to_date" }
  | { state: "downloading"; version: string }
  | { state: "ready"; version: string; notes: string | null }
  | { state: "failed"; message: string }
);

export type Updates = {
  status: UpdateStatus;
  /** Set while a check-and-install the user asked for is running. */
  installing: boolean;
  error: string | null;
  setChannel: (channel: Channel) => void;
  /** Applies the schedule now; the settings document keeps it for the next start. */
  setSchedule: (intervalDays: UpdateInterval, atLogin: boolean) => void;
  /** Checks now; a newer build downloads and waits for a restart. */
  check: () => void;
  /** Checks now and installs and restarts if a newer build is found. */
  installNow: () => void;
  /** Installs an already downloaded build and restarts. */
  restart: () => void;
};

const CHANNELS: { id: Channel; label: string; hint: string }[] = [
  { id: "main", label: "Main", hint: "Stable releases" },
  { id: "experimental", label: "Experimental", hint: "Every build of the experimental branch; may be unstable" },
];

const INTERVALS: { value: UpdateInterval; label: string }[] = [
  { value: 1, label: "Every day" },
  { value: 7, label: "Every 7 days" },
  { value: 28, label: "Every 28 days" },
];

function when(ms: number): string {
  return new Date(ms).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

/** When the last check ran and the next one is due. */
function schedule(status: UpdateStatus): string {
  if (!status.automatic) return "Development build: checks only when asked.";
  const last = status.last_checked_ms ? `Last checked ${when(status.last_checked_ms)}` : "Not checked yet";
  const next = status.next_check_ms && status.next_check_ms > Date.now() ? ` · next check ${when(status.next_check_ms)}` : "";
  return `${last}${next}.`;
}

function describe(status: UpdateStatus): string {
  switch (status.state) {
    case "idle":
      return status.automatic ? "Checks automatically on the schedule below" : "Development build: checks only when asked";
    case "checking":
      return "Checking…";
    case "up_to_date":
      return "Up to date";
    case "downloading":
      return `Downloading ${status.version}…`;
    case "ready":
      return `${status.version} is ready. It installs when you quit, or restart now.`;
    case "failed":
      return status.message;
  }
}

/** The updater's status and actions, or null outside the desktop app. */
export function useUpdates(): Updates | null {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!insideTauri()) return;
    let active = true;
    const unlisten = listen<UpdateStatus>("update://status", (event) => setStatus(event.payload));
    invoke<UpdateStatus>("update_status")
      .then((initial) => active && setStatus(initial))
      .catch(() => undefined);
    return () => {
      active = false;
      unlisten.then((stop) => stop()).catch(() => undefined);
    };
  }, []);

  if (!status) return null;

  const run = (action: Promise<unknown>) => {
    setError(null);
    return action.catch((reason: unknown) => setError(String(reason)));
  };
  return {
    status,
    installing,
    error,
    setChannel: (channel) => void run(invoke<UpdateStatus>("update_set_channel", { channel }).then(setStatus)),
    setSchedule: (intervalDays, atLogin) =>
      void run(invoke<UpdateStatus>("update_set_schedule", { intervalDays, atLogin }).then(setStatus)),
    check: () => void run(invoke("update_check")),
    installNow: () => {
      setInstalling(true);
      // On success the app restarts; otherwise the status says why nothing was installed.
      void run(invoke<UpdateStatus>("update_install_now").then(setStatus)).finally(() => setInstalling(false));
    },
    restart: () => void run(invoke("update_restart")),
  };
}

/** Title-bar items: a restart button once a build has downloaded, and the version and channel. */
export function UpdateControl({ updates, onOpen }: { updates: Updates | null; onOpen: () => void }) {
  if (!updates) return null;
  const { status } = updates;
  return (
    <>
      {status.state === "ready" && (
        <button className="primary update-ready" onClick={updates.restart}>
          <RotateCw aria-hidden="true" /> Restart to update
        </button>
      )}
      <button className="ghost update-version" onClick={onOpen} title="Updates">
        v{status.current_version}
        {status.channel === "experimental" && <span className="channel-tag">experimental</span>}
      </button>
    </>
  );
}

/**
 * The Updates section of Settings: the installed version and its status with the check buttons,
 * the channel, and the schedule. The schedule is kept in the app settings (so it persists with
 * them) and passed to the native side, which reads it again at every start.
 */
export function UpdateSettings({
  updates,
  settings,
  onChange,
}: {
  updates: Updates;
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
}) {
  const { status, installing } = updates;
  const busy = installing || status.state === "checking" || status.state === "downloading";
  const setSchedule = (intervalDays: UpdateInterval, atLogin: boolean) => {
    onChange({ ...settings, updateIntervalDays: intervalDays, checkUpdatesAtLogin: atLogin });
    updates.setSchedule(intervalDays, atLogin);
  };
  const channelHint = CHANNELS.find((channel) => channel.id === status.channel)?.hint;
  return (
    <>
      <Row label="Installed version" description={`v${status.current_version} · ${status.channel} channel`}>
        <div className="row-actions">
          <button onClick={updates.check} disabled={busy} title="Look for a newer build now; it downloads and waits for a restart">
            <RefreshCw aria-hidden="true" /> Check for updates
          </button>
          <button className="primary" onClick={updates.installNow} disabled={busy} title="Check now, and install and restart if there is a newer build">
            <Download aria-hidden="true" /> {busy ? "Updating…" : "Check and install"}
          </button>
        </div>
      </Row>
      <RowNote sub>
        <p className="muted update-state" data-state={status.state}>
          {installing && status.state !== "downloading" && status.state !== "checking" ? "Installing…" : describe(status)}
        </p>
        {status.state === "ready" && (
          <button className="primary" onClick={updates.restart}>
            <RotateCw aria-hidden="true" /> Restart to update
          </button>
        )}
        {updates.error && <p className="error">{updates.error}</p>}
      </RowNote>
      <Row label="Update channel" description={channelHint}>
        <Select
          value={status.channel}
          options={CHANNELS.map((channel) => ({ value: channel.id, label: channel.label, description: channel.hint }))}
          onChange={(next) => updates.setChannel(next as Channel)}
          disabled={busy}
          aria-label="Update channel"
        />
      </Row>
      <Row label="Check automatically" description={schedule(status)}>
        <Select
          value={String(settings.updateIntervalDays)}
          options={INTERVALS.map((interval) => ({ value: String(interval.value), label: interval.label }))}
          onChange={(next) => setSchedule(Number(next) as UpdateInterval, settings.checkUpdatesAtLogin)}
          aria-label="How often to check for updates"
        />
      </Row>
      <Row
        sub
        label="Check for updates when the computer starts"
        description={
          status.automatic
            ? "Starts Imperium in the background when you sign in, without a window: it checks, installs a newer build if there is one, and exits."
            : "Only installed builds register to start with the computer."
        }
      >
        <Switch
          checked={settings.checkUpdatesAtLogin}
          onChange={(next) => setSchedule(settings.updateIntervalDays, next)}
          label="Check for updates when the computer starts"
        />
      </Row>
    </>
  );
}
