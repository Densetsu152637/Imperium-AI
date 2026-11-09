// Panels in windows of their own. The main window opens, focuses, and closes them; a detached
// window reports back what the user did there (pinned it, moved it, docked it) as events, since
// only the main window writes the layout. Outside Tauri the windows are browser popups, which
// talk over a BroadcastChannel instead and cannot be pinned.

import { emit, listen } from "@tauri-apps/api/event";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { availableMonitors } from "@tauri-apps/api/window";

import { insideTauri } from "./daemon/connection";
import type { DetachedPanel } from "./layout/state";

export type PanelWindowEvent =
  | { kind: "pinned"; id: string; pinned: boolean }
  | { kind: "bounds"; id: string; bounds: NonNullable<DetachedPanel["bounds"]> }
  | { kind: "dock"; id: string }
  /** From the main window: close this panel's window. */
  | { kind: "close"; id: string }
  /** From a panel window's command palette: the main window runs the command with this id. */
  | { kind: "command"; command: string };

const EVENT = "imperium://panel-window";
const DEFAULT_SIZE = { width: 420, height: 560 };

const channel = typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel("imperium-panel-window");

/** The panel this window shows alone, or null in the main window. */
export function detachedPanelId(): string | null {
  return new URLSearchParams(window.location.search).get("panel");
}

export function panelLabel(id: string): string {
  return `panel-${id.replace(/[^A-Za-z0-9_-]/g, "_")}`;
}

function panelUrl(id: string): string {
  return `index.html?panel=${encodeURIComponent(id)}`;
}

export function emitPanelEvent(event: PanelWindowEvent): void {
  if (insideTauri()) emit(EVENT, event).catch(() => undefined);
  else channel?.postMessage(event);
}

/** Listens for panel window events; returns how to stop. */
export function listenPanelEvents(handler: (event: PanelWindowEvent) => void): () => void {
  if (!insideTauri()) {
    if (!channel) return () => undefined;
    const onMessage = (message: MessageEvent<PanelWindowEvent>) => handler(message.data);
    channel.addEventListener("message", onMessage);
    return () => channel.removeEventListener("message", onMessage);
  }
  const stop = listen<PanelWindowEvent>(EVENT, (event) => handler(event.payload));
  return () => void stop.then((unlisten) => unlisten()).catch(() => undefined);
}

type Bounds = NonNullable<DetachedPanel["bounds"]>;

/**
 * Moves saved bounds onto a visible monitor, so a window saved on a monitor that is now gone
 * (or off-screen) comes back where it can be reached. Sizes shrink to fit the monitor.
 */
async function clampToMonitors(bounds: Bounds): Promise<Bounds> {
  try {
    const monitors = await availableMonitors();
    if (monitors.length === 0) return bounds;
    const areas = monitors.map((monitor) => ({
      x: monitor.position.x / monitor.scaleFactor,
      y: monitor.position.y / monitor.scaleFactor,
      width: monitor.size.width / monitor.scaleFactor,
      height: monitor.size.height / monitor.scaleFactor,
    }));
    // Visible enough: the title strip lies inside some monitor.
    const reachable = areas.some(
      (a) => bounds.x + 40 < a.x + a.width && bounds.x + bounds.width - 40 > a.x && bounds.y >= a.y - 1 && bounds.y + 30 < a.y + a.height,
    );
    if (reachable) return bounds;
    const target = areas[0];
    const width = Math.min(bounds.width, target.width);
    const height = Math.min(bounds.height, target.height);
    return { x: target.x + 80, y: target.y + 80, width, height };
  } catch {
    return bounds;
  }
}

/** Opens the panel's window at its saved place, or focuses it when it is already open. */
export async function openPanelWindow(
  id: string,
  title: string,
  saved: DetachedPanel | undefined,
  onFail: () => void,
): Promise<void> {
  const label = panelLabel(id);
  let bounds = saved?.bounds;
  if (!insideTauri()) {
    const size = bounds ?? { ...DEFAULT_SIZE, x: window.screenX + 60, y: window.screenY + 60 };
    // The same name reuses and focuses an open popup.
    const popup = window.open(
      panelUrl(id),
      label,
      `popup,width=${size.width},height=${size.height},left=${size.x},top=${size.y}`,
    );
    if (popup) popup.focus();
    else onFail();
    return;
  }
  const existing = await WebviewWindow.getByLabel(label);
  if (existing) {
    await existing.setFocus();
    return;
  }
  if (bounds) bounds = await clampToMonitors(bounds);
  let place: { x: number; y: number } | undefined = bounds;
  if (!place) {
    // Near the main window, so the new panel appears where the user is looking.
    const main = WebviewWindow.getCurrent();
    const [position, scale] = await Promise.all([main.outerPosition(), main.scaleFactor()]);
    const logical = position.toLogical(scale);
    place = { x: logical.x + 80, y: logical.y + 80 };
  }
  // The window reports back through events; one that cannot be created leaves the panel docked.
  const created = new WebviewWindow(label, {
    url: panelUrl(id),
    title: `${title} — Imperium`,
    x: place.x,
    y: place.y,
    width: bounds?.width ?? DEFAULT_SIZE.width,
    height: bounds?.height ?? DEFAULT_SIZE.height,
    alwaysOnTop: saved?.pinned ?? false,
  });
  void created.once("tauri://error", onFail);
}
