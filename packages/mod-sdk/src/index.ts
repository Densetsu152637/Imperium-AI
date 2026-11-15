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
  /** Whether it is an environment's persistent agent session (absent from older hosts). */
  persistent?: boolean;
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

/** What a behaviour is told each tick (all integers; velocities and positions in px). */
export type BehaviourState = {
  /** Milliseconds since this behaviour's last tick, 0 to 1000. */
  dt: number;
  /** The animation the pet shows now (one of `behaviours.animations`). */
  anim: string;
  /** The pet is idle and free to roam. */
  free: boolean;
  /** The pet is working, waiting or reacting. */
  busy: boolean;
  /** Milliseconds since the pet last had an event, 0 to 600000. */
  quiet: number;
  x: number;
  y: number;
  /** The rectangle the pet's feet may be in. */
  bounds: { minX: number; maxX: number; minY: number; maxY: number };
};

/** A behaviour's answer: a one-shot to request (a name from `behaviours.shots`, or null) and a velocity in px/s. */
export type BehaviourTick = { ok: true; shot: string | null; vx: number; vy: number } | { ok: false; error: string };

/** A running WebAssembly behaviour. After one fault it is stopped for good: every tick reports an error. */
export type Behaviour = {
  readonly stopped: boolean;
  readonly fault: string | null;
  /** The most instructions one tick can run, proved when it was loaded. */
  readonly fuel: number;
  tick(state: BehaviourState): BehaviourTick;
  /** Gives its slot back (a frame runs at most `limits.maxBehaviours` at once). */
  close(): void;
};

/** `busy` is set when the frame is at its limit of live behaviours: the module itself was not refused, so a later load may succeed. */
export type BehaviourLoad = { ok: true; behaviour: Behaviour } | { ok: false; error: string; busy?: true };

/** Runs pet behaviours shipped as WebAssembly, sandboxed: see docs/06-extensibility.md. */
export type Behaviours = {
  /**
   * Checks and starts a module. `name` labels it in reports. `seed` seeds the module's only import,
   * `imperium.rand`. A module that breaks the rules, or later traps, takes too long or answers
   * with something invalid, is refused or stopped and reported to the person in Settings → Mods.
   */
  load(name: string, bytes: Uint8Array, options?: { seed?: number }): BehaviourLoad;
  readonly limits: { maxBytes: number; maxFuel: number; maxPages: number; maxTickMs: number; maxBehaviours: number; maxSpeed: number };
  /** Animation names, by the index the module reads from its input. */
  readonly animations: readonly string[];
  /** The one-shots a module may request, by 1-based index in its output. */
  readonly shots: readonly string[];
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
  /** WebAssembly pet behaviours. Needs no permission: a module can touch nothing but its own memory. */
  behaviours: Behaviours;
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
