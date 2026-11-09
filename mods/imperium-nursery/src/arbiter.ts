// The pet's behaviour arbiter (docs/pet-format-v3.md, "Behaviour priorities"): PURE.
// Ported from Pixel Pets (MIT; see ../NOTICE); its rules are unchanged. Every nursery pet
// owns one state (PetsArbiter) and the view reads it with `current`.
//
// It takes timestamped events and answers what animation is on screen, since when, and
// which way the pet looks. All state is plain data (PetsArbiter) so the mod keeps it in
// `$.state`. Time only moves when the caller says so: `dispatch` and `advance` take `now`.
//
// Reading of the spec where it is silent (recorded in the mod's report):
//  - A source's loop is `working`/`review`/`eating`, or `waiting` while a prompt is pending.
//    A subagent's `waiting` is drawn as `alert` while the main conversation's own loop is
//    P2 or lower (or absent), which is the spec's alert rule; otherwise it stays `waiting`.
//  - Coalescing (rule 3) plays the first one-shot at once and merges what follows within
//    1500 ms of the previous arrival: a one-shot of equal or lower priority is dropped, a
//    higher one replaces it. The window slides with every arrival.
//  - Glance (rule 6) is played whenever the animation to show belongs to a source other
//    than the one the pet last turned toward, for one `look` minMs, unless the animation
//    is P0 or P1 (those need the person now).
//  - Sleep counts from the last event; `sleepAfterMs <= 0` never sleeps.

import type { PetsArbiter, PetsShown, PetsSource } from './types.ts'
import type { Catalog } from './pet.ts'
import { resolveAnimation } from './pet.ts'

export type Env = {
  catalog: Catalog
  sleepAfterMs: number
  fidget: boolean
  /** Replaces the state's own generator, for tests. Returns [0, 1). */
  rng?: () => number
}

export type PetEvent =
  | { type: 'start' }
  | { type: 'show' }
  | { type: 'hide' }
  | { type: 'tick' }
  /** Sets (or with name null clears) a source's base loop; creates a subagent source. */
  | { type: 'loop'; source: string; name: string | null }
  /** Sets the loop only when the source has none. */
  | { type: 'ensure'; source: string; name: string }
  /** Lays a loop over the base loop (null removes it). */
  | { type: 'overlay'; source: string; name: string | null }
  | { type: 'spawn'; source: string; name: string }
  | { type: 'end'; source: string }
  | { type: 'block'; source: string; id: string }
  | { type: 'unblock'; source: string; id: string }
  | { type: 'shot'; name: string; source: string | null }

export const COALESCE_MS = 1500
export const MAIN_DIR = 180
/** Directions handed to subagents in spawn order: the lower half, 90 to 270 degrees. */
export const SUB_DIRS = [112.5, 247.5, 135, 225, 90, 270, 157.5, 202.5]
export const FIDGET_MIN_MS = 20_000
export const FIDGET_SPAN_MS = 40_000

export function initial(now: number, seed = 1): PetsArbiter {
  return {
    sources: { main: { kind: 'main', base: null, over: null, since: now, waits: {} } },
    dirs: { main: MAIN_DIR },
    shown: { name: 'idle', at: now, until: null, kind: 'loop', source: null, priority: 6, look: null },
    queued: null,
    focus: 'main',
    isHidden: true,
    isHiding: false,
    burstAt: -1e12,
    burstPriority: 6,
    activeAt: now,
    touchedAt: now,
    fidgetAt: null,
    seed: seed | 0 || 1,
    spawned: 0,
  }
}

const clone = (s: PetsArbiter): PetsArbiter => JSON.parse(JSON.stringify(s)) as PetsArbiter

// --- catalog lookups --------------------------------------------------------------------

const prio = (env: Env, name: string): number => env.catalog[name]?.priority ?? 4
const minMs = (env: Env, name: string): number => env.catalog[name]?.minMs ?? 600

/** A one-shot this pet can play: what it resolves to is drawn, not a loop, not `idle`. */
export function isPlayable(env: Env, name: string): boolean {
  const r = resolveAnimation(env.catalog, name)
  const a = env.catalog[r]
  return r !== 'idle' && a !== undefined && !a.loop && !a.pose
}

/** How long a one-shot plays: the frame durations of the animation it resolves to. */
export function playMs(env: Env, name: string): number {
  const a = env.catalog[resolveAnimation(env.catalog, name)]
  return Math.max(1, a ? a.durations.reduce((x, y) => x + y, 0) : 600)
}

const lookDrawn = (env: Env): boolean => resolveAnimation(env.catalog, 'look') === 'look'

// --- randomness -------------------------------------------------------------------------

function rand(s: PetsArbiter, env: Env): number {
  if (env.rng) return env.rng()
  // mulberry32, its state kept in the arbiter so a copy of the state draws the same.
  s.seed = (s.seed + 0x6d2b79f5) | 0
  let t = s.seed
  t = Math.imul(t ^ (t >>> 15), t | 1)
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61)
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296
}

// --- sources and the loop to show -------------------------------------------------------

type Target = { name: string; source: string; since: number; p: number }

const effLoop = (src: PetsSource): { name: string; since: number } | null => {
  const waits = Object.values(src.waits)
  if (waits.length) return { name: 'waiting', since: Math.min(...waits) }
  const name = src.over ?? src.base
  return name ? { name, since: src.since } : null
}

/** The loop rule 1 shows: the highest priority across sources, ties to the most recent. */
function target(s: PetsArbiter, env: Env): Target | null {
  const main = s.sources.main
  const mainLoop = main ? effLoop(main) : null
  const mainP = mainLoop ? prio(env, mainLoop.name) : 99
  let best: Target | null = null
  for (const [id, src] of Object.entries(s.sources)) {
    const l = effLoop(src)
    if (!l) continue
    let name = l.name
    // Another source needs the person while the shown source is busy with P2 or lower.
    if (id !== 'main' && name === 'waiting' && mainP >= 2) name = 'alert'
    const p = prio(env, name)
    if (!best || p < best.p || (p === best.p && l.since > best.since)) best = { name, source: id, since: l.since, p }
  }
  return best
}

function ensureSource(s: PetsArbiter, id: string, now: number): PetsSource {
  let src = s.sources[id]
  if (!src) {
    src = { kind: id === 'main' ? 'main' : 'sub', base: null, over: null, since: now, waits: {} }
    s.sources[id] = src
    if (s.dirs[id] === undefined) {
      s.dirs[id] = id === 'main' ? MAIN_DIR : (SUB_DIRS[s.spawned % SUB_DIRS.length] ?? 180)
      s.spawned++
      const keys = Object.keys(s.dirs)
      if (keys.length > 200) for (const k of keys) if (!s.sources[k] && k !== 'main' && Object.keys(s.dirs).length > 100) delete s.dirs[k]
    }
  }
  return src
}

// --- what is on screen ------------------------------------------------------------------

function show(s: PetsArbiter, env: Env, name: string, at: number, kind: PetsShown['kind'], source: string | null, until: number | null, look: number | null = null): void {
  s.shown = { name, at, until, kind, source, priority: prio(env, name), look }
}

function enterLoop(s: PetsArbiter, env: Env, name: string, at: number, source: string | null): void {
  show(s, env, name, at, 'loop', source, null)
  s.fidgetAt = name === 'idle' && env.fidget && isPlayable(env, 'fidget') ? at + FIDGET_MIN_MS + Math.floor(rand(s, env) * (FIDGET_SPAN_MS + 1)) : null
}

function playOnce(s: PetsArbiter, env: Env, name: string, at: number, source: string | null): void {
  show(s, env, name, at, 'once', source, at + playMs(env, name))
  s.fidgetAt = null
}

/** Turns toward `source` for one `look` minMs; false when the pet has no look cells or no direction. */
function glance(s: PetsArbiter, env: Env, source: string | null, at: number): boolean {
  if (!source || source === s.focus) return false
  s.focus = source
  const dir = s.dirs[source]
  if (!lookDrawn(env) || dir === undefined) return false
  show(s, env, 'look', at, 'look', source, at + minMs(env, 'look'), dir)
  s.fidgetAt = null
  return true
}

function startLoop(s: PetsArbiter, env: Env, tg: Target, t: number): void {
  if (tg.p >= 2 && glance(s, env, tg.source, t)) return
  s.focus = tg.source
  enterLoop(s, env, tg.name, t, tg.source)
}

function toBaseline(s: PetsArbiter, env: Env, t: number): void {
  const tg = target(s, env)
  if (tg) startLoop(s, env, tg, t)
  else enterLoop(s, env, 'idle', t, null)
}

function playShot(s: PetsArbiter, env: Env, name: string, source: string | null, t: number): void {
  if (prio(env, name) >= 2 && glance(s, env, source, t)) {
    s.queued = { name, source, notBefore: s.shown.until ?? t }
    return
  }
  if (source) s.focus = source
  playOnce(s, env, name, t, source)
}

function playQueued(s: PetsArbiter, env: Env, t: number): boolean {
  const q = s.queued
  s.queued = null
  if (!q || s.isHidden || !isPlayable(env, q.name)) return false
  playShot(s, env, q.name, q.source, t)
  return true
}

function enqueue(s: PetsArbiter, env: Env, q: NonNullable<PetsArbiter['queued']>): void {
  if (!s.queued || prio(env, q.name) < prio(env, s.queued.name)) s.queued = q
}

/** Waking first (rule 5): `waking` plays, then `q` (if any), then whatever loop wins. */
function wake(s: PetsArbiter, env: Env, q: PetsArbiter['queued'], t: number): void {
  if (isPlayable(env, 'waking')) {
    playOnce(s, env, 'waking', t, null)
    s.queued = q ? { ...q, notBefore: s.shown.until ?? t } : null
  } else {
    enterLoop(s, env, 'idle', t, null)
    s.queued = q ? { ...q, notBefore: t } : null
  }
}

function finish(s: PetsArbiter, env: Env, t: number): void {
  if (s.isHiding) {
    s.isHiding = false
    s.isHidden = true
    const q = s.queued
    s.queued = null
    enterLoop(s, env, 'idle', t, null)
    if (q?.name === 'pop-out') reveal(s, env, t, null)
    return
  }
  if (s.queued && playQueued(s, env, t)) return
  toBaseline(s, env, t)
}

function reveal(s: PetsArbiter, env: Env, t: number, then: string | null): void {
  s.isHidden = false
  s.isHiding = false
  if (isPlayable(env, 'pop-out')) {
    playOnce(s, env, 'pop-out', t, null)
    s.queued = then ? { name: then, source: 'main', notBefore: s.shown.until ?? t } : null
  } else {
    enterLoop(s, env, 'idle', t, null)
    s.queued = then ? { name: then, source: 'main', notBefore: t } : null
  }
}

// --- time ---------------------------------------------------------------------------------

function step(s: PetsArbiter, env: Env, now: number): boolean {
  const sh = s.shown
  const tg = target(s, env)
  if (sh.until !== null) {
    if (sh.until <= now) {
      finish(s, env, sh.until)
      return true
    }
    // Something more important is running: cut a one-shot short (P5 transitions only for P0/P1).
    if (!s.isHiding && tg && sh.kind === 'once' && tg.p < sh.priority && !(sh.priority === 5 && tg.p > 1)) {
      finish(s, env, Math.min(now, Math.max(s.touchedAt, sh.at)))
      return true
    }
    return false
  }
  if (s.isHidden) return false
  if (s.queued && s.queued.notBefore <= now) {
    playQueued(s, env, Math.min(now, Math.max(s.queued.notBefore, sh.at)))
    return true
  }
  if (sh.name === 'sleeping') {
    if (tg) {
      wake(s, env, null, Math.min(now, Math.max(s.touchedAt, sh.at)))
      return true
    }
    return false
  }
  if (tg) {
    if (sh.name === tg.name) {
      sh.source = tg.source
      s.focus = tg.source
      return false
    }
    const at = sh.at + minMs(env, sh.name)
    const waits = tg.p >= sh.priority
    if (waits && now < at) return false
    startLoop(s, env, tg, Math.min(now, Math.max(waits ? at : sh.at, s.touchedAt)))
    return true
  }
  if (sh.name !== 'idle') {
    const at = sh.at + minMs(env, sh.name)
    if (now < at) return false
    enterLoop(s, env, 'idle', Math.min(now, Math.max(at, s.touchedAt)), null)
    return true
  }
  const sleepAt = env.sleepAfterMs > 0 ? s.activeAt + env.sleepAfterMs : Infinity
  const fidgetAt = s.fidgetAt ?? Infinity
  if (fidgetAt <= now && fidgetAt <= sleepAt) {
    s.fidgetAt = null
    if (isPlayable(env, 'fidget')) playOnce(s, env, 'fidget', Math.max(fidgetAt, sh.at), null)
    return true
  }
  if (sleepAt <= now) {
    show(s, env, 'sleeping', Math.max(sleepAt, sh.at), 'loop', null, null)
    s.fidgetAt = null
    return true
  }
  return false
}

function run(s: PetsArbiter, env: Env, now: number): void {
  for (let i = 0; i < 64; i++) if (!step(s, env, now)) break
}

/** The state at `now`, with every elapsed transition applied. Does not change `state`. */
export function advance(state: PetsArbiter, now: number, env: Env): PetsArbiter {
  const s = clone(state)
  run(s, env, now)
  return s
}

// --- events -------------------------------------------------------------------------------

function handleShot(s: PetsArbiter, env: Env, name: string, source: string | null, now: number): void {
  if (s.isHidden || s.isHiding || !isPlayable(env, name)) return
  const p = prio(env, name)
  // Rule 3: one-shots within 1500 ms of each other merge; the highest priority plays once.
  const inBurst = now - s.burstAt < COALESCE_MS
  if (inBurst && p >= s.burstPriority) {
    s.burstAt = now
    return
  }
  s.burstPriority = inBurst ? Math.min(s.burstPriority, p) : p
  s.burstAt = now
  // Rule 5: waking first.
  if (s.shown.name === 'sleeping' && s.shown.kind === 'loop') {
    wake(s, env, { name, source, notBefore: now }, now)
    return
  }
  // Rule 2: a one-shot interrupts only a loop of equal or lower priority; otherwise it is dropped.
  const tg = target(s, env)
  if ((tg ? tg.p : 6) < p) return
  const sh = s.shown
  if (sh.until === null) {
    const at = sh.at + minMs(env, sh.name)
    // Rule 4: no flicker.
    if (p >= sh.priority && now < at) {
      enqueue(s, env, { name, source, notBefore: at })
      return
    }
    playShot(s, env, name, source, now)
    return
  }
  if (p < sh.priority && !(sh.kind === 'once' && sh.priority === 5 && p > 1)) {
    playShot(s, env, name, source, now)
    return
  }
  enqueue(s, env, { name, source, notBefore: sh.until })
}

function apply(s: PetsArbiter, env: Env, ev: PetEvent, now: number): void {
  const wakeUp = (): void => {
    if (!s.isHidden && s.shown.name === 'sleeping' && s.shown.kind === 'loop') wake(s, env, null, now)
  }
  switch (ev.type) {
    case 'tick':
      return
    case 'start':
      if (s.isHidden && !s.isHiding) reveal(s, env, now, 'waving')
      return
    case 'show':
      if (s.isHiding) s.queued = { name: 'pop-out', source: null, notBefore: s.shown.until ?? now }
      else if (s.isHidden) reveal(s, env, now, null)
      return
    case 'hide':
      if (s.isHidden || s.isHiding) return
      s.queued = null
      if (isPlayable(env, 'duck-in')) {
        playOnce(s, env, 'duck-in', now, null)
        s.isHiding = true
      } else {
        s.isHidden = true
        enterLoop(s, env, 'idle', now, null)
      }
      return
    case 'loop': {
      const src = ensureSource(s, ev.source, now)
      if (src.base !== ev.name) src.since = now
      src.base = ev.name
      if (ev.name) wakeUp()
      return
    }
    case 'ensure': {
      const src = ensureSource(s, ev.source, now)
      if (!src.base) {
        src.base = ev.name
        src.since = now
        wakeUp()
      }
      return
    }
    case 'overlay': {
      const src = ensureSource(s, ev.source, now)
      if (src.over !== ev.name) src.since = now
      src.over = ev.name
      if (ev.name) wakeUp()
      return
    }
    case 'spawn': {
      const src = ensureSource(s, ev.source, now)
      src.base = ev.name
      src.since = now
      wakeUp()
      return
    }
    case 'end':
      if (ev.source === 'main') {
        const m = ensureSource(s, 'main', now)
        m.base = null
        m.over = null
        m.waits = {}
        m.since = now
      } else delete s.sources[ev.source]
      return
    case 'block': {
      ensureSource(s, ev.source, now).waits[ev.id] = now
      wakeUp()
      return
    }
    case 'unblock': {
      const src = s.sources[ev.source]
      if (src) {
        delete src.waits[ev.id]
        src.since = now
      }
      return
    }
    case 'shot':
      handleShot(s, env, ev.name, ev.source, now)
      return
  }
}

/** Applies `ev` at `now`: first every elapsed transition, then the event, then what it sets off. */
export function dispatch(state: PetsArbiter, ev: PetEvent, now: number, env: Env): PetsArbiter {
  const s = clone(state)
  run(s, env, now)
  s.touchedAt = now
  if (ev.type !== 'tick') s.activeAt = now
  apply(s, env, ev, now)
  run(s, env, now)
  return s
}

// --- answers --------------------------------------------------------------------------------

export type Current = {
  /** The logical animation (what the arbiter chose); resolve it against the pet to draw it. */
  name: string
  startedAt: number
  kind: PetsShown['kind']
  /** Look direction in degrees while `kind` is `look`. */
  look: number | null
  source: string | null
  isHidden: boolean
}

/** What to draw at `now`. */
export function current(state: PetsArbiter, now: number, env: Env): Current {
  const s = advance(state, now, env)
  return { name: s.shown.name, startedAt: s.shown.at, kind: s.shown.kind, look: s.shown.look, source: s.shown.source, isHidden: s.isHidden }
}

/** The next time the answer changes by itself (no event), or null when it never does. */
export function nextWake(state: PetsArbiter, now: number, env: Env): number | null {
  const s = advance(state, now, env)
  const sh = s.shown
  const times: number[] = []
  if (sh.until !== null) times.push(sh.until)
  else if (!s.isHidden) {
    if (s.queued) times.push(s.queued.notBefore)
    const tg = target(s, env)
    if (sh.name === 'sleeping') {
      // Stays asleep until an event.
    } else if (tg) {
      if (sh.name !== tg.name && tg.p >= sh.priority) times.push(sh.at + minMs(env, sh.name))
    } else if (sh.name !== 'idle') times.push(sh.at + minMs(env, sh.name))
    else {
      if (s.fidgetAt !== null) times.push(s.fidgetAt)
      if (env.sleepAfterMs > 0) times.push(s.activeAt + env.sleepAfterMs)
    }
  }
  const future = times.filter(t => t > now)
  return future.length ? Math.min(...future) : null
}

/** Frame index of a look direction in the 16-cell `look` strip (000, 022.5, ... 337.5). */
export const lookIndex = (degrees: number): number => ((Math.round(degrees / 22.5) % 16) + 16) % 16
