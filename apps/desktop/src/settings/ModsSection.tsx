// The Mods section: which mods are on.

import { useSyncExternalStore } from "react";
import type { ModListResult } from "@imperium/protocol";
import { installedByAgent, sourceLabel } from "./configChanges";
import { Row } from "../components/SettingsRow";
import { Switch as Toggle } from "../components/Switch";
import { getFaults, subscribeFaults } from "../mods/faults";
import { BUNDLED_ON_BY_DEFAULT, modEnabled, type AppSettings } from ".";

/** Mods the daemon found: installed ones and the nursery are on until turned off, other bundled ones are opt-in. */
export function ModsSection({
  mods,
  settings,
  onChange,
}: {
  mods: ModListResult | null;
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
}) {
  const faults = useSyncExternalStore(subscribeFaults, getFaults);
  if (!mods) return <p className="muted">Connect to imperiumd to see the mods it found.</p>;
  const toggle = (id: string, on: boolean) => onChange({ ...settings, enabledMods: { ...settings.enabledMods, [id]: on } });
  return (
    <>
      {mods.mods.length === 0 && <p className="muted">No mods are installed. Put one in the mods folder of the Imperium config folder.</p>}
      {mods.mods.map((mod) => {
        const panels = mod.panels.map((panel) => panel.title).join(", ") || "none";
        const permissions = mod.permissions.join(", ") || "none";
        const byAgent = installedByAgent(mod);
        const optIn = (mod.bundled && !BUNDLED_ON_BY_DEFAULT.includes(mod.id)) || byAgent || mod.opt_in;
        const from = mod.installed_from ? ` From ${sourceLabel(mod.installed_from)}.` : "";
        return (
          <Row
            key={mod.id}
            label={`${mod.name}${mod.version ? ` ${mod.version}` : ""}`}
            badge={<span className="badge">{mod.bundled ? "bundled" : byAgent ? "installed by an agent" : "installed"}</span>}
            description={`Panels: ${panels}. Permissions: ${permissions}.${from}${mod.modified ? " Its files changed since you approved it (or the approvals could not be read), so it has no permissions and stays off." : ""}${optIn ? " Off until you turn it on." : ""}`}
          >
            <Toggle checked={modEnabled(settings, mod)} onChange={(on) => toggle(mod.id, on)} label={mod.name} />
          </Row>
        );
      })}
      {faults.map((fault) => (
        <p key={`${fault.modId}|${fault.at}|${fault.name}|${fault.reason}`} className="muted">
          A behaviour of {fault.modId} was {fault.phase === "load" ? "refused" : "stopped"}: {fault.name} ({fault.reason}). The mod carries on without it.
        </p>
      ))}
      {mods.problems.map((problem) => (
        <p key={problem} className="muted">
          Could not load {problem}
        </p>
      ))}
    </>
  );
}
