// The About section: version, updates and the tour.

import { Row } from "../components/SettingsRow";
import type { DaemonClient } from "../daemon/client";
import type { Updates } from "../UpdateControl";

export function AboutSection({ updates, client, onStartTour }: { updates: Updates | null; client: DaemonClient | null; onStartTour: () => void }) {
  const daemon = client?.daemon;
  return (
    <>
      <Row label="Imperium" description="The desktop app.">
        <span>{updates ? `v${updates.status.current_version} · ${updates.status.channel}` : "browser preview"}</span>
      </Row>
      <Row label="imperium-core" description="The daemon this window is connected to.">
        <span>{daemon ? `${daemon.daemon.version} · protocol ${daemon.protocol.major}.${daemon.protocol.minor}` : "not connected"}</span>
      </Row>
      <Row label="Tour" description="A short guided tour of the main areas of the app.">
        <button onClick={onStartTour}>Take the tour</button>
      </Row>
      <Row label="Source" description="Code, releases, and issues.">
        <span>github.com/Densetsu152637/Imperium-AI</span>
      </Row>
    </>
  );
}
