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
export const AREA_PER_PET = 9_000;
export const MIN_ROOM_HEIGHT = 220;
/** Two pets closer than this (x, y) push each other apart, gently enough that they may overlap; depth counts for less than width. */
export const SEP_X = 34;
export const SEP_Y = 14;
/** How fast overlapping pets move apart, in px per second at full overlap. */
export const SEP_RATE = 70;
/** The smallest a pet is drawn in a crowded room, as a fraction of its full size. */
export const MIN_SCALE = 0.3;

/** A pet's whole box: the sprite above the feet and the label below, as wide as the label. */
export const boxSize = (m: Metrics): Size => ({ width: m.half * 2, height: m.above + m.below });

/** Whether two pets standing at `a` and `b` have boxes (sprite plus label) that overlap; touching is not overlapping. */
export const boxesOverlap = (a: Pos, b: Pos, m: Metrics): boolean => {
  const box = boxSize(m);
  return Math.abs(a.x - b.x) < box.width && Math.abs(a.y - b.y) < box.height;
};

/** The narrowest a label is drawn while there is room for it: a name stays readable when the sprite shrinks. */
export const MIN_LABEL = 72;

/**
 * A pet drawn at `scale` of full size: the sprite shrinks, and so does the label's width, but not
 * below `MIN_LABEL` (or its full width) unless `labelFloor` is off. The label line and margins do not shrink. Whole pixels.
 */
export const scaleMetrics = (m: Metrics, scale: number, labelFloor = true): Metrics => ({
  ...m,
  half: Math.max(labelFloor ? Math.min(m.half, MIN_LABEL / 2) : 0, Math.round(m.half * scale)),
  above: Math.round(m.above * scale),
});

/**
 * Feet spots of a grid of boxes that do not overlap (row-major): as many columns and rows of whole
 * boxes as fit inside the room's margins, with the spare space shared between them. Empty when not
 * even one box fits.
 */
export function boxSlots(room: Size, m: Metrics): Pos[] {
  const box = boxSize(m);
  const width = Math.floor(room.width) - 2 * m.pad;
  const height = Math.floor(room.height) - 2 * m.pad;
  const cols = Math.floor(width / box.width);
  const rows = Math.floor(height / box.height);
  if (cols < 1 || rows < 1) return [];
  const gapX = Math.floor((width - cols * box.width) / cols);
  const gapY = Math.floor((height - rows * box.height) / rows);
  const slots: Pos[] = [];
  for (let r = 0; r < rows; r++) {
    for (let c = 0; c < cols; c++) {
      slots.push({ x: m.pad + Math.floor(gapX / 2) + c * (box.width + gapX) + m.half, y: m.pad + Math.floor(gapY / 2) + r * (box.height + gapY) + m.above });
    }
  }
  return slots;
}

/** Up to this many pets need no spare boxes; above it a tenth more (at least one) leave room to wander in. */
export const SLACK_FROM = 4;

/**
 * How big `count` pets can be drawn, as a fraction of `base` (1 down to `min`, in steps of 0.05):
 * the largest at which all their whole boxes fit in the room without overlapping (one more per ten
 * pets once there are more than `SLACK_FROM`, for wandering). Labels keep at least `MIN_LABEL`
 * unless no size fits that way, and only then narrow with the sprite. `min` when even that is too tight.
 */
export function fitMetrics(room: Size, count: number, base: Metrics, min = MIN_SCALE): { scale: number; metrics: Metrics } {
  const need = count <= SLACK_FROM ? count : count + Math.ceil(count / 10);
  for (const floor of [true, false]) {
    for (let percent = 100; percent > min * 100; percent -= 5) {
      const metrics = scaleMetrics(base, percent / 100, floor);
      if (boxSlots(room, metrics).length >= need) return { scale: percent / 100, metrics };
    }
  }
  return { scale: min, metrics: scaleMetrics(base, min, false) };
}

export const fitScale = (room: Size, count: number, base: Metrics, min = MIN_SCALE): number => fitMetrics(room, count, base, min).scale;

/**
 * Which pets to put back on a free spot after the boxes grew: those that cannot move by
 * themselves (`moves` false: asleep, at work) and overlap an earlier pet. Pets that can move are
 * never listed; the separation pass pushes them apart.
 */
export function stranded(pets: { pos: Pos; moves: boolean }[], m: Metrics): number[] {
  const out: number[] = [];
  const kept: Pos[] = [];
  pets.forEach((pet, i) => {
    if (!pet.moves && kept.some((q) => boxesOverlap(pet.pos, q, m))) out.push(i);
    else kept.push(pet.pos);
  });
  return out;
}

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

/**
 * A roaming spot at least `minTravel` px from `from` whose whole box is clear of the boxes of
 * `others` (the other pets' feet and the spots they are walking to): a few random tries over the
 * room, then any free grid spot. Null when there is none, so the caller may take any spot.
 */
export function clearTarget(b: Bounds, from: Pos, others: Pos[], m: Metrics, room: Size, rand: () => number, minTravel = 60): Pos | null {
  const clear = (p: Pos): boolean => distance(p, from) >= minTravel && others.every((q) => !boxesOverlap(p, q, m));
  for (let i = 0; i < 8; i++) {
    const p = { x: Math.round(b.minX + rand() * (b.maxX - b.minX)), y: Math.round(b.minY + rand() * (b.maxY - b.minY)) };
    if (clear(p)) return p;
  }
  const free = boxSlots(room, m).filter(clear);
  return free.length > 0 ? free[Math.min(free.length - 1, Math.floor(rand() * free.length))]! : null;
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
 * a new spot anywhere in the room (`pick`, when given, chooses it), then walks there at a steady speed.
 */
export function roam(r: Roamer, b: Bounds, now: number, dt: number, rand: () => number, speed = SPEED, pick: ((from: Pos) => Pos) | null = null): { roamer: Roamer; dx: number; dy: number } {
  let { tx, ty, nextAt } = r;
  if (Math.hypot(tx - r.x, ty - r.y) < 0.5 && now >= nextAt) {
    const t = pick ? pick(r) : pickTarget(b, r, rand);
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
 * `sep` is how close (x, y) pets may stand before they push; the room passes a pet's box. Returns
 * the new positions in the order of `bodies`.
 */
export function separate(bodies: Body[], dt: number, rate = SEP_RATE, sep: Size = { width: SEP_X, height: SEP_Y }): Pos[] {
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
      const d = Math.hypot(dx / sep.width, dy / sep.height);
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
 * at the best of a few random spots: the one farthest from the pets already there. With `spot`
 * (the room and a pet's metrics) it takes a grid spot whose whole box is clear of the pets already
 * there (the one nearest `anchor`, else a random one) when any is, and falls back to the above.
 */
export function spawnPoint(b: Bounds, existing: Pos[], rand: () => number, anchor: Pos | null = null, gap = 64, spot: { room: Size; m: Metrics } | null = null): Pos {
  if (spot) {
    const free = boxSlots(spot.room, spot.m).filter((slot) => existing.every((q) => !boxesOverlap(slot, q, spot.m)));
    if (free.length > 0) {
      if (!anchor) return free[Math.min(free.length - 1, Math.floor(rand() * free.length))]!;
      return free.reduce((best, slot) => (distance(slot, anchor) < distance(best, anchor) ? slot : best));
    }
  }
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
 * Where `n` pets stand with reduced motion in a room of `room`: spread over the grid of boxes when
 * they all fit (whole boxes apart), else evenly in rows like `reducedLayout`.
 */
export function spreadSpots(n: number, room: Size, m: Metrics): Pos[] {
  if (n <= 0) return [];
  const slots = boxSlots(room, m);
  if (slots.length < n) return reducedLayout(n, boundsOf(room, m));
  return Array.from({ length: n }, (_, i) => slots[Math.floor((i * slots.length) / n)]!);
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
