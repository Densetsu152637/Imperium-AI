// A tiny WebAssembly assembler for pet behaviours: enough to write the i32-only, loop-free modules
// the app's behaviour host accepts (apps/desktop/src/mods/behaviour.js) without a toolchain. It
// writes bytes; it checks nothing, since the host validates what it is given. No imports.

export type Bytes = number[]

/** Unsigned LEB128. */
export function u32(value: number): Bytes {
  const out: Bytes = []
  let n = value >>> 0
  do {
    let byte = n & 0x7f
    n = Math.floor(n / 128)
    if (n !== 0) byte |= 0x80
    out.push(byte)
  } while (n !== 0)
  return out
}

/** Signed LEB128 of a 32-bit value. */
export function s32(value: number): Bytes {
  const out: Bytes = []
  let n = value | 0
  for (;;) {
    const byte = n & 0x7f
    n >>= 7
    if ((n === 0 && (byte & 0x40) === 0) || (n === -1 && (byte & 0x40) !== 0)) {
      out.push(byte)
      return out
    }
    out.push(byte | 0x80)
  }
}

const vec = (items: Bytes[]): Bytes => [...u32(items.length), ...items.flat()]
const text = (s: string): Bytes => [...u32(s.length), ...Array.from(s, (c) => c.charCodeAt(0))]
const section = (id: number, body: Bytes): Bytes => [id, ...u32(body.length), ...body]

const constant = (n: number): Bytes => [0x41, ...s32(n)]

/** The instructions, as byte builders. Operands come first, as in a stack machine. */
export const op = {
  c: constant,
  lget: (i: number): Bytes => [0x20, ...u32(i)],
  lset: (i: number): Bytes => [0x21, ...u32(i)],
  gget: (i: number): Bytes => [0x23, ...u32(i)],
  gset: (i: number): Bytes => [0x24, ...u32(i)],
  /** i32.load of the word at byte address `addr` (a constant). */
  load: (addr: number): Bytes => [...constant(addr), 0x28, 2, 0],
  /** i32.store of `value` at byte address `addr`. */
  store: (addr: number, value: Bytes): Bytes => [...constant(addr), ...value, 0x36, 2, 0],
  call: (f: number): Bytes => [0x10, ...u32(f)],
  /** `if` with an empty type; `then` and `otherwise` are instruction sequences. */
  when: (cond: Bytes, then: Bytes, otherwise?: Bytes): Bytes => [...cond, 0x04, 0x40, ...then, ...(otherwise ? [0x05, ...otherwise] : []), 0x0b],
  ret: [0x0f] as Bytes,
  eqz: [0x45] as Bytes,
  eq: [0x46] as Bytes,
  ne: [0x47] as Bytes,
  lt: [0x48] as Bytes,
  gt: [0x4a] as Bytes,
  le: [0x4c] as Bytes,
  ge: [0x4e] as Bytes,
  add: [0x6a] as Bytes,
  sub: [0x6b] as Bytes,
  mul: [0x6c] as Bytes,
  and: [0x71] as Bytes,
  or: [0x72] as Bytes,
  xor: [0x73] as Bytes,
  shl: [0x74] as Bytes,
  shru: [0x76] as Bytes,
}

export type Func = {
  params?: number
  results?: number
  /** Locals beyond the parameters. */
  locals?: number
  /** The instructions, without the closing `end`. */
  body: Bytes
}

export type ModuleSpec = {
  /** Import `imperium.rand` (function index 0; defined functions follow); an object names another import, for tests of the host. */
  rand?: boolean | { module: string; name: string }
  funcs: Func[]
  memory?: { min: number; max?: number }
  /** Mutable i32 globals and their initial values. */
  globals?: number[]
  exports: { name: string; kind: "func" | "memory" | "global"; index: number }[]
  data?: { offset: number; bytes: number[] }[]
  /** Extra sections as written (id and content), placed in id order: for modules the host should refuse. */
  raw?: { id: number; body: Bytes }[]
}

const KIND = { func: 0, memory: 2, global: 3 } as const

/** Assembles a module; `funcs[0]` has function index 0 (or 1 when `rand` is imported). */
export function assemble(spec: ModuleSpec): Uint8Array {
  const sigs: string[] = []
  const typeOf = (params: number, results: number): number => {
    const key = `${params}:${results}`
    let i = sigs.indexOf(key)
    if (i < 0) i = sigs.push(key) - 1
    return i
  }
  const randType = spec.rand ? typeOf(0, 1) : -1
  const funcTypes = spec.funcs.map((f) => typeOf(f.params ?? 0, f.results ?? 0))
  const types = sigs.map((key) => {
    const [params = 0, results = 0] = key.split(":").map(Number)
    return [0x60, ...vec(Array.from({ length: params }, () => [0x7f])), ...vec(Array.from({ length: results }, () => [0x7f]))]
  })
  const imp = spec.rand === true ? { module: "imperium", name: "rand" } : spec.rand || null
  const sections: [number, Bytes][] = [[1, vec(types)]]
  if (imp) sections.push([2, vec([[...text(imp.module), ...text(imp.name), 0x00, ...u32(randType)]])])
  sections.push([3, vec(funcTypes.map((t) => u32(t)))])
  if (spec.memory) sections.push([5, vec([spec.memory.max === undefined ? [0x00, ...u32(spec.memory.min)] : [0x01, ...u32(spec.memory.min), ...u32(spec.memory.max)]])])
  if (spec.globals) sections.push([6, vec(spec.globals.map((v) => [0x7f, 0x01, ...constant(v), 0x0b]))])
  sections.push([7, vec(spec.exports.map((e) => [...text(e.name), KIND[e.kind], ...u32(e.index)]))])
  const bodies = spec.funcs.map((f) => {
    const body: Bytes = [...(f.locals ? vec([[...u32(f.locals), 0x7f]]) : u32(0)), ...f.body, 0x0b]
    return [...u32(body.length), ...body]
  })
  sections.push([10, vec(bodies)])
  if (spec.data) sections.push([11, vec(spec.data.map((d) => [0x00, ...constant(d.offset), 0x0b, ...u32(d.bytes.length), ...d.bytes]))])
  for (const r of spec.raw ?? []) sections.push([r.id, r.body])
  sections.sort((a, b) => a[0] - b[0])
  const out: Bytes = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
  for (const [id, body] of sections) out.push(...section(id, body))
  return Uint8Array.from(out)
}
