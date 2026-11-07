// Mirrors the selected session's browser: the page after the agent's latest action.

import type { SessionState } from "../sessions";

export function BrowserPanel({ session }: { session: SessionState | null }) {
  const view = session?.browser;
  if (!view) {
    return <p className="empty">The browser appears here when an agent opens a page.</p>;
  }
  return (
    <figure className="browser-view">
      <figcaption title={view.url}>
        <strong>{view.title || "Untitled"}</strong> <span className="muted">{view.url}</span>
      </figcaption>
      <img src={`data:image/jpeg;base64,${view.screenshot}`} alt={`Screenshot of ${view.title || view.url}`} />
    </figure>
  );
}
