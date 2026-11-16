// Types for behaviour.js (plain JavaScript, so the app can embed it in mod frames as text).

export const LIMITS: Readonly<{
  maxBytes: number;
  maxFunctions: number;
  maxLocals: number;
  maxGlobals: number;
  maxPages: number;
  maxFuel: number;
  maxTickMs: number;
  maxSlowTicks: number;
  maxBehaviours: number;
  maxSpeed: number;
}>;
export const ANIMATIONS: readonly string[];
export const SHOTS: readonly string[];
export const ABI: Readonly<{
  version: number;
  inputWords: number;
  flags: Readonly<{ free: number; busy: number }>;
  outputStart: number;
  outputWords: number;
  clearedWords: number;
  minBytes: number;
}>;

export type Validated = { ok: true; fuel: number; pages: number } | { ok: false; error: string };
export function validateModule(bytes: Uint8Array): Validated;
export function seededRandom(seed: number): () => number;

export type TickState = {
  dt: number;
  anim: string;
  free: boolean;
  busy: boolean;
  quiet: number;
  x: number;
  y: number;
  bounds: { minX: number; maxX: number; minY: number; maxY: number };
};
export type TickResult = { ok: true; shot: string | null; vx: number; vy: number } | { ok: false; error: string };

export type RunningBehaviour = {
  readonly fuel: number;
  readonly stopped: boolean;
  readonly fault: string | null;
  tick(state: TickState): TickResult;
};
export type LoadResult = { ok: true; behaviour: RunningBehaviour & { close(): void } } | { ok: false; error: string; busy?: true };

export function loadBehaviour(
  bytes: Uint8Array,
  options?: { seed?: number; clock?: () => number; onFault?: (message: string) => void },
): { ok: true; behaviour: RunningBehaviour & { ticks(): number } } | { ok: false; error: string };

export type FaultReport = { name: string; phase: "load" | "tick"; reason: string };
export function createHost(options?: { report?: (info: FaultReport) => void; clock?: () => number }): {
  limits: typeof LIMITS;
  animations: typeof ANIMATIONS;
  shots: typeof SHOTS;
  abi: typeof ABI;
  load(name: string, bytes: Uint8Array, options?: { seed?: number }): LoadResult;
};
