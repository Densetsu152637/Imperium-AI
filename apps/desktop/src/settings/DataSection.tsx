// The Data section: where the app keeps its files.

import { useEffect, useState } from "react";
import { Row } from "../components/SettingsRow";
import type { Daemon } from "../DaemonControl";
import { DEFAULT_SETTINGS, type AppSettings } from ".";
import { storeFolder } from "../store";

export function DataSection({
  userFile,
  settings,
  onChange,
  onResetLayout,
}: {
  userFile: string | null;
  settings: AppSettings;
  onChange: (settings: AppSettings) => void;
  onResetLayout: () => void;
}) {
  const [folder, setFolder] = useState<string | null>(null);
  useEffect(() => {
    storeFolder().then(setFolder, () => setFolder(null));
  }, []);
  const changed = (Object.keys(DEFAULT_SETTINGS) as (keyof AppSettings)[]).filter(
    (key) => key !== "tour" && JSON.stringify(settings[key]) !== JSON.stringify(DEFAULT_SETTINGS[key]),
  );
  return (
    <>
      <Row label="App settings and layout" description="Kept on this computer, outside the install folder, so they survive updates and reinstalls.">
        <code className="path">{folder ?? "…"}</code>
      </Row>
      <Row label="Daemon settings" description="Imperium's config file; every setting under Agent and daemon is saved here.">
        <code className="path">{userFile ?? "connect to imperium-core to see it"}</code>
      </Row>
      <Row label="Reset app settings" description={`Return the ${changed.length} changed app settings to their defaults. Daemon settings are not affected.`}>
        <button onClick={() => onChange({ ...DEFAULT_SETTINGS, tour: settings.tour })} disabled={changed.length === 0}>
          Reset
        </button>
      </Row>
      <Row label="Reset layout" description="Reopen every panel and restore the default sizes.">
        <button onClick={onResetLayout}>Reset</button>
      </Row>
    </>
  );
}
