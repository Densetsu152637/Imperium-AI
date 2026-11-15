// WebAssembly pet behaviours (src/mods/behaviour.js): what a module may contain, the proved
// instruction bound, determinism, the memory contract, and what happens to a misbehaving module.
// The test modules are assembled with the nursery mod's tiny assembler, so no toolchain is needed.
// Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import { ABI, ANIMATIONS, LIMITS, SHOTS, createHost, loadBehaviour, seededRandom, validateModule, type TickState } from "../src/mods/behaviour.js";
import { assemble, op, type Bytes, type ModuleSpec } from "../../../mods/imperium-nursery/src/behaviour/asm.ts";
import { wanderModule } from "../../../mods/imperium-nursery/src/behaviour/wander.ts";

const { c, load, store, gget, gset, call } = op;
const [X, VX, SHOT, VY] = [24, 68, 64, 72];

const exportsOf = (tick: number): ModuleSpec["exports"] => [
  { name: "memory", kind: "memory", index: 0 },
  { name: "tick", kind: "func", index: tick },
];

/** A module whose `tick` runs `body`. */
const ticking = (body: Bytes, extra: Partial<ModuleSpec> = {}): Uint8Array =>
  assemble({ funcs: [{ body }], memory: { min: 1, max: 1 }, exports: exportsOf(0), ...extra });

const state = (extra: Partial<TickState> = {}): TickState => ({
  dt: 250,
  anim: "idle",
  free: true,
  busy: false,
  quiet: 0,
  x: 100,
  y: 50,
  bounds: { minX: 10, maxX: 300, minY: 20, maxY: 200 },
  ...extra,
});

const refused = (bytes: Uint8Array, pattern: RegExp): void => {
  const checked = validateModule(bytes);
  assert.equal(checked.ok, false);
  if (!checked.ok) assert.match(checked.error, pattern);
};

test("a small straight-line module is accepted and its instruction bound is exact", () => {
  const checked = validateModule(ticking([0x01, 0x01]));
  assert.deepEqual(checked, { ok: true, fuel: 3, pages: 1 }); // nop, nop, end
});

test("the bundled wander behaviour is accepted, well inside every limit", () => {
  const bytes = wanderModule();
  const checked = validateModule(bytes);
  assert.equal(checked.ok, true);
  if (checked.ok) assert.ok(checked.fuel > 0 && checked.fuel < 400, `fuel ${checked.fuel}`);
  assert.ok(bytes.length < LIMITS.maxBytes / 4, `${bytes.length} bytes`);
  assert.deepEqual([...bytes], [...wanderModule()], "assembling it again gives the same bytes");
});

test("loops, indirect calls and memory.grow are refused", () => {
  refused(ticking([0x03, 0x40, 0x0b]), /loops are not allowed/);
  refused(ticking([0x41, 0x00, 0x11, 0x00, 0x00]), /call_indirect/);
  refused(ticking([...c(1), 0x40, 0x00, 0x1a]), /memory\.grow/);
});

test("floating point, 64-bit values and SIMD are refused", () => {
  refused(ticking([0x43, 0, 0, 0, 0, 0x1a]), /floating point/);
  refused(ticking([0x42, 0x00, 0x1a]), /32-bit/);
  refused(ticking([...c(0), 0x29, 3, 0, 0x1a]), /32-bit/); // i64.load
  refused(ticking([0xfd, 0x00]), /SIMD/);
  refused(ticking([0xfc, 0x00]), /bulk memory/);
});

test("recursion, direct or through another function, is refused", () => {
  const direct = assemble({ funcs: [{ body: [...call(0)] }], memory: { min: 1, max: 1 }, exports: exportsOf(0) });
  refused(direct, /recursion/);
  const mutual = assemble({ funcs: [{ body: [...call(1)] }, { body: [...call(0)] }], memory: { min: 1, max: 1 }, exports: exportsOf(0) });
  refused(mutual, /recursion/);
});

test("a tick that could run too many instructions is refused by the proved bound", () => {
  // Each function calls the next one twice: the bound doubles with every level.
  const chain = (levels: number): Uint8Array =>
    assemble({
      funcs: Array.from({ length: levels }, (_, i) => ({ body: i + 1 < levels ? [...call(i + 1), ...call(i + 1)] : [0x01] })),
      memory: { min: 1, max: 1 },
      exports: exportsOf(0),
    });
  assert.equal(validateModule(chain(6)).ok, true);
  refused(chain(20), /at most 100000 instructions/);
});

test("a module may import only imperium.rand", () => {
  refused(ticking([], { rand: { module: "env", name: "rand" } }), /only import allowed is imperium\.rand/);
  refused(ticking([], { rand: { module: "imperium", name: "now" } }), /only import allowed is imperium\.rand/);
  assert.equal(validateModule(ticking([], { rand: true, exports: exportsOf(1) })).ok, true);
});

test("tables, start functions and element segments are refused", () => {
  refused(ticking([], { raw: [{ id: 4, body: [1, 0x70, 0, 1] }] }), /tables/);
  refused(ticking([], { raw: [{ id: 8, body: [0] }] }), /start function/);
  refused(ticking([], { raw: [{ id: 9, body: [0] }] }), /element segments/);
});

test("memory must be declared with a small maximum, and the exports must be right", () => {
  refused(assemble({ funcs: [{ body: [] }], memory: { min: 1 }, exports: exportsOf(0) }), /declare a maximum/);
  refused(assemble({ funcs: [{ body: [] }], memory: { min: 1, max: LIMITS.maxPages + 1 }, exports: exportsOf(0) }), /at most 2 pages/);
  refused(assemble({ funcs: [{ body: [] }], exports: exportsOf(0) }), /no memory/);
  refused(assemble({ funcs: [{ body: [] }], memory: { min: 1, max: 1 }, exports: [{ name: "tick", kind: "func", index: 0 }] }), /export its memory/);
  refused(assemble({ funcs: [{ body: [] }], memory: { min: 1, max: 1 }, exports: [{ name: "memory", kind: "memory", index: 0 }] }), /export a function "tick"/);
  refused(assemble({ funcs: [{ params: 1, body: [] }], memory: { min: 1, max: 1 }, exports: exportsOf(0) }), /tick takes nothing/);
});

test("a data segment must fit in the memory, and a module has a size limit", () => {
  refused(ticking([], { data: [{ offset: 65530, bytes: [1, 2, 3, 4, 5, 6, 7, 8] }] }), /does not fit/);
  assert.equal(validateModule(ticking([], { data: [{ offset: 1000, bytes: [1, 2, 3] }] })).ok, true);
  refused(ticking([], { data: [{ offset: 0, bytes: new Array(LIMITS.maxBytes).fill(1) }] }), /at most 4096 bytes/);
});

test("garbage is refused without throwing", () => {
  for (const bytes of [new Uint8Array(0), new Uint8Array([1, 2, 3]), new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 1, 99]), wanderModule().slice(0, 40)]) {
    assert.equal(validateModule(bytes).ok, false);
    assert.equal(loadBehaviour(bytes).ok, false);
  }
  assert.equal(validateModule("not bytes" as unknown as Uint8Array).ok, false);
});

test("the host writes the input words and a module's output comes back, clamped", () => {
  // vx = x (input word 6), shot = animation index of the pet (word 3), vy = maxX (word 9).
  const echo = ticking([...store(VX, load(X)), ...store(SHOT, load(12)), ...store(VY, load(36))]);
  const out = loadBehaviour(echo);
  assert.ok(out.ok);
  const result = out.behaviour.tick(state({ x: 40, anim: "waving", bounds: { minX: 0, maxX: 5000, minY: 0, maxY: 1 } }));
  // The shot word echoes the animation index (3: waving), which is also shot 3 = "celebrating".
  assert.deepEqual(result, { ok: true, shot: SHOTS[ANIMATIONS.indexOf("waving") - 1], vx: 40, vy: LIMITS.maxSpeed });
  const negative = out.behaviour.tick(state({ x: -500 }));
  assert.ok(negative.ok);
  assert.equal(negative.ok && negative.vx, -LIMITS.maxSpeed);
});

test("flags, time and quiet time reach the module, and the output area is cleared every tick", () => {
  const body = [...store(VX, load(16)), ...store(VY, load(20))];
  const run = loadBehaviour(ticking(body));
  assert.ok(run.ok);
  const first = run.behaviour.tick(state({ free: true, busy: true, quiet: 7 }));
  assert.deepEqual(first, { ok: true, shot: null, vx: ABI.flags.free | ABI.flags.busy, vy: 7 });
  // A module that asks for a reaction on its first tick only: the host clears the output area, so it is not repeated.
  const once = loadBehaviour(ticking(op.when([...load(8), ...op.eqz], store(SHOT, c(1)))));
  assert.ok(once.ok);
  assert.deepEqual([once.behaviour.tick(state()), once.behaviour.tick(state())].map((r) => r.ok && r.shot), ["waving", null]);
});

test("the same seed gives the same behaviour, a different seed another", () => {
  const trace = (seed: number): string => {
    const run = loadBehaviour(wanderModule(), { seed });
    assert.ok(run.ok);
    let [x, y] = [100, 100];
    const out: string[] = [];
    for (let i = 0; i < 80; i++) {
      const r = run.behaviour.tick(state({ x, y, dt: 250 }));
      assert.ok(r.ok);
      x += r.vx / 4;
      y += r.vy / 4;
      out.push(`${r.vx},${r.vy},${r.shot}`);
    }
    return out.join(";");
  };
  assert.equal(trace(7), trace(7));
  assert.notEqual(trace(7), trace(8));
  const [a, b] = [seededRandom(1), seededRandom(1)];
  assert.deepEqual([a(), a(), a()], [b(), b(), b()]);
});

test("wander leaves busy pets alone, stays in the room and sometimes reacts", () => {
  const run = loadBehaviour(wanderModule(), { seed: 3 });
  assert.ok(run.ok);
  const busy = run.behaviour.tick(state({ free: false, busy: true }));
  assert.deepEqual(busy, { ok: true, shot: null, vx: 0, vy: 0 });
  const bounds = { minX: 10, maxX: 300, minY: 20, maxY: 200 };
  let [x, y] = [150, 100];
  let walked = 0;
  const shots = new Set<string | null>();
  for (let i = 0; i < 4000; i++) {
    const r = run.behaviour.tick(state({ x, y, bounds }));
    assert.ok(r.ok);
    assert.ok(Math.abs(r.vx) <= LIMITS.maxSpeed && Math.abs(r.vy) <= LIMITS.maxSpeed);
    x = Math.min(bounds.maxX, Math.max(bounds.minX, x + r.vx / 4));
    y = Math.min(bounds.maxY, Math.max(bounds.minY, y + r.vy / 4));
    if (r.vx !== 0 || r.vy !== 0) walked++;
    shots.add(r.shot);
  }
  assert.ok(walked > 1000 && walked < 3600, `walked on ${walked} ticks`);
  assert.ok(shots.has("waving") && shots.has("jumping") && shots.has(null));
  assert.ok(![...shots].some((s) => s !== null && !SHOTS.includes(s)));
});

test("state kept in globals lasts across ticks, and rand is the module's only source of chance", () => {
  const counter = loadBehaviour(ticking([...gget(0), ...c(1), ...op.add, ...gset(0), ...store(VX, gget(0))], { globals: [0] }));
  assert.ok(counter.ok);
  const seen = [1, 2, 3].map(() => counter.ok && counter.behaviour.tick(state()));
  assert.deepEqual(seen.map((r) => r && r.ok && r.vx), [1, 2, 3]);
  const dice = (seed: number) => {
    const r = loadBehaviour(ticking([...store(VX, [...call(0), ...c(31), ...op.and])], { rand: true, exports: exportsOf(1) }), { seed });
    return r.ok ? [r.behaviour.tick(state()), r.behaviour.tick(state())] : null;
  };
  assert.deepEqual(dice(5), dice(5));
  assert.notDeepEqual(dice(5), dice(6));
});

test("a trap stops the behaviour for good and reports it once; the caller is never thrown at", () => {
  const faults: string[] = [];
  const run = loadBehaviour(ticking([0x00]), { onFault: (m) => faults.push(m) });
  assert.ok(run.ok);
  const first = run.behaviour.tick(state());
  assert.equal(first.ok, false);
  assert.match(first.ok ? "" : first.error, /trapped/);
  assert.equal(run.behaviour.stopped, true);
  const again = run.behaviour.tick(state());
  assert.match(again.ok ? "" : again.error, /^stopped: trapped/);
  assert.equal(faults.length, 1);
});

test("division by zero and an access outside the memory trap rather than corrupt anything", () => {
  for (const body of [[...c(1), ...c(0), 0x6d, 0x1a], [...store(70000, c(1))], [...load(65534), 0x1a]]) {
    const run = loadBehaviour(ticking(body));
    assert.ok(run.ok);
    const r = run.behaviour.tick(state());
    assert.equal(r.ok, false, String(body));
    assert.equal(run.behaviour.stopped, true);
  }
});

test("a tick that takes too long, or answers with something invalid, stops the behaviour", () => {
  let now = 0;
  const slow = loadBehaviour(ticking([0x01]), { clock: () => (now += LIMITS.maxTickMs + 1) });
  assert.ok(slow.ok);
  for (let i = 1; i < LIMITS.maxSlowTicks; i++) assert.equal(slow.behaviour.tick(state()).ok, true, "one slow tick is forgiven");
  const r = slow.behaviour.tick(state());
  assert.match(r.ok ? "" : r.error, /ticks took over 50 ms/);
  assert.equal(slow.behaviour.stopped, true);
  const bad = loadBehaviour(ticking([...store(SHOT, c(SHOTS.length + 1))]));
  assert.ok(bad.ok);
  assert.match((bad.behaviour.tick(state()) as { error: string }).error, /does not exist/);
  const negative = loadBehaviour(ticking([...store(SHOT, c(-1))]));
  assert.ok(negative.ok);
  assert.equal(negative.behaviour.tick(state()).ok, false);
});

test("the host counts live behaviours, reports what it refuses or stops, and frees a closed slot", () => {
  const reports: { name: string; phase: string; reason: string }[] = [];
  const host = createHost({ report: (info) => reports.push(info) });
  const ok = ticking([0x01]);
  const live = Array.from({ length: LIMITS.maxBehaviours }, (_, i) => host.load(`b${i}`, ok));
  assert.ok(live.every((l) => l.ok));
  const over = host.load("extra", ok);
  assert.equal(over.ok, false);
  assert.equal(!over.ok && over.busy, true, "a full frame is not a refused module");
  assert.deepEqual(reports.map((r) => [r.name, r.phase]), [["extra", "load"]]);
  if (live[0]?.ok) live[0].behaviour.close();
  assert.equal(host.load("again", ok).ok, true);

  const bad = createHost({ report: (info) => reports.push(info) }).load("looping", ticking([0x03, 0x40, 0x0b]));
  assert.equal(bad.ok, false);
  assert.equal(reports.at(-1)?.phase, "load");
  assert.match(reports.at(-1)?.reason ?? "", /loops/);

  const trap = createHost({ report: (info) => reports.push(info) }).load("trapper", ticking([0x00]), { seed: 1 });
  assert.ok(trap.ok);
  trap.behaviour.tick(state());
  trap.behaviour.tick(state());
  assert.deepEqual(reports.filter((r) => r.name === "trapper").map((r) => r.phase), ["tick"]);
});

test("a report callback that throws cannot break the host", () => {
  const host = createHost({
    report: () => {
      throw new Error("boom");
    },
  });
  assert.equal(host.load("looping", ticking([0x03, 0x40, 0x0b])).ok, false);
  const trap = host.load("t", ticking([0x00]));
  assert.ok(trap.ok);
  assert.equal(trap.behaviour.tick(state()).ok, false);
});

test("a module cannot reach anything the host did not give it", () => {
  // Compiled with an empty import object, a module importing anything else cannot even link.
  const outside = ticking([], { rand: { module: "env", name: "fetch" }, exports: exportsOf(1) });
  assert.equal(loadBehaviour(outside).ok, false);
  assert.throws(() => new WebAssembly.Instance(new WebAssembly.Module(outside), {}));
});

test("memory access flags cannot desynchronise the validator (multi-memory smuggling)", () => {
  // The reviewed proof of concept: i32.load with alignment 0x42 (bit 6 = a memory index follows), hiding a loop.
  const poc = [0x41, 0x00, 0x28, 0x42, 0x00, 0x02, 0x40, 0x00, 0x1a, 0x41, 0x00, 0x28, 0x42, 0x00, 0x41, 0x03, 0x00, 0x0c, 0x00, 0x0b, 0x1a, 0x0b];
  refused(ticking(poc.slice(0, -1)), /natural alignment/);
  // Every access kind: alignment at its natural limit is fine, one more is refused, flag bits are refused.
  const access = (opcode: number, align: number, store = false): Uint8Array => ticking(store ? [...c(0), ...c(0), opcode, align, 0] : [...c(0), opcode, align, 0, 0x1a]);
  for (const [opcode, natural, store] of [[0x28, 2, false], [0x2c, 0, false], [0x2d, 0, false], [0x2e, 1, false], [0x2f, 1, false], [0x36, 2, true], [0x3a, 0, true], [0x3b, 1, true]] as const) {
    assert.equal(validateModule(access(opcode, natural, store)).ok, true, `op ${opcode} at its natural alignment`);
    refused(access(opcode, natural + 1, store), /natural alignment/);
    refused(access(opcode, 0x40 | natural, store), /natural alignment/);
  }
});
