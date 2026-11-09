// A detached panel's window: the panel fills it under a slim header with the panel's own
// actions, a Pin toggle (always on top), and a Dock button. The window never writes the layout;
// pinning, moving, and docking are reported to the main window (panelWindows.ts), which does.

import { useCallback, useEffect, useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { insideTauri } from "../daemon/connection";
import { emitPanelEvent, listenPanelEvents } from "../panelWindows";
import type { PanelSpec } from "./Columns";
import { getLayout } from "./state";

const BOUNDS_DELAY_MS = 400;

/** Window behaviour for one detached panel: pin state, bounds reports, docking on close. */
function usePanelWindow(id: string) {
  const [pinned, setPinned] = useState(() => getLayout().detached[id]?.pinned ?? false);
  const tauri = insideTauri();
  // Whether the main window already knows this panel is docked.
  const docked = useRef(false);

  const dock = useCallback(() => {
    docked.current = true;
    emitPanelEvent({ kind: "dock", id });
    if (tauri) void getCurrentWindow().close();
    else window.close();
  }, [id, tauri]);

  const togglePin = () => {
    const next = !pinned;
    setPinned(next);
    emitPanelEvent({ kind: "pinned", id, pinned: next });
    if (tauri) getCurrentWindow().setAlwaysOnTop(next).catch(() => setPinned(!next));
  };

  // The main window asks for this window to close when the panel is docked from Settings.
  useEffect(
    () =>
      listenPanelEvents((event) => {
        if (event.kind !== "close" || event.id !== id) return;
        if (tauri) void getCurrentWindow().close();
        else window.close();
      }),
    [id, tauri],
  );

  useEffect(() => {
    if (!tauri) return;
    const win = getCurrentWindow();
    let timer: number | undefined;
    const report = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(async () => {
        try {
          const [position, size, scale] = await Promise.all([win.outerPosition(), win.innerSize(), win.scaleFactor()]);
          const at = position.toLogical(scale);
          const area = size.toLogical(scale);
          emitPanelEvent({ kind: "bounds", id, bounds: { x: at.x, y: at.y, width: area.width, height: area.height } });
        } catch {
          // The window went away while measuring.
        }
      }, BOUNDS_DELAY_MS);
    };
    const stops = [win.onMoved(report), win.onResized(report)];
    return () => {
      window.clearTimeout(timer);
      stops.forEach((stop) => void stop.then((unlisten) => unlisten()).catch(() => undefined));
    };
  }, [id, tauri]);

  // Closing with the OS button docks the panel, so the main window shows it again. (Quitting
  // from the main window ends the process without closing this window, so it reopens next start.)
  useEffect(() => {
    if (tauri) {
      const stop = getCurrentWindow().onCloseRequested(() => {
        if (!docked.current) emitPanelEvent({ kind: "dock", id });
        docked.current = true;
      });
      return () => void stop.then((unlisten) => unlisten()).catch(() => undefined);
    }
    const onUnload = () => {
      if (!docked.current) emitPanelEvent({ kind: "dock", id });
    };
    window.addEventListener("pagehide", onUnload);
    return () => window.removeEventListener("pagehide", onUnload);
  }, [id, tauri]);

  return { pinned, togglePin, dock, canPin: tauri };
}

export function PanelWindow({ id, panel }: { id: string; panel: PanelSpec | null }) {
  const { pinned, togglePin, dock, canPin } = usePanelWindow(id);
  const title = panel?.title ?? id;

  useEffect(() => {
    document.title = `${title} — Imperium`;
  }, [title]);

  return (
    <section className="panel panel-window" data-panel={id} aria-labelledby={`panel-${id}`}>
      <header className="panel-header">
        <h2 id={`panel-${id}`}>{title}</h2>
        {panel?.actions}
        {canPin && (
          <button
            className="ghost panel-pin"
            onClick={togglePin}
            aria-pressed={pinned}
            title={pinned ? "Unpin from above other windows" : "Keep above other windows"}
          >
            {pinned ? "Pinned" : "Pin"}
          </button>
        )}
        <button className="ghost" onClick={dock} title="Return to the main window">
          Dock
        </button>
      </header>
      <div className="panel-body">{panel?.body ?? <p className="empty">Loading…</p>}</div>
    </section>
  );
}
