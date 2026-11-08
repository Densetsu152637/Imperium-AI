// Self-update: the native side checks the chosen channel on its own and downloads newer builds;
// this module shows its status, switches channel, and installs on request. Absent outside Tauri.

import { Download, RotateCw } from "lucide-react";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { insideTauri } from "./daemon/connection";

export type Channel = "main" | "experimental";

export type UpdateStatus = {
  current_version: string;
  channel: Channel;
  automatic: boolean;
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
  /** Checks now and installs and restarts if a newer build is found. */
  installNow: () => void;
  /** Installs an already downloaded build and restarts. */
  restart: () => void;
};

const CHANNELS: { id: Channel; label: string; hint: string }[] = [
  { id: "main", label: "Main", hint: "Stable releases" },
  { id: "experimental", label: "Experimental", hint: "Every build of the experimental branch; may be unstable" },
];

function describe(status: UpdateStatus): string {
  switch (status.state) {
    case "idle":
      return status.automatic ? "Checks automatically every few hours" : "Development build: checks only when asked";
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

/** The Updates section of the settings menu: channel choice, status, and check-and-install. */
export function UpdateSettings({ updates }: { updates: Updates }) {
  const { status, installing } = updates;
  const busy = installing || status.state === "checking" || status.state === "downloading";
  return (
    <fieldset className="setting update-settings">
      <legend>Updates</legend>
      <span>
        Installed <output>v{status.current_version}</output>
      </span>
      <div className="update-channels" role="radiogroup" aria-label="Update channel">
        {CHANNELS.map((channel) => (
          <label key={channel.id} className="radio" title={channel.hint}>
            <input
              type="radio"
              name="update-channel"
              checked={status.channel === channel.id}
              onChange={() => updates.setChannel(channel.id)}
              disabled={busy}
            />
            {channel.label}
          </label>
        ))}
      </div>
      <p className="muted update-state" data-state={status.state}>
        {installing && status.state !== "downloading" && status.state !== "checking" ? "Installing…" : describe(status)}
      </p>
      {updates.error && <p className="error">{updates.error}</p>}
      <button className="primary" onClick={updates.installNow} disabled={busy}>
        <Download aria-hidden="true" /> {busy ? "Updating…" : "Check and install updates"}
      </button>
    </fieldset>
  );
}
