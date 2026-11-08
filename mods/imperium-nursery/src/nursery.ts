// A nursery's state, no DOM: its pets (one arbiter each), fed by session events and snapshots
// through the routing in route.ts, or by hand for "My nursery". The view only reads it.

import type { SessionEvent, SessionSnapshot } from "@imperium/mod-sdk";

import { advance, current, dispatch, initial, nextWake } from "./arbiter.ts";
import type { Current, Env, PetEvent } from "./arbiter.ts";
import { isReviewer } from "./events.ts";
import { clip, hash } from "./pick.ts";
import { REMOVE_MS, ROOT, busyOf, busySubs, keyOf, repair, routeEvent } from "./route.ts";
import type { Mode } from "./route.ts";
import type { PetsArbiter } from "./types.ts";

export type Entry = {
  key: string;
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
  cache: { cur: Current; wake: number | null; from: number } | null;
};

export type Deps = {
  /** The arbiter's environment for a pet (its catalog, sleep and fidget settings). */
  envFor(petId: string | null): Env;
  /** The pet for something new or after the setting changed. */
  assign(who: { key: string; sessionId: string; root: boolean }): string | null;
};

type Init = { sessionId: string; agentId: string; root: boolean; label: string; petId?: string | null };

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

  readonly mode: Mode | "mine";
  private deps: Deps;

  constructor(mode: Mode | "mine", deps: Deps) {
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
    const petId = init.petId !== undefined ? init.petId : this.deps.assign({ key, sessionId: init.sessionId, root: init.root });
    const entry: Entry = {
      key,
      sessionId: init.sessionId,
      agentId: init.agentId,
      root: init.root,
      label: init.label,
      petId,
      arbiter: initial(now, hash(key)),
      removeAt: null,
      review: {},
      overlayEnds: {},
      order: this.serial++,
      cache: null,
    };
    entry.arbiter = dispatch(entry.arbiter, { type: "start" }, now, this.deps.envFor(petId));
    this.entries.set(key, entry);
    return entry;
  }

  /** Applies events to one pet; a reviewer's `working` loop is `review`, a stale unblock is dropped. */
  private send(entry: Entry, events: PetEvent[], now: number): void {
    const env = this.deps.envFor(entry.petId);
    for (const ev of events) {
      if (ev.type === "unblock" && entry.arbiter.sources[ev.source]?.waits[ev.id] === undefined) continue;
      const out = (ev.type === "loop" || ev.type === "ensure" || ev.type === "spawn") && ev.name === "working" && entry.review[ev.source] ? { ...ev, name: "review" } : ev;
      entry.arbiter = dispatch(entry.arbiter, out, now, env);
    }
    entry.cache = null;
  }

  private remove(key: string): void {
    this.entries.delete(key);
  }

  // --- the nurseries -------------------------------------------------------------------

  /** A session event: routed to the pets of the nursery it concerns. */
  event(e: SessionEvent, now: number): void {
    if (this.mode === "mine") return;
    if (this.mode === "session" && e.sessionId !== this.target()) return;
    let changed = false;
    for (const step of routeEvent(this.mode, e)) {
      let entry = this.entries.get(step.key);
      if (!entry) {
        if (!step.create || (this.retired.has(step.key) && !step.root)) continue;
        const label = step.root ? (this.titles.get(step.sessionId) ?? "Main agent") : clip(step.spawn?.brief || step.agentId, 60);
        entry = this.create(step.key, { sessionId: step.sessionId, agentId: step.agentId, root: step.root, label }, now);
      }
      if (step.spawn) {
        const src = this.mode === "session" ? "main" : step.spawn.agentId;
        entry.review[src] = isReviewer("", step.spawn.brief);
        if (this.mode === "session") entry.label = clip(step.spawn.brief || step.spawn.agentId, 60);
      }
      if (step.removeAfterMs !== undefined) entry.removeAt = now + step.removeAfterMs;
      if (step.overlay) entry.overlayEnds[step.overlay.source] = now + step.overlay.ms;
      this.send(entry, step.events, now);
      changed = true;
    }
    if (changed) this.onChange();
  }

  /** The sessions and selection changed (or were first read): seed new pets, repair the rest. */
  snapshots(sessions: SessionSnapshot[], selected: string | null, now: number): void {
    if (this.mode === "mine") return;
    this.selected = selected;
    this.sessionIds = new Set(sessions.map((s) => s.sessionId));
    for (const s of sessions) this.titles.set(s.sessionId, s.title);
    if (this.mode === "global") this.globalSnapshots(sessions, now);
    else this.sessionSnapshots(sessions, now);
    this.onChange();
  }

  private globalSnapshots(sessions: SessionSnapshot[], now: number): void {
    const seen = new Set<string>();
    sessions.forEach((s, i) => {
      const key = keyOf("global", s.sessionId, ROOT);
      seen.add(key);
      const entry = this.entries.get(key) ?? this.create(key, { sessionId: s.sessionId, agentId: ROOT, root: true, label: s.title }, now);
      entry.label = s.title;
      entry.order = i;
      const ids = new Set<string>();
      for (const ag of s.agents) {
        const root = ag.agentId === ROOT || ag.parentId === null;
        const src = root ? "main" : ag.agentId;
        ids.add(src);
        if (!root) entry.review[src] = isReviewer("", ag.brief);
        this.send(entry, repair(entry.arbiter, src, ag.status, !root), now);
      }
      // A subagent the app no longer lists is over.
      for (const src of Object.keys(entry.arbiter.sources)) if (src !== "main" && !ids.has(src)) this.send(entry, [{ type: "end", source: src }], now);
    });
    for (const key of [...this.entries.keys()]) if (!seen.has(key)) this.remove(key);
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
        entry = this.create(key, { sessionId: session.sessionId, agentId: ag.agentId, root, label: root ? session.title : clip(ag.brief || ag.agentId, 60) }, now);
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

  /** Puts a pet in (My nursery). */
  add(uid: string, petId: string, name: string, now: number): void {
    if (this.entries.has(uid)) return;
    this.create(uid, { sessionId: "", agentId: uid, root: false, label: name, petId }, now);
    this.onChange();
  }

  /** Takes a pet out. */
  take(key: string): void {
    this.remove(key);
    this.onChange();
  }

  /** Pats a pet. */
  pat(key: string, now: number): void {
    const entry = this.entries.get(key);
    if (!entry) return;
    this.send(entry, [{ type: "shot", name: "petted", source: "main" }], now);
  }

  // --- reading --------------------------------------------------------------------------

  /** Changes of their own: pets leave, overlays end. True when the membership changed. */
  tick(now: number): boolean {
    let changed = false;
    for (const entry of [...this.entries.values()]) {
      if (entry.removeAt !== null && entry.removeAt <= now) {
        this.retired.add(entry.key);
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

  /** The pet with the current `petId`s redrawn: after the setting or the installed pets changed. */
  reassign(now: number): void {
    for (const entry of this.entries.values()) {
      if (this.mode === "mine") continue;
      entry.petId = this.deps.assign({ key: entry.key, sessionId: entry.sessionId, root: entry.root });
      entry.arbiter = advance(entry.arbiter, now, this.deps.envFor(entry.petId));
      entry.cache = null;
    }
    this.onChange();
  }

  /** What the pet shows at `now`, cached until its next change of its own. */
  view(entry: Entry, now: number): Current {
    const hit = entry.cache;
    if (hit && now >= hit.from && (hit.wake === null || now < hit.wake)) return hit.cur;
    const env = this.deps.envFor(entry.petId);
    const cur = current(entry.arbiter, now, env);
    entry.cache = { cur, wake: nextWake(entry.arbiter, now, env), from: now };
    return cur;
  }

  /** The pets in display order: session nursery by spawn order, global with busy sessions first. */
  sorted(): Entry[] {
    const rank = (e: Entry): number => {
      if (this.mode !== "global") return 0;
      const b = busyOf(e.arbiter);
      return b === "waiting" ? 0 : b === "working" ? 1 : 2;
    };
    return [...this.entries.values()].sort((a, b) => rank(a) - rank(b) || a.order - b.order);
  }

  /** The counts the summary line reports. */
  summary(): { pets: number; busy: number; subagents: number } {
    let busy = 0;
    let subagents = 0;
    for (const e of this.entries.values()) {
      if (this.mode === "global") {
        if (busyOf(e.arbiter)) busy++;
        subagents += busySubs(e.arbiter);
      } else if (!e.root) {
        // Session nursery: a subagent's own pet is busy while its main source is.
        if (busyOf(e.arbiter)) subagents++;
      }
    }
    return { pets: this.entries.size, busy, subagents };
  }
}

