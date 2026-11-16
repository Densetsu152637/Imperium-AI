import { test } from 'node:test'

import { expect } from './expect.ts'

import { COALESCE_MS, SUB_DIRS, advance, current, dispatch, initial, isPlayable, lookIndex, nextWake, playMs } from '../src/arbiter.ts'
import type { Env, PetEvent } from '../src/arbiter.ts'
import { fallbackChain, resolveAnimation, standardCatalog, withDrawn } from '../src/pet.ts'
import type { PetsArbiter } from '../src/types.ts'

const ALL = Object.keys(standardCatalog())
const T0 = 1_000_000
const SLEEP = 300_000

const envOf = (names: string[] = ALL, extra: Partial<Env> = {}): Env => ({
  catalog: withDrawn(standardCatalog(), new Set(names)),
  sleepAfterMs: SLEEP,
  fidget: true,
  rng: () => 0,
  ...extra,
})

// A revealed pet at T0 whose pop-out and waving have played.
const started = (env = envOf(), at = T0): PetsArbiter => {
  let s = dispatch(initial(at), { type: 'start' }, at, env)
  s = advance(s, at + 5000, env)
  return s
}

const send = (s: PetsArbiter, at: number, env: Env, ...evs: PetEvent[]) => evs.reduce((acc, ev) => dispatch(acc, ev, at, env), s)
const name = (s: PetsArbiter, at: number, env: Env) => current(s, at, env).name

test('start: pop-out, then waving, then idle', () => {
  const env = envOf()
  let s = dispatch(initial(T0), { type: 'start' }, T0, env)
  expect(name(s, T0, env)).toBe('pop-out')
  const popMs = playMs(env, 'pop-out')
  expect(name(s, T0 + popMs - 1, env)).toBe('pop-out')
  expect(name(s, T0 + popMs + 1, env)).toBe('waving')
  s = advance(s, T0 + popMs + playMs(env, 'waving') + 1, env)
  expect(name(s, T0 + 5000, env)).toBe('idle')
  expect(current(s, T0 + 5000, env).isHidden).toBe(false)
})

test('before start the pet is hidden', () => {
  const env = envOf()
  expect(current(initial(T0), T0, env).isHidden).toBe(true)
})

test('rule 1: a loop is a state; the highest priority across sources shows, ties go to the most recent', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  expect(name(s, t, env)).toBe('working')
  // A later source on the same priority takes over after the dwell; a review loop differs by name.
  s = send(s, t + 200, env, { type: 'spawn', source: 'a1', name: 'review' })
  expect(current(s, t + 200, env).source).toBe('main')
  expect(current(s, t + 1700, env).source).toBe('a1')
  // Main waiting is P1 and outranks any P4 loop at once.
  s = send(s, t + 2000, env, { type: 'block', source: 'main', id: 'x' })
  expect(name(s, t + 2000, env)).toBe('waiting')
  expect(current(s, t + 2000, env).source).toBe('main')
})

test('rule 1: the tie goes to the source that started last', () => {
  const env = envOf(ALL.filter(n => n !== 'look'))
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'spawn', source: 'a1', name: 'review' })
  expect(current(s, t + 2000, env).name).toBe('review')
  s = send(s, t + 3000, env, { type: 'spawn', source: 'a2', name: 'working' })
  expect(current(s, t + 4000, env).source).toBe('a2')
  s = send(s, t + 5000, env, { type: 'end', source: 'a2' })
  expect(current(s, t + 6000, env).source).toBe('a1')
})

test('rule 2: a one-shot interrupts a loop of equal or lower priority, then the loop resumes', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'shot', name: 'jumping', source: 'main' })
  expect(name(s, t + 1000, env)).toBe('jumping')
  expect(name(s, t + 1000 + playMs(env, 'jumping') + 1, env)).toBe('working')
})

test('rule 2: a one-shot of lower priority than the shown loop is dropped, not queued', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'shot', name: 'waving', source: 'main' })
  expect(name(s, t + 1000, env)).toBe('working')
  expect(s.queued).toBeNull()
  // failed (P2) against waiting (P1)
  s = send(s, t + 5000, env, { type: 'block', source: 'main', id: 'p' })
  s = send(s, t + 9000, env, { type: 'shot', name: 'failed', source: 'main' })
  expect(name(s, t + 9000, env)).toBe('waiting')
  expect(name(s, t + 20_000, env)).toBe('waiting')
})

test('rule 3: one-shots within 1500 ms merge; the highest priority plays once', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  for (let i = 0; i < 5; i++) s = send(s, t + i * 250, env, { type: 'shot', name: 'celebrating', source: `a${i}` })
  expect(current(s, t + 1000, env).name).toBe('celebrating')
  expect(current(s, t + 1000, env).startedAt).toBe(t)
  // Played once: after it ends, no second celebration.
  expect(name(s, t + playMs(env, 'celebrating') + 50, env)).toBe('idle')
  // A higher-priority one in the same burst replaces it.
  s = send(s, t + 1300, env, { type: 'shot', name: 'failed', source: 'a0' })
  expect(name(s, t + 1300, env)).toBe('failed')
  // Outside the window a new one plays.
  s = send(s, t + 1300 + COALESCE_MS + playMs(env, 'failed'), env, { type: 'shot', name: 'celebrating', source: 'a0' })
  expect(name(s, t + 1300 + COALESCE_MS + playMs(env, 'failed'), env)).toBe('celebrating')
})

test('rule 4: minMs keeps an animation on screen before a same or lower priority one replaces it', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 50, env, { type: 'block', source: 'main', id: 'p' })
  expect(name(s, t + 50, env)).toBe('waiting')
  s = send(s, t + 150, env, { type: 'unblock', source: 'main', id: 'p' })
  expect(name(s, t + 400, env)).toBe('waiting')
  expect(name(s, t + 50 + 600 + 1, env)).toBe('working')
  expect(nextWake(s, t + 150, env)).toBe(t + 50 + 600)
})

test('rule 4: a P5 transition finishes unless a P0 or P1 event arrives', () => {
  const env = envOf()
  const popMs = playMs(env, 'pop-out')
  // pop-out (P5) is playing: a failed (P2) waits for it.
  let s = dispatch(initial(T0), { type: 'start' }, T0, env)
  s = send(s, T0 + 100, env, { type: 'shot', name: 'failed', source: 'main' })
  expect(name(s, T0 + 100, env)).toBe('pop-out')
  expect(name(s, T0 + popMs + 10, env)).toBe('failed')
  // petted (P0) cuts it, and the queued waving still follows.
  let v = dispatch(initial(T0), { type: 'start' }, T0, env)
  v = send(v, T0 + 100, env, { type: 'shot', name: 'petted', source: 'main' })
  expect(name(v, T0 + 100, env)).toBe('petted')
  expect(name(v, T0 + 100 + playMs(env, 'petted') + 10, env)).toBe('waving')
  // waiting (P1) cuts it too.
  let w = dispatch(initial(T0), { type: 'start' }, T0, env)
  w = send(w, T0 + 100, env, { type: 'block', source: 'main', id: 'p' })
  expect(name(w, T0 + 100, env)).toBe('waiting')
})

test('rule 5: waking plays first from sleeping, before a one-shot and before a loop', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 5000 + SLEEP
  expect(name(s, t + 1, env)).toBe('sleeping')
  const wokenOnShot = send(s, t + 10, env, { type: 'shot', name: 'jumping', source: 'main' })
  expect(name(wokenOnShot, t + 10, env)).toBe('waking')
  expect(name(wokenOnShot, t + 10 + playMs(env, 'waking') + 1, env)).toBe('jumping')
  const wokenOnLoop = send(s, t + 10, env, { type: 'loop', source: 'main', name: 'working' })
  expect(name(wokenOnLoop, t + 10, env)).toBe('waking')
  expect(name(wokenOnLoop, t + 10 + playMs(env, 'waking') + 1, env)).toBe('working')
  // Petting also wakes first.
  expect(name(send(s, t + 10, env, { type: 'shot', name: 'petted', source: 'main' }), t + 10, env)).toBe('waking')
})

test('rule 6: a glance toward another source plays look with its direction, then the animation', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'spawn', source: 'a1', name: 'review' })
  const c = current(s, t + 1100, env)
  expect(c.name).toBe('look')
  expect(c.look).toBe(SUB_DIRS[0])
  expect(current(s, t + 1000 + 600 + 1, env).name).toBe('review')
  // The second subagent gets another direction in the lower half.
  s = send(s, t + 5000, env, { type: 'spawn', source: 'a2', name: 'review' })
  expect(s.dirs.a2).toBe(SUB_DIRS[1])
  expect(Object.values(s.dirs).every(d => d >= 90 && d <= 270)).toBe(true)
  // The main conversation is 180: back to it after the subagents end.
  s = send(s, t + 9000, env, { type: 'end', source: 'a1' }, { type: 'end', source: 'a2' })
  const back = current(s, t + 9100, env)
  expect(back.name).toBe('look')
  expect(back.look).toBe(180)
  expect(lookIndex(180)).toBe(8)
  expect(lookIndex(337.5)).toBe(15)
  expect(lookIndex(0)).toBe(0)
})

test('rule 6: a pet without look cells just changes animation', () => {
  const env = envOf(ALL.filter(n => n !== 'look'))
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'spawn', source: 'a1', name: 'review' })
  expect(current(s, t + 1700, env).name).toBe('review')
})

test('rule 7: hidden after duck-in until pop-out; reactions are dropped meanwhile', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'hide' })
  expect(name(s, t, env)).toBe('duck-in')
  expect(current(s, t + 100, env).isHidden).toBe(false)
  const hiddenAt = t + playMs(env, 'duck-in') + 1
  expect(current(s, hiddenAt, env).isHidden).toBe(true)
  s = send(s, hiddenAt + 10, env, { type: 'shot', name: 'jumping', source: 'main' })
  expect(current(s, hiddenAt + 20, env).isHidden).toBe(true)
  expect(s.queued).toBeNull()
  s = send(s, hiddenAt + 1000, env, { type: 'show' })
  expect(current(s, hiddenAt + 1000, env)).toMatchObject({ name: 'pop-out', isHidden: false })
  expect(name(s, hiddenAt + 1000 + playMs(env, 'pop-out') + 1, env)).toBe('idle')
})

test('alert: a subagent waiting while the main turn works; plain waiting once main waits itself', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' }, { type: 'spawn', source: 'a1', name: 'working' })
  s = send(s, t + 2000, env, { type: 'block', source: 'a1', id: 'q' })
  const alerting = current(s, t + 2000 + 600, env)
  expect(alerting.source).toBe('a1')
  expect(['alert', 'look']).toContain(alerting.name)
  expect(current(s, t + 2000 + 1300, env).name).toBe('alert')
  // Main waits itself (P1): the subagent's wait is plain waiting, and the tie goes to the latest.
  s = send(s, t + 6000, env, { type: 'block', source: 'main', id: 'm' })
  expect(name(s, t + 7000, env)).toBe('waiting')
  // A subagent waiting with the main idle also alerts (main's loop is P6 or none).
  let idle = started(env)
  idle = send(idle, t, env, { type: 'spawn', source: 'a1', name: 'working' }, { type: 'block', source: 'a1', id: 'q' })
  expect(['alert', 'look', 'summoning', 'waking']).toContain(name(idle, t + 2000, env))
})

test('sleep: idle past sleepAfter falls asleep; any event restarts the count', () => {
  const env = envOf()
  const s = started(env)
  expect(name(s, T0 + SLEEP - 1000, env)).not.toBe('sleeping')
  expect(name(s, T0 + SLEEP + 6000, env)).toBe('sleeping')
  const poked = send(s, T0 + SLEEP, env, { type: 'shot', name: 'jumping', source: 'main' })
  expect(name(poked, T0 + SLEEP + 2 * 60_000, env)).not.toBe('sleeping')
  const never = envOf(ALL, { sleepAfterMs: 0 })
  expect(name(started(never), T0 + 10 * SLEEP, never)).not.toBe('sleeping')
  expect(nextWake(started(never), T0 + 5001, never) ?? 0).toBeGreaterThan(0)
})

test('sleep: a running loop never sleeps', () => {
  const env = envOf()
  let s = started(env)
  s = send(s, T0 + 10_000, env, { type: 'loop', source: 'main', name: 'working' })
  expect(name(s, T0 + 10 * SLEEP, env)).toBe('working')
})

test('fidget: scheduled 20-60 s after idle begins, from the injected randomness', () => {
  const low = envOf(ALL, { rng: () => 0 })
  const sLow = started(low)
  const idleAt = sLow.shown.at
  expect(sLow.fidgetAt).toBe(idleAt + 20_000)
  const high = envOf(ALL, { rng: () => 0.999999 })
  const sHigh = started(high)
  expect(sHigh.fidgetAt).toBe(sHigh.shown.at + 20_000 + 40_000)
  // Plays at its time, then idle again with the next one scheduled.
  expect(name(sLow, sLow.fidgetAt! - 1, low)).toBe('idle')
  expect(name(sLow, sLow.fidgetAt! + 1, low)).toBe('fidget')
  const after = advance(sLow, sLow.fidgetAt! + playMs(low, 'fidget') + 1, low)
  expect(after.shown.name).toBe('idle')
  expect(after.fidgetAt).toBe(after.shown.at + 20_000)
  expect(nextWake(sLow, T0 + 5001, low)).toBe(sLow.fidgetAt)
})

test('fidget: off, or undrawn, never fidgets; the state generator is deterministic when no rng is injected', () => {
  const off = envOf(ALL, { fidget: false })
  expect(started(off).fidgetAt).toBeNull()
  const undrawn = envOf(ALL.filter(n => n !== 'fidget'))
  expect(started(undrawn).fidgetAt).toBeNull()
  const own = envOf(ALL, { rng: undefined })
  const a = started(own)
  const b = started(own)
  expect(a.fidgetAt).toBe(b.fidgetAt)
  expect(a.fidgetAt! - a.shown.at).toBeGreaterThanOrEqual(20_000)
  expect(a.fidgetAt! - a.shown.at).toBeLessThanOrEqual(60_000)
})

test('a fidget is cut short when real work starts', () => {
  const env = envOf()
  let s = started(env)
  const at = s.fidgetAt! + 100
  s = advance(s, at, env)
  expect(s.shown.name).toBe('fidget')
  s = send(s, at + 50, env, { type: 'loop', source: 'main', name: 'working' })
  expect(name(s, at + 50, env)).toBe('working')
})

test('fallback resolution: a missing animation shows what its fallback draws', () => {
  const sparse = envOf(['idle', 'working', 'waving', 'jumping'])
  const c = sparse.catalog
  expect(resolveAnimation(c, 'celebrating')).toBe('jumping')
  expect(resolveAnimation(c, 'summoning')).toBe('waving')
  expect(resolveAnimation(c, 'review')).toBe('working')
  expect(resolveAnimation(c, 'eating')).toBe('working')
  expect(resolveAnimation(c, 'alert')).toBe('idle')
  expect(resolveAnimation(c, 'confused')).toBe('idle')
  expect(resolveAnimation(c, 'pop-out')).toBe('jumping')
  expect(resolveAnimation(c, 'unknown-thing')).toBe('idle')
  expect(fallbackChain(c, 'celebrating')).toEqual(['celebrating', 'jumping', 'idle'])
  // A reaction that falls back to a loop (or idle) is not played.
  expect(isPlayable(sparse, 'confused')).toBe(false)
  expect(isPlayable(sparse, 'celebrating')).toBe(true)
  let s = started(sparse)
  s = send(s, T0 + 10_000, sparse, { type: 'shot', name: 'confused', source: 'main' })
  expect(name(s, T0 + 10_000, sparse)).toBe('idle')
  // The fallback's frames time the reaction.
  expect(playMs(sparse, 'celebrating')).toBe(playMs(sparse, 'jumping'))
})

test('compaction: eating overlays the working loop while it lasts', () => {
  const env = envOf()
  let s = started(env)
  const t = T0 + 10_000
  s = send(s, t, env, { type: 'loop', source: 'main', name: 'working' })
  s = send(s, t + 1000, env, { type: 'overlay', source: 'main', name: 'eating' })
  expect(name(s, t + 1700, env)).toBe('eating')
  s = send(s, t + 3000, env, { type: 'overlay', source: 'main', name: null })
  expect(name(s, t + 3700, env)).toBe('working')
})

test('the same state and clock always give the same answer (the state is plain data)', () => {
  const env = envOf()
  let s = started(env)
  s = send(s, T0 + 10_000, env, { type: 'loop', source: 'main', name: 'working' }, { type: 'spawn', source: 'a1', name: 'review' })
  const copy = JSON.parse(JSON.stringify(s)) as PetsArbiter
  for (const dt of [0, 500, 1300, 4000, 70_000]) expect(current(copy, T0 + 10_000 + dt, env)).toEqual(current(s, T0 + 10_000 + dt, env))
})
