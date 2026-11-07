// The Imperium UI mod SDK. A mod is an ES module that calls `defineMod`; the desktop app runs
// it in a sandboxed frame (one per panel it contributes) and passes it a context through a
// message bridge. A mod never sees the daemon token, the filesystem, or other mods.

/** Telemetry for one finished model call (`telemetry:read`). */
export type CallCompleted = {
  sessionId: string;
  agentId: string;
  model: string;
  promptTokens: number;
  completionTokens: number;
  cachedTokens: number;
  durationMs: number;
  tokensPerSec: number | null;
  costUsd: number | null;
};

/** A raw session event (`sessions:read`), as the daemon protocol defines it. */
export type SessionEvent = { sessionId: string; agentId: string; event: { type: string } & Record<string, unknown> };

export type Permission = "telemetry:read" | "sessions:read";

export type PanelContribution = { id: string; title: string; slot?: string };

export type ModContext = {
  /** The panel this frame renders. */
  panelId: string;
  permissions: Permission[];
  telemetry: { subscribe(kind: "call.completed", listener: (call: CallCompleted) => void): void };
  sessions: { subscribe(listener: (event: SessionEvent) => void): void };
  /** Renders into a panel; only the frame's own panel renders, other ids are ignored. */
  panel(id: string): { render(content: string | Node): void };
};

export type ModDefinition = {
  id: string;
  permissions?: Permission[];
  contributes?: { panels?: PanelContribution[] };
  activate(ctx: ModContext): void;
};

type Host = { __imperiumDefine?: (definition: ModDefinition) => void };

/** Declares a mod. Inside the desktop app this activates it; elsewhere it just returns it. */
export function defineMod(definition: ModDefinition): ModDefinition {
  (globalThis as Host).__imperiumDefine?.(definition);
  return definition;
}
