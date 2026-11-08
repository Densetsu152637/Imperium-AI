// Play, pure: every so often two free pets act out a short scripted interaction. Time and
// randomness are injected, like the arbiter's.
//
// A script is a timeline of steps per pet. A `shot` step is a one-shot handed to that pet's
// own arbiter (waving, jumping, celebrating, waking), so the arbiter's rules decide whether it
// plays; a pet that does not draw it falls back along its chain or just stays idle. A `pose`
// step is what the view lays over an idle pet: walk toward/away from the partner, face the
// partner with the `look` cells, or sleep. Nothing here outranks a real event: the nursery
// cancels play the moment either pet gets one, and the view draws a pose only while the
// arbiter shows `idle`.

export type PlayKind = "greet" | "chase" | "highfive" | "nap" | "showoff";

/**
 * What an idle pet does during play. `toward` for `walk` is the fraction of its reach to cover:
 * 1 is all the way toward the partner, -1 all the way away, 0 back at its slot.
 */
export type Pose = { mode: "idle" | "walk" | "look" | "sleep"; toward: number };

export type Act = { kind: "pose"; pose: Pose } | { kind: "shot"; name: string };
export type Step = { at: number; who: 0 | 1; act: Act };

export type Play = {
  kind: PlayKind;
  /** The two pets; `who` 0 is keys[0]. */
  keys: [string, string];
  startedAt: number;
  steps: Step[];
  endMs: number;
  /** Steps before this index have been delivered. */
  next: number;
};

const pose = (mode: Pose["mode"], toward = 0): Act => ({ kind: "pose", pose: { mode, toward } });
const shot = (name: string): Act => ({ kind: "shot", name });
const both = (at: number, act: Act): Step[] => [
  { at, who: 0, act },
  { at, who: 1, act },
];

/** The timeline of each interaction, in ms from its start. */
export function stepsOf(kind: PlayKind): { steps: Step[]; endMs: number } {
  switch (kind) {
    case "greet":
      return { steps: [...both(0, pose("look")), ...both(1400, shot("waving")), ...both(3000, pose("idle"))], endMs: 3600 };
    case "chase":
      return {
        steps: [
          { at: 0, who: 0, act: pose("walk", 1) },
          { at: 400, who: 1, act: pose("walk", -1) },
          ...both(3200, shot("jumping")),
          ...both(4600, pose("walk", 0)),
        ],
        endMs: 7000,
      };
    case "highfive":
      return { steps: [...both(0, pose("walk", 1)), ...both(2400, shot("jumping")), ...both(3800, pose("walk", 0))], endMs: 6000 };
    case "nap":
      return {
        steps: [...both(0, pose("walk", 0.6)), ...both(1800, pose("sleep")), ...both(8000, pose("idle")), ...both(8000, shot("waking")), ...both(8800, pose("walk", 0))],
        endMs: 11_000,
      };
    case "showoff":
      return {
        steps: [{ at: 0, who: 1, act: pose("look") }, { at: 600, who: 0, act: shot("celebrating") }, { at: 2300, who: 1, act: shot("waving") }, { at: 3800, who: 1, act: pose("idle") }],
        endMs: 4800,
      };
  }
}

/** Starts an interaction; `rand` decides which pet takes the first role. */
export function startPlay(kind: PlayKind, keys: [string, string], now: number, rand: () => number): Play {
  const { steps, endMs } = stepsOf(kind);
  return { kind, keys: rand() < 0.5 ? keys : [keys[1], keys[0]], startedAt: now, steps, endMs, next: 0 };
}

/** The shots now due (and the play with them marked delivered). */
export function dueShots(play: Play, now: number): { shots: { key: string; name: string }[]; play: Play } {
  const shots: { key: string; name: string }[] = [];
  let next = play.next;
  while (next < play.steps.length && (play.steps[next]?.at ?? Infinity) + play.startedAt <= now) {
    const step = play.steps[next]!;
    if (step.act.kind === "shot") shots.push({ key: play.keys[step.who], name: step.act.name });
    next++;
  }
  return { shots, play: next === play.next ? play : { ...play, next } };
}

/** What pet `who` does at `now`: the latest pose step that has started. */
export function poseAt(play: Play, who: 0 | 1, now: number): Pose {
  let out: Pose = { mode: "idle", toward: 0 };
  for (const s of play.steps) if (s.who === who && s.at + play.startedAt <= now && s.act.kind === "pose") out = s.act.pose;
  return out;
}

export const isOver = (play: Play, now: number): boolean => now >= play.startedAt + play.endMs;

/** The pose of the pet `key` in this play (null when it is not in it), and its partner. */
export function roleOf(play: Play, key: string, now: number): { pose: Pose; partner: string } | null {
  const who = play.keys.indexOf(key);
  if (who < 0) return null;
  return { pose: poseAt(play, who as 0 | 1, now), partner: play.keys[who === 0 ? 1 : 0] };
}

/** Time to the next try at play: 15 to 45 seconds. */
export const nextPlayDelay = (rand: () => number): number => 15_000 + Math.floor(rand() * 30_000);

/** How long a pet must have been quiet to be drowsy enough for a nap. */
export const DROWSY_MS = 25_000;
/** How long after a pat or event a pet is left alone. */
export const SETTLE_MS = 3000;

export type Candidate = {
  key: string;
  /** The animation the arbiter shows and how it ends. */
  shown: string;
  kind: "loop" | "once" | "look";
  hidden: boolean;
  /** Real agent work: any loop, overlay, or pending prompt. */
  busy: boolean;
  /** A finished subagent waiting to be removed. */
  leaving: boolean;
  inPlay: boolean;
  /** Ms since the last event of any kind. */
  quietFor: number;
};

/** Whether a pet may start playing: awake, unoccupied, at P6 (idle, fidget, look) and settled. */
export function isFree(c: Candidate): boolean {
  if (c.hidden || c.busy || c.leaving || c.inPlay || c.quietFor < SETTLE_MS) return false;
  return c.shown === "idle" || c.shown === "fidget" || (c.kind === "look" && c.shown === "look");
}

/** Two free pets standing next to each other, picked at random; null when there are none. */
export function pickPair(cands: Candidate[], neighbors: (key: string) => string[], rand: () => number): { a: Candidate; b: Candidate } | null {
  const free = new Map(cands.filter(isFree).map((c) => [c.key, c]));
  const keys = [...free.keys()];
  // Walk the free pets from a random start until one has a free neighbour.
  const start = Math.floor(rand() * keys.length);
  for (let i = 0; i < keys.length; i++) {
    const a = free.get(keys[(start + i) % keys.length]!)!;
    const near = neighbors(a.key).flatMap((k) => (free.has(k) ? [free.get(k)!] : []));
    if (near.length > 0) return { a, b: near[Math.floor(rand() * near.length)]! };
  }
  return null;
}

const WEIGHTS: [PlayKind, number][] = [
  ["greet", 3],
  ["chase", 2],
  ["highfive", 2],
  ["showoff", 2],
  ["nap", 2],
];

/** A weighted pick; a nap only when both pets are drowsy. */
export function chooseKind(rand: () => number, drowsy: boolean): PlayKind {
  const pool = WEIGHTS.filter(([k]) => drowsy || k !== "nap");
  let roll = rand() * pool.reduce((sum, [, w]) => sum + w, 0);
  for (const [kind, w] of pool) {
    roll -= w;
    if (roll < 0) return kind;
  }
  return "greet";
}

/**
 * How far a pet can walk toward its partner, in px (signed): half the gap between the sprites,
 * at most `max`. The partner is `dx` px away (centre to centre), the sprites `size` px wide.
 */
export function reachPx(dx: number, size: number, max = 40): number {
  const room = Math.max(0, (Math.abs(dx) - size - 6) / 2);
  return Math.sign(dx) * Math.min(max, room);
}

/** The angle, clockwise from up in degrees, from a pet to its partner `dx`, `dy` px away. */
export function angleTo(dx: number, dy: number): number {
  const deg = (Math.atan2(dx, -dy) * 180) / Math.PI;
  return (deg + 360) % 360;
}
