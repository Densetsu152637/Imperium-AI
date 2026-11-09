// The nursery's shared room, pure: where pets may stand, where they wander, how they keep from
// stacking on one spot, who plays with whom, and where they stand with reduced motion. (x, y) is
// the middle of a pet's feet; y is the floor depth, so a larger y is nearer and is drawn in front.
// Time and randomness are injected, like the arbiter's.

export type Pos = { x: number; y: number };
export type Size = { width: number; height: number };

/** The space a pet takes around its feet: half its width, the sprite above, the label below, and a margin. */
export type Metrics = { half: number; above: number; below: number; pad: number };

/** The rectangle the feet may be in. */
export type Bounds = { minX: number; maxX: number; minY: number; maxY: number };

/** Walking speed in px per second while roaming and while playing. */
export const SPEED = 22;
export const PLAY_SPEED = 50;
/** The least area a pet needs in the room, in px squared. */
export const AREA_PER_PET = 11_000;
export const MIN_ROOM_HEIGHT = 160;
/** Two pets closer than this (x, y) push each other apart; depth counts for less than width. */
export const SEP_X = 58;
export const SEP_Y = 24;
/** How fast overlapping pets move apart, in px per second at full overlap. */
export const SEP_RATE = 70;

/** The room's feet bounds; a room too small for a pet collapses to its middle. */
export function boundsOf(room: Size, m: Metrics): Bounds {
  const span = (lo: number, hi: number): [number, number] => (hi >= lo ? [lo, hi] : [(lo + hi) / 2, (lo + hi) / 2]);
  const [minX, maxX] = span(m.half + m.pad, room.width - m.half - m.pad);
  const [minY, maxY] = span(m.above + m.pad, room.height - m.below - m.pad);
  return { minX, maxX, minY, maxY };
}

export const clampTo = (b: Bounds, p: Pos): Pos => ({ x: Math.min(b.maxX, Math.max(b.minX, p.x)), y: Math.min(b.maxY, Math.max(b.minY, p.y)) });

/** The height a room of `width` needs for `count` pets: the minimum area each, never below `min`. */
export function roomHeight(count: number, width: number, area = AREA_PER_PET, min = MIN_ROOM_HEIGHT): number {
  if (width <= 0) return min;
  return Math.max(min, Math.ceil((count * area) / width));
}

/** Time to stand still before the next walk: 2 to 8 seconds. */
export const pauseDelay = (rand: () => number): number => 2000 + Math.floor(rand() * 6000);

export type Roamer = { x: number; y: number; tx: number; ty: number; nextAt: number };

export const newRoamer = (p: Pos, now: number, rand: () => number): Roamer => ({ x: p.x, y: p.y, tx: p.x, ty: p.y, nextAt: now + pauseDelay(rand) });

export const distance = (a: Pos, b: Pos): number => Math.hypot(a.x - b.x, a.y - b.y);

/** A spot anywhere in the room at least `minTravel` px from `from` when one turns up in a few tries. */
export function pickTarget(b: Bounds, from: Pos, rand: () => number, minTravel = 60): Pos {
  let p = from;
  for (let i = 0; i < 6; i++) {
    p = { x: Math.round(b.minX + rand() * (b.maxX - b.minX)), y: Math.round(b.minY + rand() * (b.maxY - b.minY)) };
    if (distance(p, from) >= minTravel) break;
  }
  return p;
}

/** One movement step from `from` toward `to` at `speed` px/s over `dt` ms; never overshoots. */
export function stepToward(from: Pos, to: Pos, speed: number, dt: number): { x: number; y: number; dx: number; dy: number; arrived: boolean } {
  const gx = to.x - from.x;
  const gy = to.y - from.y;
  const gap = Math.hypot(gx, gy);
  if (gap < 0.5) return { x: to.x, y: to.y, dx: 0, dy: 0, arrived: true };
  const move = Math.min(gap, (speed * Math.max(0, dt)) / 1000);
  const dx = (gx / gap) * move;
  const dy = (gy / gap) * move;
  return { x: from.x + dx, y: from.y + dy, dx, dy, arrived: move >= gap };
}

/**
 * Advances a roaming pet by `dt` ms at `now`: once it has arrived and its pause is over it picks
 * a new spot anywhere in the room, then walks there at a steady speed.
 */
export function roam(r: Roamer, b: Bounds, now: number, dt: number, rand: () => number, speed = SPEED): { roamer: Roamer; dx: number; dy: number } {
  let { tx, ty, nextAt } = r;
  if (Math.hypot(tx - r.x, ty - r.y) < 0.5 && now >= nextAt) {
    const t = pickTarget(b, r, rand);
    [tx, ty, nextAt] = [t.x, t.y, now + pauseDelay(rand)];
  }
  const s = stepToward(r, { x: tx, y: ty }, speed, dt);
  return { roamer: { x: s.x, y: s.y, tx, ty, nextAt }, dx: s.dx, dy: s.dy };
}

/** A pet for the separation pass. `group` is shared by pets that are meant to stand close (a play). */
export type Body = { key: string; x: number; y: number; movable: boolean; group: string | null };

/**
 * Soft separation: pets that overlap are pushed apart along the line between them, at a rate that
 * grows with the overlap. Pets that cannot move (at work) are not pushed; the others give way.
 * Returns the new positions in the order of `bodies`.
 */
export function separate(bodies: Body[], dt: number, rate = SEP_RATE): Pos[] {
  const out = bodies.map((p) => ({ x: p.x, y: p.y }));
  const step = (rate * Math.max(0, dt)) / 1000;
  for (let i = 0; i < bodies.length; i++) {
    for (let j = i + 1; j < bodies.length; j++) {
      const a = bodies[i]!;
      const c = bodies[j]!;
      if (a.group !== null && a.group === c.group) continue;
      if (!a.movable && !c.movable) continue;
      let dx = c.x - a.x;
      let dy = c.y - a.y;
      const d = Math.hypot(dx / SEP_X, dy / SEP_Y);
      if (d >= 1) continue;
      if (d < 1e-6) [dx, dy] = [(i + j) % 2 === 0 ? 1 : -1, 0.2];
      const len = Math.hypot(dx, dy);
      const push = (1 - d) * step;
      const share = a.movable && c.movable ? 0.5 : 1;
      const [ux, uy] = [dx / len, dy / len];
      const oa = out[i]!;
      const oc = out[j]!;
      if (a.movable) [oa.x, oa.y] = [oa.x - ux * push * share, oa.y - uy * push * share * 0.5];
      if (c.movable) [oc.x, oc.y] = [oc.x + ux * push * share, oc.y + uy * push * share * 0.5];
    }
  }
  return out;
}

/** The partner for a pet at `self` among `others`: usually the nearest, now and then any of them. */
export function nearestPartner(self: Pos, others: { key: string; x: number; y: number }[], rand: () => number): string | null {
  if (others.length === 0) return null;
  const sorted = [...others].sort((a, b) => distance(self, a) - distance(self, b));
  if (rand() < 0.7) return sorted[0]!.key;
  return sorted[Math.floor(rand() * sorted.length)]!.key;
}

/**
 * Where a new pet appears. Next to `anchor` (a subagent beside its parent) when given, otherwise
 * at the best of a few random spots: the one farthest from the pets already there.
 */
export function spawnPoint(b: Bounds, existing: Pos[], rand: () => number, anchor: Pos | null = null, gap = 64): Pos {
  let best: Pos = { x: (b.minX + b.maxX) / 2, y: (b.minY + b.maxY) / 2 };
  let bestScore = -1;
  for (let i = 0; i < 8; i++) {
    const p = anchor
      ? clampTo(b, { x: anchor.x + (rand() < 0.5 ? -1 : 1) * (gap + rand() * 30), y: anchor.y + (rand() - 0.5) * 30 })
      : { x: Math.round(b.minX + rand() * (b.maxX - b.minX)), y: Math.round(b.minY + rand() * (b.maxY - b.minY)) };
    const score = existing.reduce((least, q) => Math.min(least, distance(p, q)), Infinity);
    if (score > bestScore) [best, bestScore] = [p, score];
  }
  return best;
}

/** A deterministic spread of `n` spots over the room (reduced motion): rows of evenly spaced pets. */
export function reducedLayout(n: number, b: Bounds): Pos[] {
  if (n <= 0) return [];
  const w = Math.max(1, b.maxX - b.minX);
  const h = Math.max(1, b.maxY - b.minY);
  const cols = Math.min(n, Math.max(1, Math.round(Math.sqrt((n * w) / h))));
  const rows = Math.ceil(n / cols);
  return Array.from({ length: n }, (_, i) => {
    const r = Math.floor(i / cols);
    const c = i % cols;
    const inRow = r === rows - 1 ? n - r * cols : cols;
    return { x: b.minX + ((c + 0.5) * w) / inRow, y: b.minY + ((r + 0.5) * h) / rows };
  });
}

/**
 * Where a pet in a play walks. `toward` 1 is to a spot `gap` px from the partner (smaller
 * fractions stop farther off), a negative one runs away from the partner up to `flee` px, and
 * 0 means stand where it is (null).
 */
export function playTarget(toward: number, me: Pos, other: Pos, b: Bounds, gap = 60, flee = 170): Pos | null {
  if (toward === 0) return null;
  const dx = me.x - other.x;
  const dy = me.y - other.y;
  const len = Math.hypot(dx, dy);
  const [ux, uy] = len < 1e-6 ? [1, 0] : [dx / len, dy / len];
  if (toward > 0) return clampTo(b, { x: other.x + (ux * gap) / toward, y: other.y + (uy * gap) / toward });
  const run = flee * -toward;
  const away = clampTo(b, { x: me.x + ux * run, y: me.y + uy * run });
  if (distance(away, me) >= 24) return away;
  // Cornered: run along the wall instead.
  const side = clampTo(b, { x: me.x - uy * run, y: me.y + ux * run });
  return distance(side, me) >= 24 ? side : clampTo(b, { x: me.x + uy * run, y: me.y - ux * run });
}

/** A spot just outside the nearer side of the room, level with `p`: where a finished pet walks to leave. */
export const edgeTarget = (b: Bounds, p: Pos, margin = 80): Pos => ({ x: p.x - b.minX < b.maxX - p.x ? b.minX - margin : b.maxX + margin, y: p.y });
