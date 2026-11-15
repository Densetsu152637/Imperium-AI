// The example behaviour: a pet that picks a heading, walks it for a few seconds, sometimes stops,
// sometimes waves or jumps, and turns back at the edge of the room. It is written with the
// assembler in asm.ts, so the module is built (the same bytes every time) rather than stored.
//
// The module's memory words (see ABI in apps/desktop/src/mods/behaviour.js). Input, bytes 0-47:
//   4 dt ms, 16 flags (bit 0: the pet is free to roam), 24 x, 28 y, 32 minX, 36 maxX, 40 minY, 44 maxY.
// Output: 64 shot (0 none, 1 waving, 2 jumping, 3 celebrating, 4 fidget), 68 vx, 72 vy (px/s).
// Globals: 0 vx, 1 vy, 2 ms left on this heading.

import { assemble, op } from "./asm.ts"

const { c, gget, gset, load, store, lget, lset, call, when } = op

const [DT, FLAGS, X, Y, MIN_X, MAX_X, MIN_Y, MAX_Y] = [4, 16, 24, 28, 32, 36, 40, 44]
const [SHOT, VX, VY] = [64, 68, 72]
const [G_VX, G_VY, G_LEFT] = [0, 1, 2]
const RAND = 0
const R = 0

/** The shifted, masked piece of the random word in local 0: `(r >> shift) & mask`. */
const bits = (shift: number, mask: number): number[] => [...lget(R), ...c(shift), ...op.shru, ...c(mask), ...op.and]

/** Turns a velocity around when the pet is at an edge it is heading into (`axis`: its global, position, edge). */
const bounce = (g: number, pos: number, minEdge: number, maxEdge: number): number[] => [
  ...when([...gget(g), ...c(0), ...op.lt, ...load(pos), ...load(minEdge), ...op.le, ...op.and], [...c(0), ...gget(g), ...op.sub, ...gset(g)]),
  ...when([...gget(g), ...c(0), ...op.gt, ...load(pos), ...load(maxEdge), ...op.ge, ...op.and], [...c(0), ...gget(g), ...op.sub, ...gset(g)]),
]

export function wanderModule(): Uint8Array {
  const body = [
    // The heading's clock runs down by the tick's dt.
    ...gget(G_LEFT), ...load(DT), ...op.sub, ...gset(G_LEFT),
    // A pet that is busy (working, waiting, reacting) is left alone.
    ...when([...load(FLAGS), ...c(1), ...op.and, ...op.eqz], [...op.ret]),
    // Time for a new heading.
    ...when([...gget(G_LEFT), ...c(0), ...op.le], [
      ...call(RAND), ...lset(R),
      ...bits(0, 63), ...c(32), ...op.sub, ...gset(G_VX),
      ...bits(6, 31), ...c(16), ...op.sub, ...gset(G_VY),
      // One heading in four is a rest.
      ...when([...bits(12, 3), ...op.eqz], [...c(0), ...gset(G_VX), ...c(0), ...gset(G_VY)]),
      ...c(2000), ...bits(14, 4095), ...op.add, ...gset(G_LEFT),
      // One in eight is a wave or a jump.
      ...when([...bits(26, 7), ...op.eqz], [...store(SHOT, [...c(1), ...bits(29, 1), ...op.add])]),
    ]),
    ...bounce(G_VX, X, MIN_X, MAX_X),
    ...bounce(G_VY, Y, MIN_Y, MAX_Y),
    ...store(VX, gget(G_VX)),
    ...store(VY, gget(G_VY)),
  ]
  return assemble({
    rand: true,
    funcs: [{ locals: 1, body }],
    memory: { min: 1, max: 1 },
    globals: [0, 0, 0],
    exports: [
      { name: "memory", kind: "memory", index: 0 },
      { name: "tick", kind: "func", index: 1 },
    ],
  })
}
