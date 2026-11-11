// The Preview panel's rules (roadmap item 9): which pages it may show, and how the panel and
// the Browser panel follow the session's preview and browser-control events.
//
// A preview runs the project's own page inside the app's window, so it is limited to loopback
// addresses. The daemon applies the same rule to the URLs agents give (`loopback_url` in
// imperium-web), and the app's content security policy (`frame-src` in tauri.conf.json) allows
// frames from loopback origins only, so even a page that redirects cannot load a remote site.

export type PreviewUrl = { ok: true; url: string } | { ok: false; reason: string };

/** `input` as a preview URL, or why it cannot be one. */
export function previewUrl(input: string): PreviewUrl {
  const text = input.trim();
  if (!text) return { ok: false, reason: "Enter a localhost address, such as http://localhost:5173." };
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false, reason: `${text} is not a URL. Try http://localhost:5173.` };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: "Only http and https pages can be previewed." };
  }
  if (url.username || url.password) {
    return { ok: false, reason: "A preview address cannot carry a user name or password." };
  }
  // WHATWG parsing normalises hosts, so `0x7f.1` becomes 127.0.0.1 and `LOCALHOST` localhost.
  const host = url.hostname;
  if (host !== "localhost" && host !== "127.0.0.1" && host !== "[::1]") {
    return { ok: false, reason: "The preview shows only localhost, 127.0.0.1, or [::1]; remote pages stay out of the app." };
  }
  return { ok: true, url: url.toString() };
}

/** Who drives the session's browser. */
export type BrowserDriver = "agent" | "user";

/** The browser-control and preview state a session event changes; other events change nothing. */
export type PreviewState = {
  driver: BrowserDriver;
  preview: { url: string; command: string | null } | null;
};

export function applyPreviewEvent(
  state: PreviewState,
  event: { type: string; user?: boolean; url?: string | null; command?: string | null },
): PreviewState {
  if (event.type === "browser_control") return { ...state, driver: event.user ? "user" : "agent" };
  if (event.type === "preview_changed") {
    return { ...state, preview: event.url ? { url: event.url, command: event.command ?? null } : null };
  }
  return state;
}

/** Where a click on a displayed screenshot lands in the page, as fractions (0 to 1) of its size. */
export function clickFraction(
  rect: { left: number; top: number; width: number; height: number },
  clientX: number,
  clientY: number,
): { x: number; y: number } | null {
  if (rect.width <= 0 || rect.height <= 0) return null;
  const clamp = (value: number) => Math.min(1, Math.max(0, value));
  return { x: clamp((clientX - rect.left) / rect.width), y: clamp((clientY - rect.top) / rect.height) };
}

const NAMED_KEYS = new Set([
  "Enter",
  "Tab",
  "Backspace",
  "Delete",
  "Escape",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Home",
  "End",
  "PageUp",
  "PageDown",
]);

/** What a key press does in the driven browser: type a character, press a named key, or nothing. */
export function keyAction(event: {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}): { type: "type"; text: string } | { type: "key"; key: string } | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  if (NAMED_KEYS.has(event.key)) return { type: "key", key: event.key };
  // A printable character is a single code point; "Shift", "F5" and the like are not.
  if ([...event.key].length === 1) return { type: "type", text: event.key };
  return null;
}

/** An address typed in the Browser panel's bar, as an http(s) URL (a bare host gets https). */
export function browserAddress(input: string): string | null {
  const text = input.trim();
  if (!text || /\s/.test(text)) return null;
  const candidate = /^[a-z][a-z0-9+.-]*:\/\//i.test(text) ? text : `https://${text}`;
  try {
    const url = new URL(candidate);
    return url.protocol === "http:" || url.protocol === "https:" ? url.toString() : null;
  } catch {
    return null;
  }
}
