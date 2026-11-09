// Gets the user's attention: a short tone, a dot in front of the window title while Imperium is in
// the background, and a system notification (the OS's own, so its do-not-disturb applies). Which
// events call for which is in attention.ts; Settings → Notifications turns each on or off.
//
// `notify` is the one entry point: anything in the app with news for the user (approvals,
// finished or failed turns, downloads, updates, and later budget alerts) calls it with a notice.

import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getAllWebviewWindows } from "@tauri-apps/api/webviewWindow";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";

import { deliveryFor, type Attention, type Notice } from "./attention";
import type { AppSettings } from "./settings";

export type { Attention, Notice } from "./attention";

const TONES: Partial<Record<Attention, number[]>> = {
  approval: [880, 1175],
  finished: [660, 523],
  failed: [523, 392],
};

let audio: AudioContext | null = null;

function chime(frequencies: number[]): void {
  try {
    audio ??= new AudioContext();
    const start = audio.currentTime;
    frequencies.forEach((frequency, index) => {
      const oscillator = audio!.createOscillator();
      const gain = audio!.createGain();
      oscillator.frequency.value = frequency;
      const at = start + index * 0.14;
      gain.gain.setValueAtTime(0.0001, at);
      gain.gain.exponentialRampToValueAtTime(0.12, at + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.22);
      oscillator.connect(gain).connect(audio!.destination);
      oscillator.start(at);
      oscillator.stop(at + 0.25);
    });
  } catch {
    // No audio device: stay quiet.
  }
}

const BADGE = "● ";

function badgeTitle(): void {
  if (document.title.startsWith(BADGE)) return;
  document.title = BADGE + document.title;
  const clear = () => {
    if (document.hidden || !document.hasFocus()) return;
    document.title = document.title.replace(BADGE, "");
    document.removeEventListener("visibilitychange", clear);
    window.removeEventListener("focus", clear);
  };
  document.addEventListener("visibilitychange", clear);
  window.addEventListener("focus", clear);
}

/** Whether any Imperium window (the main one or a detached panel) has focus. */
async function appFocused(): Promise<boolean> {
  if (!isTauri()) return !document.hidden && document.hasFocus();
  try {
    const focus = await Promise.all((await getAllWebviewWindows()).map((window) => window.isFocused()));
    return focus.some(Boolean);
  } catch {
    return document.hasFocus();
  }
}

// ---- System notifications ----

let asked = false;

/** Whether system notifications may be shown; asks once per run where the OS wants consent (macOS, browsers). */
async function permitted(): Promise<boolean> {
  try {
    if (isTauri()) {
      if (await isPermissionGranted()) return true;
      if (asked) return false;
      asked = true;
      return (await requestPermission()) === "granted";
    }
    if (typeof Notification === "undefined") return false;
    if (Notification.permission !== "default") return Notification.permission === "granted";
    if (asked) return false;
    asked = true;
    return (await Notification.requestPermission()) === "granted";
  } catch {
    return false;
  }
}

type Opener = (notice: Notice) => void;
let opener: Opener | null = null;
/** The latest notice shown while Imperium was in the background, until a window comes to the front. */
let unseen: { notice: Notice; at: number } | null = null;
/** How long after a notification bringing Imperium forward still counts as answering it. */
const ANSWER_WINDOW_MS = 10 * 60 * 1000;

function opened(notice: Notice): void {
  unseen = null;
  opener?.(notice);
}

async function showSystem(notice: Notice, background: boolean): Promise<void> {
  if (!(await permitted())) return;
  if (isTauri()) {
    // The desktop plugin reports no clicks, so the next time the window comes forward (from the
    // notification, where the OS does that, or the taskbar) counts as opening the latest one.
    sendNotification({ title: notice.title, body: notice.body });
    if (background) unseen = { notice, at: Date.now() };
    return;
  }
  const shown = new Notification(notice.title, { body: notice.body, tag: `imperium-${notice.kind}-${notice.sessionId ?? ""}` });
  shown.onclick = () => {
    window.focus();
    shown.close();
    opened(notice);
  };
}

/**
 * Calls `open` when the user answers a notification: clicks it in a browser, or, in the desktop
 * app, brings Imperium forward within a few minutes of it. Returns a function that stops.
 */
export function onNoticeOpened(open: Opener): () => void {
  opener = open;
  let stop: (() => void) | null = null;
  let stopped = false;
  if (isTauri()) {
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused || !unseen) return;
        if (Date.now() - unseen.at < ANSWER_WINDOW_MS) opened(unseen.notice);
        unseen = null;
      })
      .then((unlisten) => (stopped ? unlisten() : (stop = unlisten)))
      .catch(() => undefined);
  }
  return () => {
    stopped = true;
    stop?.();
    if (opener === open) opener = null;
  };
}

/** Tells the user about a notice in the ways the settings ask for. */
export async function notify(notice: Notice, settings: AppSettings): Promise<void> {
  const focused = await appFocused();
  const delivery = deliveryFor(notice.kind, settings, focused);
  const tone = TONES[notice.kind];
  if (delivery.sound && tone) chime(tone);
  if (delivery.badge) badgeTitle();
  if (delivery.system) await showSystem(notice, !focused);
}

/** Plays a tone so the user can hear what a setting does. */
export function preview(kind: Attention): void {
  const tone = TONES[kind];
  if (tone) chime(tone);
}

/** Shows a sample system notification now, whatever the focus; false when the OS or browser refuses. */
export async function testNotification(): Promise<boolean> {
  if (!(await permitted())) return false;
  await showSystem({ kind: "finished", title: "Imperium", body: "Notifications are working." }, false);
  return true;
}

/** Brings the main window to the front (the desktop app), or this tab (a browser). */
export async function focusMainWindow(): Promise<void> {
  if (!isTauri()) {
    window.focus();
    return;
  }
  const main = getCurrentWindow();
  await main.unminimize().catch(() => undefined);
  await main.show().catch(() => undefined);
  await main.setFocus().catch(() => undefined);
}
