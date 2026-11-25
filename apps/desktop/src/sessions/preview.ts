// The Preview panel's rules (roadmap item 9): which pages it may show, and how the panel and
// the Browser panel follow the session's preview and browser-control events.
//
// A preview runs the project's own page inside the app's window, so it is limited to loopback
// addresses. The daemon applies the same rule to the URLs agents give (`loopback_url` in
// imperium-web), and the app's content security policy (`frame-src` in tauri.conf.json) allows
// frames from loopback origins only, so even a page that redirects cannot load a remote site.

/** The origin the app runs from (a development build is served from localhost), if known. */
function currentOrigin(): string | null {
  return typeof location === "undefined" ? null : location.origin;
}

export type PreviewUrl = { ok: true; url: string } | { ok: false; reason: string };

/** `input` as a preview URL, or why it cannot be one. */
export function previewUrl(input: string, appOrigin: string | null = currentOrigin()): PreviewUrl {
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
  if (host !== "localhost" && host !== "127.0.0.1") {
    return { ok: false, reason: "The preview shows only localhost or 127.0.0.1; remote pages stay out of the app." };
  }
  // In a development build the app is itself served from localhost: framing it would give the
  // frame the app's own origin, which an iframe sandbox cannot contain.
  if (appOrigin && url.origin === appOrigin) {
    return { ok: false, reason: "That is the app itself; the preview shows the project's page, not Imperium." };
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

/** One tab's latest page, as the agent's actions and the user's last showed it. */
export type BrowserPageView = { url: string; title: string; screenshot: string };

/** One open tab of the session's browser. */
export type BrowserTabState = { id: string; url: string; title: string; active: boolean };

/** The tabs of the session's browser and the latest page of each, for one mirror per tab. */
export type BrowserMirror = {
  /** The tab the browser tools work on; null from a daemon that does not name tabs. */
  active: string | null;
  tabs: BrowserTabState[];
  pages: Record<string, BrowserPageView>;
};

/** The key a page is kept under when the daemon does not name its tab (protocol before 0.35). */
export const UNNAMED_TAB = "0";

/**
 * The mirror after a `browser_view` event: the page is kept under its tab, the tab list is
 * replaced by the event's, and pages of tabs that are no longer open are dropped. An event
 * without tabs (an older daemon) leaves a single unnamed tab.
 */
export function applyBrowserView(
  mirror: BrowserMirror | null,
  event: { url: string; title: string; screenshot: string; tab?: string | null; tabs?: BrowserTabState[] },
): BrowserMirror {
  const id = event.tab ?? UNNAMED_TAB;
  const tabs = event.tabs ?? [];
  const open = tabs.length > 0 ? tabs.map((tab) => tab.id) : [id];
  const pages: Record<string, BrowserPageView> = {};
  for (const key of open) {
    const page = key === id ? undefined : mirror?.pages[key];
    if (page) pages[key] = page;
  }
  pages[id] = { url: event.url, title: event.title, screenshot: event.screenshot };
  return { active: event.tab ?? null, tabs, pages };
}

/** The tab a client shows: the one it chose if that tab still has a page, else the active one. */
export function shownTab(mirror: BrowserMirror | null, chosen: string | null): string | null {
  if (!mirror) return null;
  if (chosen && mirror.pages[chosen]) return chosen;
  return mirror.active ?? UNNAMED_TAB;
}
