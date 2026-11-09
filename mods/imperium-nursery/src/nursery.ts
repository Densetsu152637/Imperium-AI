// A nursery's state, no DOM: its pets (one arbiter each), fed by session events and snapshots
// through the routing in route.ts, or by hand for the extra pets. The view only reads it.
//
// Two modes. `session`: the selected session's agents, one pet each. `nursery`: your pet (always
// there, hears every session), one pet per unarchived session, and the extra pets you put in.

import type { SessionEvent, SessionSnapshot } from "@imperium/mod-sdk";

import { advance, current, dispatch, initial, nextWake } from "./arbiter.ts";
import type { Current, Env, PetEvent } from "./arbiter.ts";
import { isReviewer } from "./events.ts";
import { clip, hash } from "./pick.ts";
import { DROWSY_MS, GATHER_MS, MEET_DIST, beginScript, chooseKind, dueShots, isOver, nextPlayDelay, passingPair, pickPair, roleOf, startPlay } from "./play.ts";
import type { Candidate, Play, Pose } from "./play.ts";
import { REMOVE_MS, ROOT, busyOf, busySubs, keyOf, repair, routeEvent } from "./route.ts";
import { distance } from "./space.ts";
import type { Pos } from "./space.ts";
import type { PetsArbiter } from "./types.ts";

/** What a pet in the nursery is: yours, a session's, or one you put in by hand. */
export type Kind = "resident" | "session" | "extra" | "agent";

export type Entry = {
  key: string;
  kind: Kind;
  /** A session pet: the pet its environment names, or null. */
  envPet: string | null;
  sessionId: string;
  agentId: string;
  root: boolean;
  label: string;
  petId: string | null;
  arbiter: PetsArbiter;
  /** When a finished subagent's pet leaves (session nursery). */
  removeAt: number | null;
  /** Sources that are reviewers: their `working` loop is drawn as `review`. */
  review: Record<string, boolean>;
  /** When an `eating` overlay ends, by source. */
  overlayEnds: Record<string, number>;
  /** Display order within the nursery. */
  order: number;
  /** When the pet arrived, for depth: older pets stand behind newer ones. A session pet's is the
   * session's creation time; your pet is the oldest of all. */
  born: number;
  /** Creation order here, to break ties in `born`. */
  seq: number;
  cache: { cur: Current; wake: number | null; from: number } | null;
};

export type Deps = {
  /** The arbiter's environment for a pet (its catalog, sleep and fidget settings). */
  envFor(petId: string | null, kind: Kind): Env;
  /** The pet for something new or after the setting changed. */
  assign(who: { key: string; sessionId: string; root: boolean; envPet: string | null }): string | null;
};

type Init = {
  sessionId: string;
  agentId: string;
  root: boolean;
  label: string;
  petId?: string | null;
  kind?: Kind;
  envPet?: string | null;
  /** When it arrived (default: now). */
  born?: number;
};

/** Before anything else: your pet and a session's own pet stand behind every newer one. */
export const FIRST = Number.NEGATIVE_INFINITY;

/** The key of your pet, and of a session's pet, in the nursery. */
export const RESIDENT = "resident";
export const sessionKey = (sessionId: string): string => `session:${sessionId}`;
/** How long an archived session's pet takes to walk out. */
export const LEAVE_MS = 4000;

/** A source id for one agent of one session, on your pet (which hears them all). */
const scoped = (sessionId: string, source: string): string => `${sessionId}:${source}`;

/** `events` heard by your pet: every source belongs to its session. */
function scopeEvents(events: PetEvent[], sessionId: string): PetEvent[] {
  return events.map((ev) => ("source" in ev && ev.source !== null ? { ...ev, source: scoped(sessionId, ev.source) } : ev));
}

export class Nursery {
  readonly entries = new Map<string, Entry>();
  /** Pinned session (session nursery); null follows the app's selection. */
  pinned: string | null = null;
  private selected: string | null = null;
  private sessionIds = new Set<string>();
  private titles = new Map<string, string>();
  private retired = new Set<string>();
  private serial = 0;
  onChange: () => void = () => {};
  /** The `play` setting: free pets now and then play with each other. */
  playEnabled = true;
  /** The interaction in progress, if any (one at a time per nursery). */
  playing: Play | null = null;
  /** Randomness for play; tests inject a seeded one. */
  rand: () => number = Math.random;
  /** Where pet `key` stands in the room; the view replaces this with the real positions. */
  at: (key: string) => Pos | null = (key) => {
    const i = this.sorted().findIndex((e) => e.key === key);
    return i < 0 ? null : { x: i * 100, y: 0 };
  };
  // When play may next start; 0 means "not set yet" (set from the next tick's clock).
  private nextPlayAt = 0;
  private nextPassAt = 0;
  private clock = 0;

  readonly mode: "session" | "nursery";
  private deps: Deps;
  /** Sessions that are listed and not archived (nursery). */
  private liveSessions = new Set<string>();

  constructor(mode: "session" | "nursery", deps: Deps) {
    this.mode = mode;
    this.deps = deps;
  }

  /** The session the session nursery shows now, or null. */
  target(): string | null {
    const pin = this.pinned;
    if (pin !== null && this.sessionIds.has(pin)) return pin;
    return this.selected;
  }

  private create(key: string, init: Init, now: number): Entry {
    const envPet = init.envPet ?? null;
    const petId = init.petId !== undefined ? init.petId : this.deps.assign({ key, sessionId: init.sessionId, root: init.root, envPet });
    const kind = init.kind ?? "agent";
    const entry: Entry = {
      key,
      kind,
      envPet,
      sessionId: init.sessionId,
      agentId: init.agentId,
      root: init.root,
      label: init.label,
      petId,
      arbiter: initial(now, hash(key)),
      removeAt: null,
      review: {},
      overlayEnds: {},
      order: this.serial,
      born: init.born ?? now,
      seq: this.serial++,
      cache: null,
    };
    entry.arbiter = dispatch(entry.arbiter, { type: "start" }, now, this.deps.envFor(petId, kind));
    this.entries.set(key, entry);
    return entry;
  }

  /** Applies events to one pet; a reviewer's `working` loop is `review`, a stale unblock is dropped. */
  private send(entry: Entry, events: PetEvent[], now: number): void {
    const env = this.deps.envFor(entry.petId, entry.kind);
    for (const ev of events) {
      if (ev.type === "unblock" && entry.arbiter.sources[ev.source]?.waits[ev.id] === undefined) continue;
      const out = (ev.type === "loop" || ev.type === "ensure" || ev.type === "spawn") && ev.name === "working" && entry.review[ev.source] ? { ...ev, name: "review" } : ev;
      entry.arbiter = dispatch(entry.arbiter, out, now, env);
    }
    entry.cache = null;
  }

  private remove(key: string): void {
    this.endPlayFor(key);
    this.entries.delete(key);
  }

  /** A real event (or a pat, or the pet leaving) ends any play the pet is in, for both pets. */
  private endPlayFor(key: string): void {
    if (!this.playing?.keys.includes(key)) return;
    this.playing = null;
    this.nextPlayAt = 0;
  }

  // --- the nurseries -------------------------------------------------------------------

  /** A session event: routed to the pets it concerns. */
  event(e: SessionEvent, now: number): void {
    if (this.mode === "session") {
      if (e.sessionId !== this.target()) return;
      this.sessionEvent(e, now);
      return;
    }
    let changed = false;
    for (const step of routeEvent("global", e)) {
      const own = this.entries.get(sessionKey(step.sessionId));
      const resident = this.entries.get(RESIDENT);
      const child = step.spawn?.agentId;
      const reviewer = isReviewer("", step.spawn?.brief ?? "");
      if (own && this.liveSessions.has(step.sessionId)) {
        if (child) own.review[child] = reviewer;
        if (step.overlay) own.overlayEnds[step.overlay.source] = now + step.overlay.ms;
        this.endPlayFor(own.key);
        this.send(own, step.events, now);
        changed = true;
      }
      if (resident && (this.liveSessions.has(step.sessionId) || !this.sessionIds.has(step.sessionId))) {
        if (child) resident.review[scoped(step.sessionId, child)] = reviewer;
        if (step.overlay) resident.overlayEnds[scoped(step.sessionId, step.overlay.source)] = now + step.overlay.ms;
        this.endPlayFor(resident.key);
        this.send(resident, scopeEvents(step.events, step.sessionId), now);
        changed = true;
      }
    }
    if (changed) this.onChange();
  }

  private sessionEvent(e: SessionEvent, now: number): void {
    let changed = false;
    for (const step of routeEvent("session", e)) {
      let entry = this.entries.get(step.key);
      if (!entry) {
        if (!step.create || (this.retired.has(step.key) && !step.root)) continue;
        const label = step.root ? (this.titles.get(step.sessionId) ?? "Main agent") : clip(step.spawn?.brief || step.agentId, 60);
        entry = this.create(step.key, { sessionId: step.sessionId, agentId: step.agentId, root: step.root, label }, now);
      }
      if (step.spawn) {
        entry.review.main = isReviewer("", step.spawn.brief);
        entry.label = clip(step.spawn.brief || step.spawn.agentId, 60);
      }
      if (step.removeAfterMs !== undefined) entry.removeAt = now + step.removeAfterMs;
      if (step.overlay) entry.overlayEnds[step.overlay.source] = now + step.overlay.ms;
      this.endPlayFor(entry.key);
      this.send(entry, step.events, now);
      changed = true;
    }
    if (changed) this.onChange();
  }

  /** The sessions and selection changed (or were first read): seed new pets, repair the rest. */
  snapshots(sessions: SessionSnapshot[], selected: string | null, now: number): void {
    this.selected = selected;
    this.sessionIds = new Set(sessions.map((s) => s.sessionId));
    for (const s of sessions) this.titles.set(s.sessionId, s.title);
    if (this.mode === "nursery") this.nurserySnapshots(sessions, now);
    else this.sessionSnapshots(sessions, now);
    this.onChange();
  }

  /** Your pet follows every unarchived session; each of them has a pet of its own. */
  private nurserySnapshots(sessions: SessionSnapshot[], now: number): void {
    const live = sessions.filter((s) => !s.archived);
    this.liveSessions = new Set(live.map((s) => s.sessionId));
    const resident = this.entries.get(RESIDENT);
    const heard = new Set<string>();
    live.forEach((s, i) => {
      const key = sessionKey(s.sessionId);
      let entry = this.entries.get(key);
      if (!entry) entry = this.create(key, { sessionId: s.sessionId, agentId: ROOT, root: true, label: s.title, kind: "session", envPet: s.pet, born: s.createdAt ?? now }, now);
      if (s.createdAt !== null) entry.born = s.createdAt;
      entry.removeAt = null;
      entry.label = s.title;
      entry.order = i;
      if (entry.envPet !== s.pet) {
        entry.envPet = s.pet;
        this.assignTo(entry, now);
      }
      const ids = new Set<string>();
      for (const ag of s.agents) {
        const root = ag.agentId === ROOT || ag.parentId === null;
        const src = root ? "main" : ag.agentId;
        ids.add(src);
        if (!root) entry.review[src] = isReviewer("", ag.brief);
        this.send(entry, repair(entry.arbiter, src, ag.status, !root), now);
        if (resident) {
          const mine = scoped(s.sessionId, src);
          heard.add(mine);
          if (!root) resident.review[mine] = isReviewer("", ag.brief);
          this.send(resident, repair(resident.arbiter, mine, ag.status, !root), now);
        }
      }
      // A subagent the app no longer lists is over.
      for (const src of Object.keys(entry.arbiter.sources)) if (src !== "main" && !ids.has(src)) this.send(entry, [{ type: "end", source: src }], now);
    });
    if (resident) {
      // Whatever the app no longer lists (a subagent, an archived or deleted session) is over.
      for (const src of Object.keys(resident.arbiter.sources)) if (src !== "main" && !heard.has(src)) this.send(resident, [{ type: "end", source: src }], now);
    }
    // An archived or deleted session's pet walks out; if it comes back in time it simply stays.
    for (const entry of this.entries.values()) {
      if (entry.kind === "session" && !this.liveSessions.has(entry.sessionId)) entry.removeAt ??= now + LEAVE_MS;
    }
  }

  private sessionSnapshots(sessions: SessionSnapshot[], now: number): void {
    const target = this.target();
    const session = sessions.find((s) => s.sessionId === target);
    const seen = new Set<string>();
    session?.agents.forEach((ag, i) => {
      const root = ag.agentId === ROOT || ag.parentId === null;
      const key = keyOf("session", session.sessionId, ag.agentId);
      const over = !root && (ag.status === "finished" || ag.status === "failed");
      let entry = this.entries.get(key);
      if (!entry) {
        if (over) return;
        entry = this.create(key, { sessionId: session.sessionId, agentId: ag.agentId, root, label: root ? session.title : clip(ag.brief || ag.agentId, 60), born: root ? FIRST : now }, now);
      }
      seen.add(key);
      entry.order = i;
      if (root) entry.label = session.title;
      else entry.review.main = isReviewer("", ag.brief);
      if (over) entry.removeAt ??= now + REMOVE_MS;
      else entry.removeAt = null;
      this.send(entry, repair(entry.arbiter, "main", ag.status, false), now);
    });
    for (const [key, entry] of [...this.entries]) {
      if (!seen.has(key) && !(entry.sessionId === target && entry.removeAt !== null)) this.remove(key);
    }
  }

  // --- my nursery -----------------------------------------------------------------------

  /** Puts an extra pet in. */
  add(uid: string, petId: string, name: string, now: number): void {
    if (this.entries.has(uid)) return;
    this.create(uid, { sessionId: "", agentId: uid, root: false, label: name, petId, kind: "extra" }, now);
    this.onChange();
  }

  /** Takes an extra pet out; your pet and the session pets cannot be taken out. */
  take(key: string): void {
    if (this.entries.get(key)?.kind !== "extra") return;
    this.remove(key);
    this.onChange();
  }

  /** Sets your pet (it is always in the nursery): created the first time, then changed in place. */
  setResident(petId: string | null, label: string, now: number): void {
    const entry = this.entries.get(RESIDENT);
    if (!entry) {
      if (petId === null) return;
      const created = this.create(RESIDENT, { sessionId: "", agentId: RESIDENT, root: false, label, petId, kind: "resident", born: FIRST }, now);
      created.order = -1;
    } else {
      entry.petId = petId;
      entry.label = label;
      entry.arbiter = advance(entry.arbiter, now, this.deps.envFor(petId, "resident"));
      entry.cache = null;
    }
    this.onChange();
  }

  /** Pats a pet. */
  pat(key: string, now: number): void {
    const entry = this.entries.get(key);
    if (!entry) return;
    this.endPlayFor(key);
    this.send(entry, [{ type: "shot", name: "petted", source: "main" }], now);
  }

  // --- reading --------------------------------------------------------------------------

  /** What pet `key` is doing in the play in progress: its pose and its partner's key. */
  playInfo(key: string): { pose: Pose; partner: string } | null {
    return this.playing ? roleOf(this.playing, key, this.clock) : null;
  }

  /**
   * Runs play: delivers the shots now due, ends it when it is over or either pet got real work,
   * and now and then starts a new one between two free pets (or a greeting between two that pass close). `reduced` (reduced motion)
   * or `playEnabled` false stops it altogether.
   */
  tickPlay(now: number, reduced: boolean): void {
    this.clock = now;
    if (!this.playEnabled || reduced) {
      this.playing = null;
      this.nextPlayAt = 0;
      return;
    }
    const playing = this.playing;
    if (playing) {
      const pair = playing.keys.map((k) => this.entries.get(k));
      if (pair.some((e) => !e || e.removeAt !== null || busyOf(e.arbiter))) {
        this.playing = null;
        this.nextPlayAt = 0;
        return;
      }
      if (playing.gather !== null) {
        const [a, b] = [this.at(playing.keys[0]), this.at(playing.keys[1])];
        if (!a || !b || distance(a, b) <= MEET_DIST || now >= playing.gather) this.playing = beginScript(playing, now);
        return;
      }
      const due = dueShots(playing, now);
      this.playing = due.play;
      for (const { key, name } of due.shots) {
        const entry = this.entries.get(key);
        if (entry) this.send(entry, [{ type: "shot", name, source: "main" }], now);
      }
      if (isOver(due.play, now)) {
        this.playing = null;
        this.nextPlayAt = now + nextPlayDelay(this.rand);
      }
      return;
    }
    if (this.nextPlayAt === 0) {
      this.nextPlayAt = now + nextPlayDelay(this.rand);
      return;
    }
    const candidates = (): Candidate[] =>
      [...this.entries.values()].map((e) => {
        const cur = this.view(e, now);
        return { key: e.key, shown: cur.name, kind: cur.kind, hidden: cur.isHidden, busy: busyOf(e.arbiter) !== null, leaving: e.removeAt !== null, inPlay: false, quietFor: now - e.arbiter.activeAt };
      });
    if (now < this.nextPlayAt) {
      // Two free pets that happen to pass close may greet each other.
      if (now >= this.nextPassAt) {
        this.nextPassAt = now + 500;
        const near = passingPair(candidates(), this.at, this.rand);
        if (near && this.rand() < 0.1) {
          this.playing = startPlay("greet", [near.a.key, near.b.key], now, this.rand);
          this.nextPlayAt = 0;
          this.tickPlay(now, reduced);
        }
      }
      return;
    }
    const pair = pickPair(candidates(), this.at, this.rand);
    if (!pair) {
      this.nextPlayAt = now + 5000;
      return;
    }
    const drowsy = pair.a.quietFor >= DROWSY_MS && pair.b.quietFor >= DROWSY_MS;
    const [pa, pb] = [this.at(pair.a.key), this.at(pair.b.key)];
    const gather = pa && pb && distance(pa, pb) > MEET_DIST ? GATHER_MS : 0;
    this.playing = startPlay(chooseKind(this.rand, drowsy), [pair.a.key, pair.b.key], now, this.rand, gather);
    this.nextPlayAt = 0;
    this.tickPlay(now, reduced);
  }

  /** Changes of their own: pets leave, overlays end. True when the membership changed. */
  tick(now: number): boolean {
    let changed = false;
    for (const entry of [...this.entries.values()]) {
      if (entry.removeAt !== null && entry.removeAt <= now) {
        if (entry.kind === "agent") this.retired.add(entry.key);
        this.remove(entry.key);
        changed = true;
        continue;
      }
      for (const [src, at] of Object.entries(entry.overlayEnds)) {
        if (at > now) continue;
        delete entry.overlayEnds[src];
        this.send(entry, [{ type: "overlay", source: src, name: null }], now);
      }
    }
    if (changed) this.onChange();
    return changed;
  }

  /** The pets with their `petId`s chosen again: after the setting or the installed pets changed. */
  reassign(now: number): void {
    for (const entry of this.entries.values()) {
      if (entry.kind === "extra" || entry.kind === "resident") continue;
      this.assignTo(entry, now);
    }
    this.onChange();
  }

  private assignTo(entry: Entry, now: number): void {
    entry.petId = this.deps.assign({ key: entry.key, sessionId: entry.sessionId, root: entry.root, envPet: entry.envPet });
    entry.arbiter = advance(entry.arbiter, now, this.deps.envFor(entry.petId, entry.kind));
    entry.cache = null;
  }

  /** What the pet shows at `now`, cached until its next change of its own. */
  view(entry: Entry, now: number): Current {
    const hit = entry.cache;
    if (hit && now >= hit.from && (hit.wake === null || now < hit.wake)) return hit.cur;
    const env = this.deps.envFor(entry.petId, entry.kind);
    const cur = current(entry.arbiter, now, env);
    entry.cache = { cur, wake: nextWake(entry.arbiter, now, env), from: now };
    return cur;
  }

  /** The pets in display order: yours first, then the sessions' (busy ones first), then the extras. */
  sorted(): Entry[] {
    const rank = (e: Entry): number => {
      if (e.kind === "resident") return -1;
      if (e.kind === "extra") return 3;
      if (this.mode === "session") return 0;
      const b = busyOf(e.arbiter);
      return b === "waiting" ? 0 : b === "working" ? 1 : 2;
    };
    return [...this.entries.values()].sort((a, b) => rank(a) - rank(b) || a.order - b.order);
  }

  /** The counts the summary line reports. */
  summary(): { pets: number; sessions: number; busy: number; subagents: number } {
    let busy = 0;
    let subagents = 0;
    let sessions = 0;
    for (const e of this.entries.values()) {
      if (this.mode === "nursery") {
        if (e.kind !== "session" || e.removeAt !== null) continue;
        sessions++;
        if (busyOf(e.arbiter)) busy++;
        subagents += busySubs(e.arbiter);
      } else if (!e.root) {
        // Session nursery: a subagent's own pet is busy while its main source is.
        if (busyOf(e.arbiter)) subagents++;
      }
    }
    return { pets: this.entries.size, sessions, busy, subagents };
  }
}
