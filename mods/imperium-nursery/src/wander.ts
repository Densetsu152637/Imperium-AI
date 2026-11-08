// Wandering, pure: an idle pet now and then walks a few pixels to one side and
// stops. The arbiter has no walk state, so the view lays this over `idle`: while `dir` is not 0
// it draws walk-left/walk-right and slides the sprite by `x`.

export type Walker = { x: number; target: number; nextAt: number };

/** How far a pet strays from the middle of its cell, in px. */
export const RANGE = 14;
/** Walking speed in px per second, and while playing. */
export const SPEED = 16;
export const PLAY_SPEED = 44;

/** Time to the next wander: 6 to 20 seconds. */
export const nextDelay = (rand: () => number): number => 6000 + Math.floor(rand() * 14_000);

export const newWalker = (now: number, rand: () => number): Walker => ({ x: 0, target: 0, nextAt: now + nextDelay(rand) });

/** Advances a walker by `dt` ms at `now`; `idle` false (the pet is busy) stops it where it is. */
export function stepWalker(w: Walker, now: number, dt: number, idle: boolean, rand: () => number, range = RANGE, speed = SPEED): { walker: Walker; dir: -1 | 0 | 1 } {
  // A busy pet goes back to its slot without a walk animation.
  if (!idle) {
    const gap = -w.x;
    const move = Math.min(Math.abs(gap), (speed * Math.max(0, dt)) / 1000) * Math.sign(gap);
    return { walker: { x: w.x + move, target: 0, nextAt: Math.max(w.nextAt, now + 2000) }, dir: 0 };
  }
  let { target, nextAt } = w;
  if (Math.abs(target - w.x) < 0.5 && now >= nextAt) {
    target = Math.round((rand() * 2 - 1) * range);
    nextAt = now + nextDelay(rand);
  }
  const gap = target - w.x;
  if (Math.abs(gap) < 0.5) return { walker: { x: target, target, nextAt }, dir: 0 };
  const move = Math.min(Math.abs(gap), (speed * Math.max(0, dt)) / 1000) * Math.sign(gap);
  return { walker: { x: w.x + move, target, nextAt }, dir: gap > 0 ? 1 : -1 };
}
