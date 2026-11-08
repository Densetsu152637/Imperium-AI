// Self-update controls in the title bar: the installed version and channel, a restart button
// once a newer build has downloaded, and a menu to switch channel or check now. The native
// side checks and downloads on its own; this only shows its status. Absent outside Tauri.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { insideTauri } from "./daemon/connection";

type Channel = "main" | "experimental";

type UpdateStatus = {
  current_version: string;
  channel: Channel;
  automatic: boolean;
} & (
  | { state: "idle" | "checking" | "up_to_date" }
  | { state: "downloading"; version: string }
  | { state: "ready"; version: string; notes: string | null }
  | { state: "failed"; message: string }
);

const CHANNELS: { id: Channel; label: string; hint: string }[] = [
  { id: "main", label: "Main", hint: "Stable releases" },
  { id: "experimental", label: "Experimental", hint: "Every build of the experimental branch; may be unstable" },
];

function describe(status: UpdateStatus): string {
  switch (status.state) {
    case "idle":
      return status.automatic ? "Checks automatically" : "Development build: checks only when asked";
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

export function UpdateControl() {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [open, setOpen] = useState(false);
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
    action.catch((reason: unknown) => setError(String(reason)));
  };
  const setChannel = (channel: Channel) =>
    run(invoke<UpdateStatus>("update_set_channel", { channel }).then(setStatus));

  return (
    <>
      {status.state === "ready" && (
        <button className="primary update-ready" onClick={() => run(invoke("update_restart"))}>
          Restart to update
        </button>
      )}
      <button
        className="ghost update-version"
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        title="Updates"
      >
        v{status.current_version}
        {status.channel === "experimental" && <span className="channel-tag">experimental</span>}
      </button>
      {open && (
        <div className="settings-menu update-menu" role="dialog" aria-label="Updates">
          <fieldset className="setting">
            <legend>Update channel</legend>
            {CHANNELS.map((channel) => (
              <label key={channel.id} className="radio" title={channel.hint}>
                <input
                  type="radio"
                  name="update-channel"
                  checked={status.channel === channel.id}
                  onChange={() => setChannel(channel.id)}
                />
                {channel.label}
              </label>
            ))}
          </fieldset>
          <p className="muted update-state" data-state={status.state}>
            {describe(status)}
          </p>
          {error && <p className="error">{error}</p>}
          <div className="row">
            <button
              onClick={() => run(invoke("update_check"))}
              disabled={status.state === "checking" || status.state === "downloading"}
            >
              Check now
            </button>
            <button className="primary" onClick={() => setOpen(false)}>
              Done
            </button>
          </div>
        </div>
      )}
    </>
  );
}
