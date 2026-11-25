// WebAssembly pet behaviours for UI mods: validation, limits and a tiny host API.
//
// A behaviour is a small WebAssembly module a mod ships to steer its pets. It runs inside the
// mod's own sandboxed frame (no same-origin, no network), is given exactly one import
// (`imperium.rand`, a seeded generator), and talks to the host only through a few words of its
// own memory. Browsers' WebAssembly has no fuel metering and cannot be interrupted, so instead of
// metering this file proves a bound before the module is ever compiled: the module may contain no
// `loop`, no indirect call and no recursion (so every tick executes a bounded number of
// instructions, counted exactly below), no floating point or 64-bit arithmetic (so results are
// deterministic), no `memory.grow` (so memory is fixed at its declared, capped size), no table, no
// start function and no import beyond `imperium.rand`. A module that traps, takes too long or
// answers with an invalid value is stopped for good and reported, never the app.
//
// This file is plain JavaScript on purpose: the app embeds it, as text, into every mod frame (the
// frame runs it where the module runs), and `behaviour.test.ts` runs the very same file in Node.
// Keep it ASCII and free of imports.

/** Hard limits. The module's own declarations are checked against these before it is compiled. */
export const LIMITS = Object.freeze({
  /** Largest module, in bytes (Chromium refuses to compile more than 4 KiB synchronously on the main thread). */
  maxBytes: 4096,
  maxFunctions: 64,
  /** Locals per function, parameters included. */
  maxLocals: 64,
  maxGlobals: 32,
  /** Memory pages (64 KiB each), also the most it may declare as its maximum. */
  maxPages: 2,
  /** The most instructions one tick may execute, proved statically (call chains included). */
  maxFuel: 100000,
  /** A tick that took longer than this (milliseconds, measured by the host) stops the module. */
  maxTickMs: 50,
  /** Slow ticks (over maxTickMs) a behaviour is allowed before it is stopped: one hiccup is not a fault. */
  maxSlowTicks: 3,
  /** Live behaviours per host (one mod frame). */
  maxBehaviours: 32,
  /** Speed a behaviour may ask for, in pixels per second, each axis. */
  maxSpeed: 60,
});

/** Animation indices the host reports as the pet's current one (input word 3). */
export const ANIMATIONS = Object.freeze([
  "idle",
  "walk-right",
  "walk-left",
  "waving",
  "jumping",
  "failed",
  "waiting",
  "working",
  "review",
  "sleeping",
  "waking",
  "summoning",
  "celebrating",
  "confused",
  "eating",
  "fidget",
  "pop-out",
  "duck-in",
  "alert",
  "petted",
  "look",
]);

/** The one-shots a behaviour may ask for: output word 16 is 0 (none) or a 1-based index into this. */
export const SHOTS = Object.freeze(["waving", "jumping", "celebrating", "fidget"]);

/** The memory layout (32-bit little-endian words): the host writes the input, the module the output. */
export const ABI = Object.freeze({
  version: 1,
  /** Input words 0-11: abi version, dt ms, tick count, animation index, flags, quiet ms, x, y, minX, maxX, minY, maxY. */
  inputWords: 12,
  /** Flag bits in input word 4. */
  flags: Object.freeze({ free: 1, busy: 2 }),
  /** Output words 16-18: shot, vx, vy. The host clears words 0-31 before every tick. */
  outputStart: 16,
  outputWords: 3,
  clearedWords: 32,
  /** The memory the module must have at least, so both blocks fit. */
  minBytes: 128,
});

const I32 = 0x7f;
const GOOD_NAME = /^[\x21-\x7e]{1,32}$/;

class Reject extends Error {}

const reject = (message) => {
  throw new Reject(message);
};

class Reader {
  constructor(bytes, start, end) {
    this.bytes = bytes;
    this.pos = start;
    this.end = end;
  }

  left() {
    return this.end - this.pos;
  }

  u8() {
    if (this.pos >= this.end) reject("the module ends in the middle of a section");
    return this.bytes[this.pos++];
  }

  /** An unsigned LEB128 of at most 32 bits. */
  u32() {
    let result = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const byte = this.u8();
      result += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) {
        if (result > 0xffffffff) reject("a number is larger than 32 bits");
        return result;
      }
    }
    return reject("a number is longer than 5 bytes");
  }

  /** A signed LEB128 of at most 32 bits. */
  s32() {
    let result = 0;
    for (let shift = 0; shift < 35; shift += 7) {
      const byte = this.u8();
      result += (byte & 0x7f) * 2 ** shift;
      if ((byte & 0x80) === 0) {
        if (byte & 0x40) result -= 2 ** (shift + 7);
        if (result < -(2 ** 31) || result > 2 ** 31 - 1) reject("a constant does not fit in 32 bits");
        return result;
      }
    }
    return reject("a number is longer than 5 bytes");
  }

  name() {
    const length = this.u32();
    if (length > this.left()) reject("a name runs past the end of its section");
    const text = this.bytes.subarray(this.pos, this.pos + length);
    this.pos += length;
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(text);
    } catch {
      return reject("a name is not valid UTF-8");
    }
  }

  /** A count that the remaining bytes could possibly hold (each item takes at least one byte). */
  count() {
    const n = this.u32();
    if (n > this.left()) reject("a count is larger than its section");
    return n;
  }
}

/** The plain instructions a behaviour may use, by opcode, with how many immediates each takes. */
const SIMPLE = new Set([
  0x00, 0x01, 0x0f, 0x1a, 0x1b, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x4b, 0x4c, 0x4d, 0x4e, 0x4f, 0x67, 0x68,
  0x69, 0x6a, 0x6b, 0x6c, 0x6d, 0x6e, 0x6f, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78,
]);
/** Memory instructions allowed, with the largest alignment exponent each may declare (its natural one). */
const MEMORY_OPS = new Map([[0x28, 2], [0x2c, 0], [0x2d, 0], [0x2e, 1], [0x2f, 1], [0x36, 2], [0x3a, 0], [0x3b, 1]]);

/** Why an opcode is refused, for the ones a mod author is likely to reach for. */
function refusal(op) {
  if (op === 0x03) return "loops are not allowed (a tick must finish in a provable number of steps)";
  if (op === 0x11) return "call_indirect is not allowed";
  if (op === 0x40) return "memory.grow is not allowed (memory is fixed)";
  if ((op >= 0x29 && op <= 0x35 && op !== 0x2c && op !== 0x2d && op !== 0x2e && op !== 0x2f) || (op >= 0x37 && op <= 0x3e && op !== 0x3a && op !== 0x3b)) return "only 32-bit integer memory access is allowed";
  if (op >= 0x43 && op <= 0x44) return "floating point is not allowed (results must be deterministic)";
  if (op === 0x42 || (op >= 0x50 && op <= 0x66) || (op >= 0x79 && op <= 0xc4)) return "only 32-bit integer arithmetic is allowed";
  if (op === 0xfc) return "bulk memory and saturating instructions are not allowed";
  if (op === 0xfd) return "SIMD is not allowed";
  return `instruction 0x${op.toString(16)} is not allowed`;
}

/** Reads one function body; returns its instruction count and the functions it calls. */
function readBody(r, functionCount, paramCount) {
  const groups = r.count();
  let locals = paramCount;
  for (let i = 0; i < groups; i++) {
    const n = r.u32();
    if (r.u8() !== I32) reject("locals must be i32");
    locals += n;
    if (locals > LIMITS.maxLocals) reject(`a function has more than ${LIMITS.maxLocals} locals`);
  }
  const calls = [];
  const blocks = []; // "block" or "if" (with whether an else was seen)
  let count = 0;
  let finished = false;
  while (r.left() > 0 && !finished) {
    const op = r.u8();
    count++;
    if (SIMPLE.has(op)) continue;
    switch (op) {
      case 0x02:
      case 0x04: {
        const type = r.u8();
        if (type !== 0x40 && type !== I32) reject("a block may only be empty or return one i32");
        blocks.push(op === 0x04 ? { kind: "if", other: false } : { kind: "block" });
        break;
      }
      case 0x05: {
        const top = blocks[blocks.length - 1];
        if (!top || top.kind !== "if" || top.other) reject("else without a matching if");
        top.other = true;
        break;
      }
      case 0x0b:
        if (blocks.length === 0) finished = true;
        else blocks.pop();
        break;
      case 0x0c:
      case 0x0d: {
        const depth = r.u32();
        if (depth > blocks.length) reject("a branch leaves its function");
        break;
      }
      case 0x0e: {
        const n = r.count();
        for (let i = 0; i <= n; i++) if (r.u32() > blocks.length) reject("a branch leaves its function");
        break;
      }
      case 0x10: {
        const callee = r.u32();
        if (callee >= functionCount) reject("a call names a function that does not exist");
        calls.push(callee);
        break;
      }
      case 0x20:
      case 0x21:
      case 0x22:
        if (r.u32() >= locals) reject("a local index is out of range");
        break;
      case 0x23:
      case 0x24:
        r.u32(); // the engine checks the global index
        break;
      case 0x3f:
        if (r.u8() !== 0x00) reject("memory.size must name memory 0");
        break;
      case 0x41:
        r.s32();
        break;
      default:
        if (MEMORY_OPS.has(op)) {
          // Bit 6 of the alignment field would mean an explicit memory index follows (multi-memory),
          // which this parser would misread; any value above the natural alignment is refused, flags included.
          if (r.u32() > MEMORY_OPS.get(op)) reject("a memory access declares more than its natural alignment");
          r.u32(); // offset
          break;
        }
        reject(refusal(op));
    }
  }
  if (!finished) reject("a function body does not end");
  if (r.left() !== 0) reject("a function body has bytes after its end");
  return { count, calls };
}

/**
 * Checks a module against the rules above without compiling it. Returns `{ ok: true, fuel, pages }`
 * (the proved bound on instructions per tick and the memory size) or `{ ok: false, error }`.
 */
export function validateModule(input) {
  try {
    if (!(input instanceof Uint8Array)) return { ok: false, error: "a behaviour is a Uint8Array of WebAssembly" };
    if (input.length > LIMITS.maxBytes) return { ok: false, error: `a behaviour may be at most ${LIMITS.maxBytes} bytes` };
    return check(input);
  } catch (error) {
    if (error instanceof Reject) return { ok: false, error: error.message };
    return { ok: false, error: "the module could not be read" };
  }
}

function check(bytes) {
  const head = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
  if (bytes.length < 8 || head.some((byte, i) => bytes[i] !== byte)) reject("not a WebAssembly module (version 1)");
  const r = new Reader(bytes, 8, bytes.length);
  const types = [];
  const imports = []; // type index per imported function
  const funcs = []; // type index per defined function
  let memory = null;
  let globals = 0;
  const exportsByName = new Map();
  let bodies = null;
  let dataEnd = 0;
  let last = 0;
  while (r.left() > 0) {
    const id = r.u8();
    const size = r.u32();
    if (size > r.left()) reject("a section runs past the end of the module");
    const s = new Reader(bytes, r.pos, r.pos + size);
    r.pos += size;
    if (id === 0) continue; // custom sections (names, producers) carry nothing the host reads
    if (id <= last) reject("sections are out of order or repeated");
    last = id;
    switch (id) {
      case 1: {
        const n = s.count();
        for (let i = 0; i < n; i++) {
          if (s.u8() !== 0x60) reject("only function types are allowed");
          const params = s.count();
          for (let p = 0; p < params; p++) if (s.u8() !== I32) reject("parameters and results must be i32");
          const results = s.count();
          if (results > 1) reject("a function returns at most one value");
          for (let p = 0; p < results; p++) if (s.u8() !== I32) reject("parameters and results must be i32");
          types.push({ params, results });
        }
        break;
      }
      case 2: {
        const n = s.count();
        for (let i = 0; i < n; i++) {
          const module = s.name();
          const field = s.name();
          const kind = s.u8();
          if (module !== "imperium" || field !== "rand") reject(`the only import allowed is imperium.rand, not ${module}.${field}`);
          if (kind !== 0x00) reject("imperium.rand is a function");
          const type = s.u32();
          const t = types[type];
          if (!t || t.params !== 0 || t.results !== 1) reject("imperium.rand takes nothing and returns an i32");
          imports.push(type);
        }
        if (imports.length > 1) reject("imperium.rand may be imported once");
        break;
      }
      case 3: {
        const n = s.count();
        if (n > LIMITS.maxFunctions) reject(`a behaviour may define at most ${LIMITS.maxFunctions} functions`);
        for (let i = 0; i < n; i++) {
          const type = s.u32();
          if (!types[type]) reject("a function names a type that does not exist");
          funcs.push(type);
        }
        break;
      }
      case 4:
        reject("tables are not allowed");
        break;
      case 5: {
        if (s.u32() !== 1) reject("a behaviour has exactly one memory");
        const flag = s.u8();
        if (flag !== 1) reject("the memory must declare a maximum");
        const min = s.u32();
        const max = s.u32();
        if (max > LIMITS.maxPages || min > max) reject(`memory may be at most ${LIMITS.maxPages} pages`);
        if (min * 65536 < ABI.minBytes) reject("the memory is too small for the input and output words");
        memory = { min, max };
        break;
      }
      case 6: {
        const n = s.count();
        if (n > LIMITS.maxGlobals) reject(`a behaviour may have at most ${LIMITS.maxGlobals} globals`);
        for (let i = 0; i < n; i++) {
          if (s.u8() !== I32) reject("globals must be i32");
          const mutability = s.u8();
          if (mutability > 1) reject("a global is mutable or not");
          if (s.u8() !== 0x41) reject("a global starts from an i32 constant");
          s.s32();
          if (s.u8() !== 0x0b) reject("a global's initialiser is one constant");
          globals++;
        }
        break;
      }
      case 7: {
        const n = s.count();
        for (let i = 0; i < n; i++) {
          const name = s.name();
          const kind = s.u8();
          const index = s.u32();
          if (!GOOD_NAME.test(name)) reject("an export name is 1 to 32 printable characters");
          if (kind > 3 || kind === 1) reject("only functions, memory and globals may be exported");
          if (exportsByName.has(name)) reject(`${name} is exported twice`);
          exportsByName.set(name, { kind, index });
        }
        break;
      }
      case 8:
        reject("a start function is not allowed (nothing runs before the first tick)");
        break;
      case 9:
        reject("element segments are not allowed");
        break;
      case 10: {
        const n = s.count();
        if (n !== funcs.length) reject("the code section does not match the function section");
        const total = imports.length + funcs.length;
        bodies = [];
        for (let i = 0; i < n; i++) {
          const size = s.u32();
          if (size > s.left()) reject("a function body runs past its section");
          const body = new Reader(bytes, s.pos, s.pos + size);
          s.pos += size;
          bodies.push(readBody(body, total, types[funcs[i]].params));
        }
        break;
      }
      case 11: {
        const n = s.count();
        for (let i = 0; i < n; i++) {
          if (s.u32() !== 0) reject("data segments must be active and in memory 0");
          if (s.u8() !== 0x41) reject("a data segment's offset is an i32 constant");
          const offset = s.s32();
          if (s.u8() !== 0x0b) reject("a data segment's offset is one constant");
          const length = s.u32();
          if (length > s.left()) reject("a data segment runs past its section");
          s.pos += length;
          if (offset < 0) reject("a data segment's offset is negative");
          dataEnd = Math.max(dataEnd, offset + length);
        }
        break;
      }
      default:
        reject("a section this host does not support");
    }
    if (s.left() !== 0) reject("a section has bytes after its content");
  }
  if (!memory) reject("the module has no memory");
  if (dataEnd > memory.min * 65536) reject("a data segment does not fit in the memory");
  const mem = exportsByName.get("memory");
  if (!mem || mem.kind !== 2 || mem.index !== 0) reject("the module must export its memory as \"memory\"");
  const tick = exportsByName.get("tick");
  if (!tick || tick.kind !== 0) reject("the module must export a function \"tick\"");
  const offset = tick.index - imports.length;
  if (offset < 0 || offset >= funcs.length) reject("tick must be a function the module defines");
  const type = types[funcs[offset]];
  if (type.params !== 0 || type.results !== 0) reject("tick takes nothing and returns nothing");
  for (const [name, entry] of exportsByName) {
    if (entry.kind === 0 && entry.index >= imports.length + funcs.length) reject(`${name} names a function that does not exist`);
    if (entry.kind === 3 && entry.index >= globals) reject(`${name} names a global that does not exist`);
  }
  if (!bodies && funcs.length > 0) reject("the module has functions but no code");
  return { ok: true, fuel: fuelBound(bodies ?? [], imports.length), pages: memory.min };
}

/** The most instructions one call of `tick` can run: its own count plus its callees', each call site counted. */
function fuelBound(bodies, imported) {
  const state = new Array(bodies.length).fill(0); // 0 new, 1 on the stack, 2 done
  const cost = new Array(bodies.length).fill(0);
  const visit = (index) => {
    if (state[index] === 2) return cost[index];
    if (state[index] === 1) reject("recursion is not allowed");
    state[index] = 1;
    let total = bodies[index].count;
    for (const callee of bodies[index].calls) {
      if (callee >= imported) total += visit(callee - imported);
      if (total > LIMITS.maxFuel) reject(`a tick may run at most ${LIMITS.maxFuel} instructions; this module can run more`);
    }
    state[index] = 2;
    cost[index] = total;
    return total;
  };
  let worst = 0;
  for (let i = 0; i < bodies.length; i++) worst = Math.max(worst, visit(i));
  return worst;
}

/** A seeded 32-bit generator (mulberry32): the same seed gives the same sequence everywhere. */
export function seededRandom(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return (t ^ (t >>> 14)) >>> 0;
  };
}

const int = (value, lo, hi) => {
  const n = Math.trunc(Number(value));
  return Number.isFinite(n) ? Math.min(hi, Math.max(lo, n)) : 0;
};

/**
 * Compiles and starts one behaviour. Returns `{ ok: true, behaviour }` or `{ ok: false, error }`.
 * `options.seed` seeds `imperium.rand`; `options.clock` (milliseconds, default `performance.now`)
 * measures ticks; `options.onFault(message)` hears the one fault that stops the behaviour.
 */
export function loadBehaviour(input, options = {}) {
  const checked = validateModule(input);
  if (!checked.ok) return checked;
  const bytes = new Uint8Array(input); // the bytes checked are the bytes compiled
  let instance;
  try {
    const rng = seededRandom(options.seed ?? 0);
    instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), { imperium: { rand: () => rng() | 0 } });
  } catch (error) {
    return { ok: false, error: `the engine refused the module: ${error instanceof Error ? error.message : String(error)}` };
  }
  const { memory, tick } = instance.exports;
  if (!(memory instanceof WebAssembly.Memory) || typeof tick !== "function") return { ok: false, error: "the module's exports are not what it declared" };
  const clock = options.clock ?? (() => globalThis.performance?.now() ?? Date.now());
  let fault = null;
  let count = 0;
  let slow = 0;
  const stop = (message) => {
    fault = message;
    try {
      options.onFault?.(message);
    } catch {
      // A listener's failure is not the behaviour's.
    }
    return { ok: false, error: message };
  };
  const behaviour = {
    fuel: checked.fuel,
    get stopped() {
      return fault !== null;
    },
    get fault() {
      return fault;
    },
    ticks() {
      return count;
    },
    /**
     * Runs one tick. `state`: `dt` (ms), `anim` (a name), `free`, `busy`, `quiet` (ms), `x`, `y` and the
     * room's `bounds` `{ minX, maxX, minY, maxY }`. Returns `{ ok: true, shot, vx, vy }` (`shot` a name or
     * null, velocities in px/s), or `{ ok: false, error }` once stopped.
     */
    tick(state) {
      if (fault !== null) return { ok: false, error: `stopped: ${fault}` };
      const words = new Int32Array(memory.buffer, 0, ABI.clearedWords);
      words.fill(0);
      const b = state.bounds ?? {};
      const flags = (state.free ? ABI.flags.free : 0) | (state.busy ? ABI.flags.busy : 0);
      words[0] = ABI.version;
      words[1] = int(state.dt, 0, 1000);
      words[2] = count | 0;
      words[3] = Math.max(0, ANIMATIONS.indexOf(state.anim));
      words[4] = flags;
      words[5] = int(state.quiet, 0, 600000);
      words[6] = int(state.x, -100000, 100000);
      words[7] = int(state.y, -100000, 100000);
      words[8] = int(b.minX, -100000, 100000);
      words[9] = int(b.maxX, -100000, 100000);
      words[10] = int(b.minY, -100000, 100000);
      words[11] = int(b.maxY, -100000, 100000);
      count = (count + 1) >>> 0;
      const started = clock();
      try {
        tick();
      } catch (error) {
        return stop(`trapped: ${error instanceof Error ? error.message : String(error)}`);
      }
      const took = clock() - started;
      if (!(took <= LIMITS.maxTickMs) && ++slow >= LIMITS.maxSlowTicks) return stop(`${slow} ticks took over ${LIMITS.maxTickMs} ms (the last ${Math.round(took)} ms)`);
      const out = new Int32Array(memory.buffer, 0, ABI.clearedWords);
      const shot = out[ABI.outputStart];
      if (shot < 0 || shot > SHOTS.length) return stop(`asked for a reaction that does not exist (${shot})`);
      const clamp = (v) => Math.min(LIMITS.maxSpeed, Math.max(-LIMITS.maxSpeed, v));
      return { ok: true, shot: shot === 0 ? null : SHOTS[shot - 1], vx: clamp(out[ABI.outputStart + 1]), vy: clamp(out[ABI.outputStart + 2]) };
    },
  };
  return { ok: true, behaviour };
}

/**
 * The host one mod frame gets: it counts live behaviours, names them in reports and tells the app
 * (`report({ name, phase, reason })`) when one is refused or stopped.
 */
export function createHost(options = {}) {
  const report = (info) => {
    try {
      options.report?.(info);
    } catch {
      // Reporting must never break the caller.
    }
  };
  const live = new Set();
  return {
    limits: LIMITS,
    animations: ANIMATIONS,
    shots: SHOTS,
    abi: ABI,
    load(name, bytes, loadOptions = {}) {
      const label = String(name).slice(0, 64);
      if (live.size >= LIMITS.maxBehaviours) {
        const error = `at most ${LIMITS.maxBehaviours} behaviours may run at once`;
        // Not a fault of the module: nothing is reported, the caller may try again once a slot is free.
        return { ok: false, error, busy: true };
      }
      const loaded = loadBehaviour(bytes, {
        seed: loadOptions.seed,
        clock: options.clock,
        onFault: (reason) => report({ name: label, phase: "tick", reason }),
      });
      if (!loaded.ok) {
        report({ name: label, phase: "load", reason: loaded.error });
        return loaded;
      }
      const token = {};
      live.add(token);
      const inner = loaded.behaviour;
      return {
        ok: true,
        behaviour: {
          get stopped() {
            return inner.stopped;
          },
          get fault() {
            return inner.fault;
          },
          fuel: inner.fuel,
          tick: (state) => inner.tick(state),
          close() {
            live.delete(token);
          },
        },
      };
    },
  };
}
