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

/** What one agent in a session is doing, as the app sees it now (`sessions:read`). */
export type AgentSnapshot = {
  /** `root` for the session's main agent; a subagent's id otherwise (the `agentId` of its events). */
  agentId: string;
  /** The agent that spawned it; `null` for the root agent. */
  parentId: string | null;
  /** The task a subagent was given; empty for the root agent. */
  brief: string;
  status: "working" | "waiting" | "idle" | "finished" | "failed";
};

/** One session the app knows about (`sessions:read`). */
export type SessionSnapshot = {
  sessionId: string;
  title: string;
  projectDir: string;
  /** The daemon's session status (`idle`, `working`, `waiting`, ...). */
  status: string;
  /** The environment the session runs in, or null for the default one. */
  environmentId: string | null;
  /** Whether the session is archived. */
  archived: boolean;
  /** The environment's default pet (an installed pet id), or null when the nursery decides. */
  pet: string | null;
  /** When the session was created, in ms since the Unix epoch; null when unknown. */
  createdAt: number | null;
  /** The root agent first, then subagents in the order they were spawned. */
  agents: AgentSnapshot[];
};

/** An installed Pixel Pets v3 pet (`pets:read`). */
export type PetPackage = {
  id: string;
  displayName: string;
  description: string;
  author: string;
  license: string;
  cellWidth: number;
  cellHeight: number;
  /** The package's `pet.json`, unparsed. */
  petJson: string;
  /** Animations the pet draws (a strip exists for each); the rest fall back. */
  drawn: string[];
};

export type Permission = "telemetry:read" | "sessions:read" | "pets:read" | "storage";

export type PanelContribution = { id: string; title: string; slot?: string };

export type ModContext = {
  /** The panel this frame renders. */
  panelId: string;
  permissions: Permission[];
  telemetry: { subscribe(kind: "call.completed", listener: (call: CallCompleted) => void): void };
  sessions: {
    /** Every raw session event, as it happens. */
    subscribe(listener: (event: SessionEvent) => void): void;
    /** The sessions the app knows about now. */
    list(): Promise<SessionSnapshot[]>;
    /** The session selected in the app's window, if any. */
    selected(): Promise<string | null>;
    /** Called with the sessions and the selection whenever either changes. */
    onChange(listener: (sessions: SessionSnapshot[], selected: string | null) => void): void;
  };
  pets: {
    /** Every installed pet, whether or not the daemon's own pet is on. */
    list(): Promise<PetPackage[]>;
    /** A `data:image/png` URL of one animation's strip; rejects for an animation the pet does not draw. */
    strip(petId: string, animation: string): Promise<string>;
  };
  /** Per-mod values the app keeps across restarts, shared by the mod's panels. Values are JSON. */
  storage: {
    get<T = unknown>(key: string): Promise<T | undefined>;
    set(key: string, value: unknown): Promise<void>;
  };
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
