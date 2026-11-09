// Runs one UI mod panel in a sandboxed frame. The frame has an opaque origin (no
// allow-same-origin), so the mod cannot reach this window, the daemon token, or other mods;
// it receives only the events its permissions allow, through postMessage, and asks for the rest
// with request/response messages that this file checks against those permissions. In the desktop
// app the frame loads a page from the `imperium-mod` protocol (src-tauri/src/mod_frames.rs) and
// is handed its document once that page says it is ready, since the app's content security
// policy would block a `srcdoc` frame's scripts; in a plain browser it uses `srcdoc`.

import { convertFileSrc } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef } from "react";

import type { PetPackage, SessionSnapshot } from "@imperium/mod-sdk";
import type { ModInfo, PetListResult, PetStripResult, SessionEventParams } from "@imperium/protocol";

import type { DaemonClient } from "../daemon/client";
import { insideTauri } from "../daemon/connection";
import { getDoc, setDoc } from "../store";

type Listener = (params: SessionEventParams) => void;

/** Session events fanned out to mod frames. */
export class ModBus {
  private listeners = new Set<Listener>();

  emit(params: SessionEventParams): void {
    this.listeners.forEach((listener) => listener(params));
  }

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

// What `import { defineMod } from "@imperium/mod-sdk"` resolves to inside a frame; it matches
// packages/mod-sdk, so bundled and unbundled mods behave the same.
const SDK = "export function defineMod(definition) { globalThis.__imperiumDefine?.(definition); return definition; }";

// Receives events from the app, asks it for data, and builds the context `activate` gets.
// Requests are `{ kind: "request", id, method, params }`; the app answers with
// `{ kind: "response", id, result }` or `{ kind: "response", id, error }`.
const BRIDGE = `
const config = __CONFIG__;
const listeners = {};
const on = (kind, listener) => { (listeners[kind] ||= []).push(listener); };
const waiting = new Map();
let nextRequest = 1;
const request = (method, params) => new Promise((resolve, reject) => {
  const id = nextRequest++;
  waiting.set(id, { resolve, reject });
  window.parent.postMessage({ kind: "request", id, method, params }, "*");
});
window.addEventListener("message", (message) => {
  if (message.source !== window.parent || !message.data) return;
  if (message.data.kind === "response") {
    const pending = waiting.get(message.data.id);
    if (!pending) return;
    waiting.delete(message.data.id);
    if ("error" in message.data) pending.reject(new Error(message.data.error));
    else pending.resolve(message.data.result);
    return;
  }
  for (const listener of listeners[message.data.kind] || []) {
    try { listener(message.data.payload); } catch (error) { console.error(error); }
  }
});
globalThis.__imperiumDefine = (definition) => {
  const ctx = {
    panelId: config.panelId,
    permissions: config.permissions,
    telemetry: { subscribe: (kind, listener) => on("telemetry:" + kind, listener) },
    sessions: {
      subscribe: (listener) => on("session", listener),
      list: () => request("sessions.list"),
      selected: () => request("sessions.selected"),
      onChange: (listener) => on("sessions.changed", (change) => listener(change.sessions, change.selected)),
    },
    pets: {
      list: () => request("pets.list"),
      strip: (petId, animation) => request("pets.strip", { petId, animation }),
    },
    storage: {
      get: (key) => request("storage.get", { key }),
      set: (key, value) => request("storage.set", { key, value }),
    },
    panel: (id) => ({
      render: (content) => {
        if (id !== config.panelId) return;
        if (typeof content === "string") document.body.innerHTML = content;
        else document.body.replaceChildren(content);
      },
    }),
  };
  try { definition.activate(ctx); } catch (error) { document.body.textContent = "This mod failed to start: " + error; }
};
`;

/** CSS custom properties the frame inherits from the app's theme. */
const THEME_TOKENS = ["--bg", "--panel", "--fg", "--muted", "--border", "--accent", "--ok", "--danger", "--space", "--radius"];

function themeCss(): string {
  const style = getComputedStyle(document.documentElement);
  const tokens = THEME_TOKENS.map((token) => `${token}: ${style.getPropertyValue(token)};`).join(" ");
  // The app sets its typeface on the body, not the root.
  const text = getComputedStyle(document.body);
  return `:root { ${tokens} color-scheme: light dark; }
body { margin: 0; padding: 8px; background: transparent; color: var(--fg); font: ${text.fontSize} ${text.fontFamily}; }
button, input, select, textarea { font: inherit; }`;
}

const escapeScript = (text: string) => text.replace(/<\/script/gi, "<\\/script");

function frameDocument(mod: ModInfo, panelId: string): string {
  const sdk = `data:text/javascript;base64,${btoa(SDK)}`;
  const config = JSON.stringify({ panelId, permissions: mod.permissions }).replace(/</g, "\\u003c");
  return `<!doctype html><html><head><meta charset="utf-8"><style>${themeCss()}</style>
<script type="importmap">${JSON.stringify({ imports: { "@imperium/mod-sdk": sdk } })}</script>
<script>${escapeScript(BRIDGE.replace("__CONFIG__", config))}</script>
</head><body></body><script type="module">${escapeScript(mod.source)}</script></html>`;
}

/** The messages a mod may receive for one session event, given its permissions. */
function messagesFor(mod: ModInfo, { session_id, agent_id, event }: SessionEventParams) {
  const messages: { kind: string; payload: unknown }[] = [];
  if (mod.permissions.includes("sessions:read")) {
    messages.push({ kind: "session", payload: { sessionId: session_id, agentId: agent_id, event } });
  }
  if (mod.permissions.includes("telemetry:read") && event.type === "model_call_completed") {
    messages.push({
      kind: "telemetry:call.completed",
      payload: {
        sessionId: session_id,
        agentId: agent_id,
        model: event.model,
        promptTokens: event.usage.prompt_tokens,
        completionTokens: event.usage.completion_tokens,
        cachedTokens: event.usage.cached_tokens,
        durationMs: event.duration_ms,
        tokensPerSec: event.tokens_per_sec,
        costUsd: event.cost_usd,
      },
    });
  }
  return messages;
}

/** What a frame may ask for, and the permission each request needs. */
const REQUEST_PERMISSION: Record<string, string> = {
  "sessions.list": "sessions:read",
  "sessions.selected": "sessions:read",
  "pets.list": "pets:read",
  "pets.strip": "pets:read",
  "storage.get": "storage",
  "storage.set": "storage",
};

const MAX_KEY_LENGTH = 128;
const MAX_STORAGE_BYTES = 256 * 1024;
/** Sessions are pushed to a frame at most this often. */
const PUSH_INTERVAL_MS = 250;

type ModStorage = Record<string, Record<string, unknown>>;

/** What a mod has stored, by key (own properties only, so a key like `__proto__` is just a key). */
const storedFor = (modId: string): Record<string, unknown> => getDoc<ModStorage>("mods")?.[modId] ?? {};

function storageSet(modId: string, params: Record<string, unknown>): void {
  const { key, value } = params;
  if (typeof key !== "string" || key.length === 0 || key.length > MAX_KEY_LENGTH) {
    throw new Error(`a storage key is a string of 1-${MAX_KEY_LENGTH} characters`);
  }
  const next = { ...storedFor(modId) };
  if (value === undefined) {
    delete next[key];
  } else {
    const text = JSON.stringify(value);
    if (text === undefined) throw new Error("the value is not JSON");
    next[key] = JSON.parse(text);
  }
  if (new TextEncoder().encode(JSON.stringify(next)).length > MAX_STORAGE_BYTES) {
    throw new Error("a mod may store at most 256 KiB");
  }
  setDoc<ModStorage>("mods", { ...getDoc<ModStorage>("mods"), [modId]: next });
}

/** Strips by pet and animation; pet packages do not change while the app runs. */
const stripCache = new Map<string, string>();

async function petStrip(client: DaemonClient, params: Record<string, unknown>): Promise<string> {
  const { petId, animation } = params;
  if (typeof petId !== "string" || typeof animation !== "string") throw new Error("pets.strip needs a pet id and an animation");
  const cached = stripCache.get(`${petId}\n${animation}`);
  if (cached) return cached;
  const { png_base64 } = await client.call<PetStripResult>("pet.strip", { pet_id: petId, animation });
  const url = `data:image/png;base64,${png_base64}`;
  stripCache.set(`${petId}\n${animation}`, url);
  return url;
}

async function petList(client: DaemonClient): Promise<PetPackage[]> {
  const { pets } = await client.call<PetListResult>("pet.list");
  return pets.map((pet) => ({
    id: pet.id,
    displayName: pet.display_name,
    description: pet.description,
    author: pet.author,
    license: pet.license,
    cellWidth: pet.cell_width,
    cellHeight: pet.cell_height,
    petJson: pet.pet_json,
    drawn: pet.drawn,
  }));
}

/** The page frames load in the desktop app, or null to use `srcdoc`. */
const framePage = (): string | null => (insideTauri() ? convertFileSrc("frame.html", "imperium-mod") : null);

/** What the app shows of itself to mods; the frame reads it through requests and pushes. */
export type ModHost = { sessions: SessionSnapshot[]; selected: string | null; client: DaemonClient | null };

export function ModFrame({ mod, panelId, bus, host }: { mod: ModInfo; panelId: string; bus: ModBus; host: ModHost }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const document = useMemo(() => frameDocument(mod, panelId), [mod, panelId]);
  const latest = useRef(host);
  latest.current = host;
  const page = useMemo(framePage, []);

  // Hands the frame page its document when it says it is ready (desktop app only).
  useEffect(() => {
    if (!page) return;
    const onMessage = (message: MessageEvent) => {
      const target = frame.current?.contentWindow;
      if (!target || message.source !== target || message.origin !== "null") return;
      if ((message.data as { kind?: unknown } | null)?.kind !== "imperium.frame-ready") return;
      target.postMessage({ kind: "imperium.frame", html: document }, "*");
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [page, document]);

  // Requests from the frame. Only its own window is believed, and only for what it was granted.
  useEffect(() => {
    const onMessage = (message: MessageEvent) => {
      const target = frame.current?.contentWindow;
      const data = message.data as { kind?: unknown; id?: unknown; method?: unknown; params?: unknown } | null;
      // The frame's origin is opaque ("null"), so the sending window is what identifies it.
      if (!target || message.source !== target || message.origin !== "null") return;
      if (data?.kind !== "request" || typeof data.id !== "number" || typeof data.method !== "string") return;
      const { id, method } = data;
      const params = (typeof data.params === "object" && data.params !== null ? data.params : {}) as Record<string, unknown>;
      const reply = (body: { result: unknown } | { error: string }) => target.postMessage({ kind: "response", id, ...body }, "*");
      const needs = Object.hasOwn(REQUEST_PERMISSION, method) ? REQUEST_PERMISSION[method] : undefined;
      if (!needs) return reply({ error: `unknown method ${method}` });
      if (!mod.permissions.includes(needs)) return reply({ error: `${method} needs the ${needs} permission` });
      const { sessions, selected, client } = latest.current;
      const daemon = () => {
        if (!client) throw new Error("not connected to imperiumd");
        return client;
      };
      const run = async (): Promise<unknown> => {
        switch (method) {
          case "sessions.list":
            return sessions;
          case "sessions.selected":
            return selected;
          case "pets.list":
            return petList(daemon());
          case "pets.strip":
            return petStrip(daemon(), params);
          case "storage.get":
            return typeof params.key === "string" && Object.hasOwn(storedFor(mod.id), params.key)
              ? storedFor(mod.id)[params.key]
              : undefined;
          default:
            storageSet(mod.id, params);
            return undefined;
        }
      };
      run().then(
        (result) => reply({ result }),
        (error: unknown) => reply({ error: error instanceof Error ? error.message : String(error) }),
      );
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [mod]);

  // Pushes the sessions and selection when they change, at most every PUSH_INTERVAL_MS and only
  // when the snapshot differs from the last one sent (streaming text changes nothing in it).
  const lastSent = useRef<string | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (!mod.permissions.includes("sessions:read")) return;
    const payload = () => JSON.stringify({ sessions: latest.current.sessions, selected: latest.current.selected });
    if (lastSent.current === null) {
      lastSent.current = payload();
      return;
    }
    if (timer.current !== undefined) return;
    timer.current = window.setTimeout(() => {
      timer.current = undefined;
      const text = payload();
      if (text === lastSent.current) return;
      lastSent.current = text;
      frame.current?.contentWindow?.postMessage({ kind: "sessions.changed", payload: JSON.parse(text) }, "*");
    }, PUSH_INTERVAL_MS);
  }, [mod, host.sessions, host.selected]);
  useEffect(
    () => () => {
      window.clearTimeout(timer.current);
      timer.current = undefined;
    },
    [],
  );

  useEffect(
    () =>
      bus.subscribe((params) => {
        for (const message of messagesFor(mod, params)) {
          // The frame's origin is opaque, so "*" is the only target that reaches it.
          frame.current?.contentWindow?.postMessage(message, "*");
        }
      }),
    [bus, mod],
  );

  return (
    <iframe
      ref={frame}
      className="mod-frame"
      sandbox="allow-scripts"
      // A new document reloads the page, which asks for it again.
      key={page ? document : undefined}
      src={page ?? undefined}
      srcDoc={page ? undefined : document}
      title={`${mod.name}: ${panelId}`}
    />
  );
}
