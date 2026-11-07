// Runs one UI mod panel in a sandboxed frame. The frame has an opaque origin (no
// allow-same-origin), so the mod cannot reach this window, the daemon token, or other mods;
// it receives only the events its permissions allow, through postMessage.

import { useEffect, useMemo, useRef } from "react";

import type { ModInfo, SessionEventParams } from "@imperium/protocol";

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

// Receives events from the app and builds the context `activate` gets.
const BRIDGE = `
const config = __CONFIG__;
const listeners = {};
const on = (kind, listener) => { (listeners[kind] ||= []).push(listener); };
window.addEventListener("message", (message) => {
  if (message.source !== window.parent || !message.data) return;
  for (const listener of listeners[message.data.kind] || []) {
    try { listener(message.data.payload); } catch (error) { console.error(error); }
  }
});
globalThis.__imperiumDefine = (definition) => {
  const ctx = {
    panelId: config.panelId,
    permissions: config.permissions,
    telemetry: { subscribe: (kind, listener) => on("telemetry:" + kind, listener) },
    sessions: { subscribe: (listener) => on("session", listener) },
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
  return `:root { ${tokens} color-scheme: light dark; }
body { margin: 0; padding: 8px; background: transparent; color: var(--fg); font: ${style.getPropertyValue("font-size")} ${style.fontFamily}; }`;
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

export function ModFrame({ mod, panelId, bus }: { mod: ModInfo; panelId: string; bus: ModBus }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const document = useMemo(() => frameDocument(mod, panelId), [mod, panelId]);

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
      srcDoc={document}
      title={`${mod.name}: ${panelId}`}
    />
  );
}
