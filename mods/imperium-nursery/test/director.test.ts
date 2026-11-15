// The director that steers free pets with a WebAssembly behaviour: one instance per pet, ticked
// at a steady pace, and a behaviour that is refused or stops leaves the pet to the built-in
// roaming. The host is faked here; the real one is tested in apps/desktop/test/behaviour.test.ts.

import { test } from "node:test";

import type { Behaviour, BehaviourState, BehaviourTick, Behaviours } from "@imperium/mod-sdk";

import { Director, TICK_MS } from "../src/behaviour/director.ts";
import { Nursery } from "../src/nursery.ts";
import { allDrawn } from "../src/pet.ts";
import type { Env } from "../src/arbiter.ts";
import { expect } from "./expect.ts";

const T0 = 1_000_000;
const pet = { anim: "idle", quiet: 0, x: 10, y: 20, bounds: { minX: 0, maxX: 100, minY: 0, maxY: 100 } };

type Fake = { loads: { name: string; seed: number | undefined }[]; ticks: BehaviourState[]; closed: number; script: (n: number) => BehaviourTick };

/** A host whose behaviours answer from `script` (called with the tick number, from 0). */
function host(script: (n: number) => BehaviourTick, refuse: string | null = null): { host: Pick<Behaviours, "load">; fake: Fake } {
  const fake: Fake = { loads: [], ticks: [], closed: 0, script };
  return {
    fake,
    host: {
      load(name, _bytes, options) {
        fake.loads.push({ name, seed: options?.seed });
        if (refuse !== null) return { ok: false, error: refuse };
        let stopped: string | null = null;
        let n = 0;
        const behaviour: Behaviour = {
          get stopped() {
            return stopped !== null;
          },
          get fault() {
            return stopped;
          },
          fuel: 10,
          tick(state) {
            fake.ticks.push(state);
            const out = script(n++);
            if (!out.ok) stopped = out.error;
            return out;
          },
          close() {
            fake.closed++;
          },
        };
        return { ok: true, behaviour };
      },
    },
  };
}

const walk: BehaviourTick = { ok: true, shot: null, vx: 12, vy: -6 };

test("a pet's behaviour is loaded once, seeded from the pet, and ticked every TICK_MS", () => {
  const { host: h, fake } = host(() => walk);
  const d = new Director(h, "wander", new Uint8Array([0]));
  expect(d.guide("a", pet, T0)).toEqual({ vx: 12, vy: -6, shot: null });
  expect(fake.loads).toHaveLength(1);
  expect(typeof fake.loads[0]!.seed).toBe("number");
  // Between ticks the last velocity holds and the module is not run.
  expect(d.guide("a", pet, T0 + 100)).toEqual({ vx: 12, vy: -6, shot: null });
  expect(fake.ticks).toHaveLength(1);
  d.guide("a", pet, T0 + TICK_MS);
  expect(fake.ticks).toHaveLength(2);
  expect(fake.ticks[1]!.dt).toBe(TICK_MS);
  expect(fake.ticks[0]).toMatchObject({ free: true, busy: false, x: 10, y: 20 });
  expect(fake.loads).toHaveLength(1);
});

test("two pets get two instances with different seeds", () => {
  const { host: h, fake } = host(() => walk);
  const d = new Director(h, "wander", new Uint8Array([0]));
  d.guide("a", pet, T0);
  d.guide("b", pet, T0);
  expect(fake.loads).toHaveLength(2);
  expect(fake.loads[0]!.seed === fake.loads[1]!.seed).toBe(false);
});

test("a reaction is passed on once, on the tick that asked for it", () => {
  const { host: h } = host((n) => ({ ok: true, shot: n === 0 ? "waving" : null, vx: 0, vy: 0 }));
  const d = new Director(h, "wander", new Uint8Array([0]));
  expect(d.guide("a", pet, T0)?.shot).toBe("waving");
  expect(d.guide("a", pet, T0 + 50)?.shot).toBeNull();
  expect(d.guide("a", pet, T0 + TICK_MS)?.shot).toBeNull();
});

test("a behaviour that stops leaves that pet to the built-in roaming, and only that pet", () => {
  const { host: h, fake } = host((n) => (n === 1 ? { ok: false, error: "trapped: unreachable" } : walk));
  const d = new Director(h, "wander", new Uint8Array([0]));
  expect(d.guide("a", pet, T0)).toEqual({ vx: 12, vy: -6, shot: null });
  expect(d.guide("a", pet, T0 + TICK_MS)).toBeNull();
  expect(d.guide("a", pet, T0 + 2 * TICK_MS)).toBeNull();
  expect(fake.closed).toBe(1);
  expect(fake.loads).toHaveLength(1); // not loaded again
  expect(d.faults).toHaveLength(1);
  expect(d.faults[0]).toMatch(/trapped: unreachable/);
  expect(d.guide("b", pet, T0 + TICK_MS)).toEqual({ vx: 12, vy: -6, shot: null });
  expect(d.active).toBe(true);
});

test("a module the host refuses switches the director off for every pet", () => {
  const { host: h, fake } = host(() => walk, "loops are not allowed");
  const d = new Director(h, "wander", new Uint8Array([0]));
  expect(d.guide("a", pet, T0)).toBeNull();
  expect(d.guide("b", pet, T0)).toBeNull();
  expect(fake.loads).toHaveLength(1);
  expect(d.active).toBe(false);
  expect(d.refused).toBe("loops are not allowed");
});

test("a pet that leaves frees its instance; clear frees them all", () => {
  const { host: h, fake } = host(() => walk);
  const d = new Director(h, "wander", new Uint8Array([0]));
  d.guide("a", pet, T0);
  d.guide("b", pet, T0);
  d.forget("a");
  expect(fake.closed).toBe(1);
  d.clear();
  expect(fake.closed).toBe(2);
  d.guide("a", pet, T0 + 1000);
  expect(fake.loads).toHaveLength(3);
});

test("the nursery frees a pet's behaviour when the pet is taken out, and hands a reaction to its arbiter", () => {
  const env: Env = { catalog: allDrawn(), sleepAfterMs: 600_000, fidget: false, rng: () => 0 };
  const n = new Nursery("nursery", { envFor: () => env, assign: () => "pet" });
  n.add("x", "pet", "x", T0);
  const { host: h, fake } = host(() => walk);
  n.director = new Director(h, "wander", new Uint8Array([0]));
  n.director.guide("x", pet, T0);
  n.shot("x", "waving", T0 + 5000);
  expect(n.view(n.entries.get("x")!, T0 + 5001).name).toBe("waving");
  n.take("x");
  expect(fake.closed).toBe(1);
  n.shot("gone", "waving", T0); // no such pet: nothing happens
});
