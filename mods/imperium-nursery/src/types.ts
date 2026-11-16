// The arbiter's plain-data state, ported from Pixel Pets (MIT; see ../NOTICE). Self-contained.

/** One animation the arbiter has on screen. `kind` is how it ends. */
export type PetsShown = {
  /** Logical animation name (a pet that does not draw it shows its fallback). */
  name: string
  /** When it started, in `$.clock.now()` milliseconds. */
  at: number
  /** When it ends, or null for a loop that runs until the arbiter changes it. */
  until: number | null
  /** `loop` is a state, `once` a reaction, `look` a glance pose. */
  kind: 'loop' | 'once' | 'look'
  /** The source it is shown for: `main`, a subagent id, or null for ambient idle/sleep. */
  source: string | null
  /** Priority of `name`, 0 (highest) to 6. */
  priority: number
  /** Look direction in degrees, clockwise from up, while `kind` is `look`. */
  look: number | null
}

/** A one-shot waiting for the shown one to finish. */
export type PetsQueued = {
  name: string
  source: string | null
  /** The earliest time it may start. */
  notBefore: number
}

/** One producer of pet events: the main conversation or a running subagent. */
export type PetsSource = {
  kind: 'main' | 'sub'
  /** The loop its work holds (`working`, `review`), or null between turns. */
  base: string | null
  /** A loop laid over `base` while it lasts (`eating` during compaction). */
  over: string | null
  /** When the current loop started. */
  since: number
  /** Pending prompts (tool_use_id -> when it began): any makes the source `waiting`. */
  waits: Record<string, number>
}

/** The behaviour arbiter's whole state; plain data, so it survives a hot reload. */
export type PetsArbiter = {
  sources: Record<string, PetsSource>
  /** Look direction per source, kept after a source ends. */
  dirs: Record<string, number>
  shown: PetsShown
  queued: PetsQueued | null
  /** The source the pet last turned toward. */
  focus: string
  /** True before the first pop-out and after duck-in finishes. */
  isHidden: boolean
  /** True while duck-in plays. */
  isHiding: boolean
  burstAt: number
  burstPriority: number
  /** Last event time; the sleep timer counts from it. */
  activeAt: number
  /** The time of the last event applied. */
  touchedAt: number
  /** When the next fidget is due, or null. */
  fidgetAt: number | null
  seed: number
  spawned: number
}
