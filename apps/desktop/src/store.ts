// Local documents the interface keeps for itself: settings, panel layout, and the last-known
// sessions shown while the daemon is away. In the desktop app they are files in Imperium's
// config folder (src-tauri/src/store.rs), so they survive updates and reinstalls; in a browser
// they live in local storage. All are loaded once before the first render, then read
// synchronously and written back shortly after each change. Every change is also broadcast to
// the app's other windows (detached panels), which take it into memory without writing it.

import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";

import { insideTauri } from "./daemon/connection";

export type DocName = "settings" | "layout" | "cache" | "mods";

const NAMES: DocName[] = ["settings", "layout", "cache", "mods"];
const WRITE_DELAY_MS = 300;

const DOC_EVENT = "imperium://doc";

const docs = new Map<DocName, unknown>();
const pending = new Map<DocName, number>();
const listeners = new Map<DocName, Set<() => void>>();

type DocMessage = { name: DocName; value: unknown; source: string };

/** Tells this window's own broadcasts from other windows' (Tauri delivers events to the sender too). */
const SOURCE = Math.random().toString(36).slice(2);
const channel = typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel("imperium-doc");

/** A change another window made: take it into memory without writing it back. */
function receive(message: DocMessage): void {
  if (message.source === SOURCE || !NAMES.includes(message.name)) return;
  docs.set(message.name, message.value);
  listeners.get(message.name)?.forEach((listener) => listener());
}

function broadcast(name: DocName, value: unknown): void {
  const message: DocMessage = { name, value, source: SOURCE };
  if (insideTauri()) emit(DOC_EVENT, message).catch(() => undefined);
  else channel?.postMessage(message);
}

async function read(name: DocName): Promise<unknown> {
  try {
    const text = insideTauri()
      ? await invoke<string | null>("store_read", { name })
      : localStorage.getItem(`imperium.store.${name}`);
    return text ? JSON.parse(text) : undefined;
  } catch {
    return undefined;
  }
}

function write(name: DocName): void {
  pending.delete(name);
  const contents = JSON.stringify(docs.get(name), null, 2);
  if (insideTauri()) {
    invoke("store_write", { name, contents }).catch((error: unknown) => console.warn(`could not save ${name}`, error));
  } else {
    try {
      localStorage.setItem(`imperium.store.${name}`, contents);
    } catch {
      // Unavailable storage only means nothing is remembered.
    }
  }
}

/** Loads every document; call once before rendering. */
export async function loadStore(): Promise<void> {
  await Promise.all(NAMES.map(async (name) => docs.set(name, await read(name))));
  if (insideTauri()) await listen<DocMessage>(DOC_EVENT, (event) => receive(event.payload)).catch(() => undefined);
  else if (channel) channel.onmessage = (event: MessageEvent<DocMessage>) => receive(event.data);
  // Save what is pending when the window goes away.
  window.addEventListener("pagehide", flushStore);
}

export function getDoc<T>(name: DocName): T | undefined {
  return docs.get(name) as T | undefined;
}

/** Runs `listener` when another window changes the document; returns how to stop listening. */
export function subscribeDoc(name: DocName, listener: () => void): () => void {
  const set = listeners.get(name) ?? new Set();
  listeners.set(name, set);
  set.add(listener);
  return () => void set.delete(listener);
}

/** Replaces a document, tells the other windows, and saves it shortly after (quick changes save once). */
export function setDoc<T>(name: DocName, value: T): void {
  docs.set(name, value);
  broadcast(name, value);
  window.clearTimeout(pending.get(name));
  pending.set(
    name,
    window.setTimeout(() => write(name), WRITE_DELAY_MS),
  );
}

/** Saves every pending change now. */
export function flushStore(): void {
  for (const [name, timer] of pending) {
    window.clearTimeout(timer);
    write(name);
  }
}

/** Where the documents are kept, for the Data section of Settings. */
export async function storeFolder(): Promise<string> {
  return insideTauri() ? invoke<string>("store_folder") : "this browser's local storage";
}

/** Reads and removes a value an older version kept in local storage, for one-time migration. */
export function takeLegacy<T>(key: string): T | undefined {
  try {
    const text = localStorage.getItem(key);
    if (text === null) return undefined;
    localStorage.removeItem(key);
    return JSON.parse(text) as T;
  } catch {
    return undefined;
  }
}
