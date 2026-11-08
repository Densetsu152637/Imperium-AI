// Finds the running daemon. Inside the Tauri shell the native side reads the endpoint file;
// in a plain browser (development preview) the user pastes the URL and token once.

import { invoke, isTauri } from "@tauri-apps/api/core";

import type { DaemonConnection } from "./client";

const STORAGE_KEY = "imperium.devConnection";

export const insideTauri = (): boolean => isTauri();

/** The daemon to connect to. In the desktop app, `autostart` lets it start the bundled daemon. */
export async function findDaemon(autostart = true): Promise<DaemonConnection | null> {
  if (insideTauri()) return invoke<DaemonConnection>("daemon_endpoint", { autostart });
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    return saved ? (JSON.parse(saved) as DaemonConnection) : null;
  } catch {
    return null;
  }
}

export function rememberDevConnection(connection: DaemonConnection): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(connection));
  } catch {
    // Storage can be unavailable (private windows); the user re-enters it next time.
  }
}

/** Asks for a project folder: a native picker in Tauri, a text prompt in a browser. */
export async function pickFolder(): Promise<string | null> {
  if (insideTauri()) {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const chosen = await open({ directory: true, multiple: false, title: "Open project folder" });
    return typeof chosen === "string" ? chosen : null;
  }
  return window.prompt("Project folder (absolute path)")?.trim() || null;
}
