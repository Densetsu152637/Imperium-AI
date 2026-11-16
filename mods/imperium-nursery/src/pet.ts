// The pet package, pure parts: the v3 animation table (docs/pet-format-v3.md) and the
// validation of a pet.json. Ported from Pixel Pets (MIT; see ../NOTICE).

export type Anim = {
  name: string
  row: number
  frames: number
  /** One duration in ms per frame. */
  durations: number[]
  /** A state (true) or a one-shot reaction (false). */
  loop: boolean
  /** The `look` pose: one cell per direction, no timing. */
  pose: boolean
  /** 0 (P0) to 6 (P6). */
  priority: number
  minMs: number
  next?: string
  fallback?: string
  /** Whether this pet draws it (its strip exists and is loadable). */
  drawn: boolean
}

export type Catalog = Record<string, Anim>

export type PetDef = {
  id: string
  displayName: string
  cell: { width: number; height: number }
  stripsPath: string
  /** Standard animations with the pet's overrides, plus its custom ones; `drawn` is false everywhere. */
  catalog: Catalog
}

const rep = (ms: number, n: number): number[] => Array.from({ length: n }, () => ms)

type Row = [name: string, row: number, durations: number[], loop: boolean, priority: number, fallback?: string]

const TABLE: Row[] = [
  ['idle', 0, [280, 110, 110, 140, 140, 320], true, 6],
  ['walk-right', 1, [...rep(120, 7), 220], true, 6, 'idle'],
  ['walk-left', 2, [...rep(120, 7), 220], true, 6, 'idle'],
  ['waving', 3, [140, 140, 140, 280], false, 5, 'idle'],
  ['jumping', 4, [...rep(140, 4), 280], false, 3, 'idle'],
  ['failed', 5, [...rep(140, 7), 240], false, 2, 'idle'],
  ['waiting', 6, [...rep(150, 5), 260], true, 1, 'idle'],
  ['working', 7, [...rep(120, 5), 220], true, 4, 'idle'],
  ['review', 8, [...rep(150, 5), 280], true, 4, 'working'],
  ['sleeping', 11, rep(400, 6), true, 6, 'idle'],
  ['waking', 12, [...rep(120, 5), 200], false, 5, 'idle'],
  ['summoning', 13, [...rep(120, 5), 240], false, 4, 'waving'],
  ['celebrating', 14, [...rep(110, 7), 300], false, 3, 'jumping'],
  ['confused', 15, [...rep(150, 5), 300], false, 2, 'failed'],
  ['eating', 16, rep(140, 6), true, 4, 'working'],
  ['fidget', 17, [...rep(160, 5), 300], false, 6, 'idle'],
  ['pop-out', 18, [...rep(90, 5), 160], false, 5, 'jumping'],
  ['duck-in', 19, [...rep(90, 5), 160], false, 5, 'idle'],
  ['alert', 20, rep(120, 4), true, 1, 'waiting'],
  ['petted', 21, [...rep(120, 5), 260], false, 0, 'waving'],
]

/** The 16 look cells (rows 9 and 10): 000, 022.5, ... 337.5 degrees, clockwise from up. */
export const LOOK_CELLS = 16

/** The standard animation table of the spec, none of it marked drawn. */
export const standardCatalog = (): Catalog => {
  const c: Catalog = {}
  for (const [name, row, durations, loop, priority, fallback] of TABLE) {
    c[name] = { name, row, frames: durations.length, durations, loop, pose: false, priority, minMs: 600, fallback, drawn: false }
  }
  c.look = { name: 'look', row: 9, frames: LOOK_CELLS, durations: rep(600, LOOK_CELLS), loop: false, pose: true, priority: 6, minMs: 600, fallback: 'idle', drawn: false }
  return c
}

/** The standard table with every animation drawn: what the arbiter assumes before a pet loads. */
export const allDrawn = (): Catalog => {
  const c = standardCatalog()
  for (const a of Object.values(c)) a.drawn = true
  return c
}

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)

/** `P3`, `p3` or 3 as a priority number; undefined for anything else. */
export const parsePriority = (v: unknown): number | undefined => {
  if (typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= 6) return v
  if (typeof v === 'string') {
    const m = /^P?([0-6])$/i.exec(v.trim())
    if (m) return Number(m[1])
  }
  return undefined
}

const posInt = (v: unknown, lo: number, hi: number): number | undefined =>
  typeof v === 'number' && Number.isInteger(v) && v >= lo && v <= hi ? v : undefined

const durationsOf = (v: unknown, frames: number): number[] | undefined => {
  const ok = (n: unknown): n is number => typeof n === 'number' && Number.isFinite(n) && n >= 1 && n <= 60000
  if (ok(v)) return rep(v, frames)
  if (Array.isArray(v) && v.length === frames && v.every(ok)) return [...v]
  return undefined
}

export type ParseResult = { ok: true; pet: PetDef } | { ok: false; error: string }

/** Validates pet.json text for the folder `folderId`. Unknown fields are ignored. */
export function parsePetJson(text: string, folderId: string): ParseResult {
  let raw: unknown
  try {
    raw = JSON.parse(text)
  } catch {
    return { ok: false, error: 'pet.json is not valid JSON' }
  }
  if (!isObj(raw)) return { ok: false, error: 'pet.json is not an object' }
  const id = raw.id
  if (typeof id !== 'string' || !/^[a-z0-9-]+$/.test(id)) return { ok: false, error: 'id must be lowercase letters, digits and hyphens' }
  if (id !== folderId) return { ok: false, error: `id "${id}" does not equal the folder name "${folderId}"` }
  if (raw.spriteVersionNumber !== 3) return { ok: false, error: 'spriteVersionNumber must be 3 (run tools/package_pet.py on a Codex pet)' }
  const cell = isObj(raw.cell) ? raw.cell : {}
  const width = posInt(cell.width, 16, 128)
  const height = posInt(cell.height, 16, 128)
  if (width === undefined || height === undefined) return { ok: false, error: 'cell.width and cell.height must be integers from 16 to 128' }
  const stripsPath = typeof raw.stripsPath === 'string' && raw.stripsPath.trim() && !/(^|[\\/])\.\.([\\/]|$)/.test(raw.stripsPath) ? raw.stripsPath.trim() : 'strips'
  const catalog = standardCatalog()
  const overrides = isObj(raw.animations) ? raw.animations : {}
  for (const [name, o] of Object.entries(overrides)) {
    if (!/^[a-z0-9-]+$/.test(name) || !isObj(o)) continue
    const std = catalog[name]
    if (std) {
      const frames = posInt(o.frames, 1, 16) ?? std.frames
      const durations = durationsOf(o.durations, frames) ?? (frames === std.frames ? std.durations : rep(std.durations[0] ?? 120, frames))
      catalog[name] = {
        ...std,
        row: posInt(o.row, 0, 1000) ?? std.row,
        frames,
        durations,
        loop: typeof o.loop === 'boolean' ? o.loop : std.loop,
        priority: parsePriority(o.priority) ?? std.priority,
        minMs: typeof o.minMs === 'number' && o.minMs >= 0 && o.minMs <= 60000 ? o.minMs : std.minMs,
        next: typeof o.next === 'string' ? o.next : std.next,
        fallback: typeof o.fallback === 'string' ? o.fallback : std.fallback,
      }
      continue
    }
    // A custom animation must give row (22 or more), frames, durations and fallback.
    const frames = posInt(o.frames, 1, 16)
    const row = posInt(o.row, 22, 1000)
    const durations = frames === undefined ? undefined : durationsOf(o.durations, frames)
    if (row === undefined || frames === undefined || durations === undefined || typeof o.fallback !== 'string') continue
    catalog[name] = {
      name,
      row,
      frames,
      durations,
      loop: o.loop === true,
      pose: false,
      priority: parsePriority(o.priority) ?? 4,
      minMs: typeof o.minMs === 'number' && o.minMs >= 0 && o.minMs <= 60000 ? o.minMs : 600,
      next: typeof o.next === 'string' ? o.next : undefined,
      fallback: o.fallback,
      drawn: false,
    }
  }
  const displayName = typeof raw.displayName === 'string' && raw.displayName.trim() ? raw.displayName.trim().slice(0, 80) : id
  return { ok: true, pet: { id, displayName, cell: { width, height }, stripsPath, catalog } }
}

/** The catalog with `drawn` set for each animation whose strip is in `names`. */
export function withDrawn(catalog: Catalog, names: ReadonlySet<string>): Catalog {
  const out: Catalog = {}
  for (const [k, a] of Object.entries(catalog)) out[k] = { ...a, drawn: names.has(k) }
  return out
}

/**
 * The animation actually drawn for `name`: follows `fallback` until a drawn animation.
 * `idle` is the end of every chain (the spec requires a pet to draw it).
 */
export function resolveAnimation(catalog: Catalog, name: string): string {
  let cur = name
  const seen = new Set<string>()
  while (!seen.has(cur)) {
    seen.add(cur)
    const a = catalog[cur]
    if (a?.drawn) return cur
    const next = a?.fallback
    if (!next || !catalog[next]) break
    cur = next
  }
  return 'idle'
}

/** The fallback chain from `name` on, each animation once, ending in `idle`. */
export function fallbackChain(catalog: Catalog, name: string): string[] {
  const out: string[] = []
  let cur: string | undefined = name
  while (cur && !out.includes(cur) && catalog[cur]) {
    out.push(cur)
    cur = catalog[cur]?.fallback
  }
  if (!out.includes('idle')) out.push('idle')
  return out
}
