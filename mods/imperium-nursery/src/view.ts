// The nursery's DOM: one shared room that pets roam, and the one shared loop that animates them.
// Every string from outside (session titles, agent briefs, nicknames) goes in through
// `textContent`, never markup.

import { lookIndex } from "./arbiter.ts";
import type { LoadedPet, PetLibrary } from "./library.ts";
import type { Entry, Kind, Nursery } from "./nursery.ts";
import { resolveAnimation } from "./pet.ts";
import { angleTo } from "./play.ts";
import { busyOf, busySubs } from "./route.ts";
import { fitSize, frameAt } from "./render.ts";
import { PLAY_SPEED, SPEED, boundsOf, clampTo, edgeTarget, newRoamer, playTarget, reducedLayout, roam, roomHeight, separate, spawnPoint, stepToward } from "./space.ts";
import type { Body, Bounds, Metrics, Pos, Roamer } from "./space.ts";

/** Width of a pet's box (the label's width) and the height of its label line, in px. */
const LABEL_W = 96;
const LABEL_H = 18;
/** How long a finished pet takes to fade out before it is removed, in ms. */
const FADE_MS = 2500;

export const CSS = `
:root { color-scheme: light dark; }
body { margin: 0; padding: 0; }
.nursery { display: flex; flex-direction: column; gap: var(--space, 8px); font-size: 13px; min-height: 100vh; box-sizing: border-box; padding: var(--space, 8px); }
.bar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.bar label { color: var(--muted); }
.bar select, .bar input, .bar button { font: inherit; color: var(--fg); background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius, 6px); padding: 3px 6px; max-width: 100%; }
.bar button { cursor: pointer; }
.bar button:hover:not(:disabled) { border-color: var(--accent); }
.bar .more { flex-basis: 100%; color: var(--muted); font-size: 11px; }
.bar .more[open] { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.bar .more summary { cursor: pointer; }
.summary { color: var(--muted); }
.empty { color: var(--muted); padding: 4px 0; text-align: center; }
.empty:empty { display: none; }
.room { position: relative; flex: 1 1 auto; min-height: 160px; overflow: hidden; border-radius: var(--radius, 6px); background: linear-gradient(to bottom, transparent 0%, color-mix(in srgb, var(--panel) 80%, transparent) 100%); border: 1px solid var(--border); }
.pet { position: absolute; left: 0; top: 0; display: flex; flex-direction: column; align-items: center; will-change: transform; }
.hit { display: flex; flex-direction: column; align-items: center; padding: 0; margin: 0; border: 0; background: none; color: inherit; font: inherit; cursor: pointer; border-radius: 6px; }
.hit:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
.stage { position: relative; display: flex; align-items: flex-end; justify-content: center; width: 100%; }
.shadow { position: absolute; left: 50%; bottom: -3px; width: 60%; height: 8px; margin-left: -30%; border-radius: 50%; background: rgba(0, 0, 0, 0.2); pointer-events: none; }
.pet[data-state="working"] .shadow { background: var(--accent); opacity: 0.5; }
.pet[data-state="waiting"] .shadow { background: var(--danger); opacity: 0.6; }
.sprite { image-rendering: pixelated; background-repeat: no-repeat; flex: none; position: relative; }
.sprite.missing { border: 1px dashed var(--border); box-sizing: border-box; }
.label { max-width: 100%; margin-top: 4px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: center; color: var(--muted); font-size: 11px; line-height: 14px; }
.note { color: var(--muted); font-size: 10px; }
.note:empty { display: none; }
.badge { position: absolute; top: 0; right: 0; min-width: 16px; padding: 0 4px; border-radius: 8px; background: var(--accent); color: var(--bg); font-size: 11px; line-height: 16px; text-align: center; }
.badge[hidden] { display: none; }
.take { position: absolute; top: 0; left: 0; width: 20px; height: 20px; padding: 0; line-height: 1; border: 0; background: var(--panel); color: var(--muted); cursor: pointer; border-radius: 4px; opacity: 0; }
.pet:hover .take, .pet:focus-within .take, .take:focus-visible { opacity: 1; }
.take:hover { color: var(--danger); }
`;

export type RoomOptions = {
  /** Sprite size in px (a pet is fitted inside a size x size square). */
  size: number;
  /** Extra pets get a button that takes them out. */
  takeable: boolean;
  /** Show the count of busy subagents on session pets. */
  badges: boolean;
  onPat(key: string): void;
  onTake?(key: string): void;
};

type Cell = {
  el: HTMLElement;
  sprite: HTMLElement;
  label: HTMLElement;
  note: HTMLElement;
  badge: HTMLElement;
  /** Where the pet stands (feet) and where it is walking. */
  r: Roamer;
  placed: boolean;
  /** In a play on the last frame. */
  playing: boolean;
  /** The walk direction now (0 standing) and the way it last faced. */
  dir: number;
  facing: number;
  walkSince: number;
  url: string;
  pos: string;
  sized: string;
  text: string;
  state: string;
  xform: string;
  z: string;
  fade: string;
};

type Moved = { anim: string | null; name: string; since: number; look: number | null };

const make = (tag: string, cls: string, text = ""): HTMLElement => {
  const el = document.createElement(tag);
  el.className = cls;
  if (text) el.textContent = text;
  return el;
};

export class Room {
  readonly el = make("div", "room");
  private cells = new Map<string, Cell>();
  private order: string[] = [];
  private last = 0;
  private width = 0;
  private height = 0;
  private observer: ResizeObserver | null = null;

  private nursery: Nursery;
  private lib: PetLibrary;
  private opts: RoomOptions;

  constructor(nursery: Nursery, lib: PetLibrary, opts: RoomOptions) {
    this.nursery = nursery;
    this.lib = lib;
    this.opts = opts;
    if (typeof ResizeObserver !== "undefined") {
      this.observer = new ResizeObserver((entries) => {
        const box = entries[entries.length - 1]?.contentRect;
        if (box) this.resize(box.width, box.height);
      });
      this.observer.observe(this.el);
    }
  }

  private get metrics(): Metrics {
    return { half: LABEL_W / 2, above: this.opts.size, below: LABEL_H, pad: 4 };
  }

  private bounds(): Bounds {
    return boundsOf({ width: this.width, height: this.height }, this.metrics);
  }

  /** The room changed size: it grows when crowded, and pets are put back inside. */
  private resize(width: number, height: number): void {
    this.width = width;
    this.height = height;
    this.fit();
    const b = this.bounds();
    for (const [key, cell] of this.cells) {
      if (this.nursery.entries.get(key)?.removeAt != null) continue;
      const p = clampTo(b, cell.r);
      const t = clampTo(b, { x: cell.r.tx, y: cell.r.ty });
      cell.r = { ...cell.r, x: p.x, y: p.y, tx: t.x, ty: t.y };
    }
  }

  /** A room with many pets is tall enough for the least area each; the panel scrolls only past that. */
  private fit(): void {
    const px = `${roomHeight(this.cells.size, this.width)}px`;
    if (this.el.style.minHeight !== px) this.el.style.minHeight = px;
  }

  /** Where pet `key` stands, or null when it has no place yet. */
  pos(key: string): Pos | null {
    const cell = this.cells.get(key);
    return cell?.placed ? { x: cell.r.x, y: cell.r.y } : null;
  }

  /** Makes the pets in the room match the nursery's pets. */
  sync(): void {
    const entries = this.nursery.sorted();
    const keep = new Set(entries.map((e) => e.key));
    for (const [key, cell] of this.cells) {
      if (keep.has(key)) continue;
      cell.el.remove();
      this.cells.delete(key);
    }
    this.order = entries.map((e) => e.key);
    for (const entry of entries) {
      let cell = this.cells.get(entry.key);
      if (!cell) {
        cell = this.build(entry.key, entry.kind);
        this.cells.set(entry.key, cell);
        this.el.append(cell.el);
      }
      if (cell.text !== entry.label) {
        cell.text = entry.label;
        cell.label.textContent = entry.label;
        cell.el.title = entry.label;
        cell.el.querySelector("button")?.setAttribute("aria-label", `Pat ${entry.label}`);
      }
    }
    if (this.width === 0) this.resize(this.el.clientWidth, this.el.clientHeight);
    else this.fit();
  }

  private build(key: string, kind: Kind): Cell {
    const el = make("div", "pet");
    el.style.width = `${LABEL_W}px`;
    const hit = make("button", "hit") as HTMLButtonElement;
    hit.type = "button";
    hit.style.width = "100%";
    hit.addEventListener("click", () => this.opts.onPat(key));
    const stage = make("div", "stage");
    stage.style.height = `${this.opts.size}px`;
    const shadow = make("div", "shadow");
    const sprite = make("div", "sprite");
    stage.append(shadow, sprite);
    const label = make("div", "label");
    label.style.width = "100%";
    const note = make("div", "note");
    hit.append(stage, label, note);
    const badge = make("div", "badge");
    badge.hidden = true;
    el.append(hit, badge);
    if (this.opts.takeable && kind === "extra") {
      const take = make("button", "take", "×") as HTMLButtonElement;
      take.type = "button";
      take.title = "Take out of the nursery";
      take.setAttribute("aria-label", "Take out of the nursery");
      take.addEventListener("click", () => this.opts.onTake?.(key));
      el.append(take);
    }
    return {
      el,
      sprite,
      label,
      note,
      badge,
      r: newRoamer({ x: 0, y: 0 }, 0, Math.random),
      placed: false,
      playing: false,
      dir: 0,
      facing: 1,
      walkSince: 0,
      url: "",
      pos: "",
      sized: "",
      text: "\0",
      state: "",
      xform: "",
      z: "",
      fade: "",
    };
  }

  /** Moves and draws every pet at `now` (ms since epoch). */
  paint(now: number, reduced: boolean): void {
    const dt = this.last ? Math.min(250, Math.max(0, now - this.last)) : 0;
    this.last = now;
    // The observer reports size changes too; reading here keeps a hidden or just-resized room right.
    const [w, h] = [this.el.clientWidth, this.el.clientHeight];
    if (w !== this.width || h !== this.height) this.resize(w, h);
    const b = this.bounds();
    const live = this.order.flatMap((key) => {
      const cell = this.cells.get(key);
      const entry = this.nursery.entries.get(key);
      return cell && entry ? [{ key, cell, entry }] : [];
    });
    if (this.width > 0) this.place(live, b, reduced, now);
    const moved = new Map<string, Moved>();
    const bodies: Body[] = [];
    const group = this.nursery.playing ? `${this.nursery.playing.keys[0]}|${this.nursery.playing.keys[1]}` : null;
    for (const { key, cell, entry } of live) {
      const pet = this.lib.get(entry.petId);
      const cur = this.nursery.view(entry, now);
      const idle = cur.name === "idle" && cur.kind === "loop";
      const leaving = entry.removeAt !== null;
      if (reduced || !pet || !cell.placed || this.width === 0) {
        cell.playing = false;
        cell.dir = 0;
      } else {
        moved.set(key, this.move(cell, entry, pet, idle, now, dt, b));
      }
      if (!reduced && cell.placed && !leaving) bodies.push({ key, x: cell.r.x, y: cell.r.y, movable: idle && !cell.playing, group: this.nursery.playing?.keys.includes(key) ? group : null });
    }
    if (bodies.length > 1) {
      const out = separate(bodies, dt);
      bodies.forEach((body, i) => {
        const cell = this.cells.get(body.key)!;
        const p = body.movable ? clampTo(b, out[i]!) : cell.r;
        cell.r = { ...cell.r, x: p.x, y: p.y };
      });
    }
    for (const { cell, entry } of live) this.draw(cell, entry, moved.get(entry.key) ?? null, now, reduced);
  }

  /** Gives new pets a place (a subagent next to its root pet) and, with reduced motion, spreads all of them. */
  private place(live: { key: string; cell: Cell; entry: Entry }[], b: Bounds, reduced: boolean, now: number): void {
    if (reduced) {
      const spots = reducedLayout(live.length, b);
      live.forEach(({ cell }, i) => {
        const p = spots[i]!;
        cell.r = { ...cell.r, x: p.x, y: p.y, tx: p.x, ty: p.y };
        cell.placed = true;
      });
      return;
    }
    for (const { cell, entry } of live) {
      if (cell.placed) continue;
      const others = live.filter((l) => l.cell.placed);
      const parent = this.nursery.mode === "session" && !entry.root ? others.find((l) => l.entry.root) : undefined;
      const p = spawnPoint(b, others.map((l) => l.cell.r), Math.random, parent ? parent.cell.r : null);
      cell.r = newRoamer(p, now, Math.random);
      cell.placed = true;
    }
  }

  private draw(cell: Cell, entry: Entry, moved: Moved | null, now: number, reduced: boolean): void {
    const pet = this.lib.get(entry.petId);
    const state = busyOf(entry.arbiter) ?? "idle";
    if (state !== cell.state) {
      cell.state = state;
      cell.el.dataset.state = state;
    }
    if (this.opts.badges && entry.kind === "session") {
      const n = busySubs(entry.arbiter);
      cell.badge.hidden = n === 0;
      if (n > 0) {
        cell.badge.textContent = String(n);
        cell.badge.title = `${n} subagent${n === 1 ? "" : "s"} working`;
      }
    }
    this.stand(cell, entry, now);
    if (!pet) {
      cell.sprite.className = "sprite missing";
      this.size(cell, { width: this.opts.size, height: this.opts.size }, 1);
      cell.note.textContent = entry.petId ? "pet not installed" : "no pet";
      return;
    }
    cell.sprite.className = "sprite";
    if (cell.note.textContent) cell.note.textContent = "";
    const cur = this.nursery.view(entry, now);
    let anim = cur.kind === "look" ? "look" : resolveAnimation(pet.catalog, cur.name);
    let startedAt = cur.startedAt;
    let look: number | null = cur.look;
    if (moved?.anim && !reduced) {
      anim = moved.anim;
      startedAt = moved.since;
      look = moved.look ?? look;
    }
    let url = this.lib.strip(pet.id, anim);
    if (url === undefined && this.lib.failed(pet.id, anim)) {
      anim = "idle";
      url = this.lib.strip(pet.id, anim);
    }
    const def = pet.catalog[anim] ?? pet.catalog.idle;
    if (!def) return;
    const fit = fitSize(pet.cell, this.opts.size);
    this.size(cell, fit, def.frames);
    const frame = reduced ? 0 : anim === "look" ? lookIndex(look ?? 180) : frameAt(def.durations, now - startedAt, def.loop);
    const pos = `${-frame * fit.width}px 0`;
    if (pos !== cell.pos) {
      cell.pos = pos;
      cell.sprite.style.backgroundPosition = pos;
    }
    if (url !== undefined && url !== cell.url) {
      cell.url = url;
      cell.sprite.style.backgroundImage = `url("${url}")`;
    }
  }

  /** Puts the pet's box at its spot: nearer pets (larger y) in front, and a finished pet fading out. */
  private stand(cell: Cell, entry: Entry, now: number): void {
    if (!cell.placed) return;
    const xform = `translate(${(cell.r.x - LABEL_W / 2).toFixed(1)}px, ${(cell.r.y - this.opts.size).toFixed(1)}px)`;
    if (xform !== cell.xform) {
      cell.xform = xform;
      cell.el.style.transform = xform;
    }
    const z = String(Math.round(cell.r.y));
    if (z !== cell.z) {
      cell.z = z;
      cell.el.style.zIndex = z;
    }
    const fade = entry.removeAt === null ? "" : Math.max(0, Math.min(1, (entry.removeAt - now) / FADE_MS)).toFixed(2);
    if (fade !== cell.fade) {
      cell.fade = fade;
      cell.el.style.opacity = fade;
    }
  }

  private size(cell: Cell, fit: { width: number; height: number }, frames: number): void {
    const sized = `${fit.width}x${fit.height}x${frames}`;
    if (sized === cell.sized) return;
    cell.sized = sized;
    cell.sprite.style.width = `${fit.width}px`;
    cell.sprite.style.height = `${fit.height}px`;
    cell.sprite.style.backgroundSize = `${fit.width * frames}px ${fit.height}px`;
  }

  // Where the pet stands this frame. A busy pet (or one showing a reaction) stays where it is. A
  // finished one walks toward the nearer edge. In a play (and while the arbiter shows idle) the
  // script's pose drives it: walk to or away from the partner, face it, or sleep. Otherwise it
  // roams: pick a spot anywhere in the room, walk there, pause, repeat. A pet that draws no walk
  // strips glides in idle.
  private move(cell: Cell, entry: Entry, pet: LoadedPet, idle: boolean, now: number, dt: number, b: Bounds): Moved {
    const walkAnim = (dir: number): string => resolveAnimation(pet.catalog, dir > 0 ? "walk-right" : "walk-left");
    const hasWalk = walkAnim(1) !== "idle" || walkAnim(-1) !== "idle";
    const info = idle && entry.removeAt === null ? this.nursery.playInfo(entry.key) : null;
    const wasPlaying = cell.playing;
    cell.playing = info !== null;
    let anim: string | null = null;
    let look: number | null = null;
    let dx = 0;
    let dy = 0;
    const walkTo = (to: Pos, speed: number): void => {
      const s = stepToward(cell.r, to, speed, dt);
      cell.r = { ...cell.r, x: s.x, y: s.y, tx: s.x, ty: s.y };
      [dx, dy] = [s.dx, s.dy];
    };
    if (!idle) {
      // Holds still: the roam target and pause stay as they were.
    } else if (entry.removeAt !== null) {
      walkTo(edgeTarget(b, cell.r), SPEED);
    } else if (info) {
      const other = this.cells.get(info.partner);
      const at: Pos = other?.placed ? other.r : cell.r;
      if (info.pose.mode === "walk" && other?.placed) {
        const to = playTarget(info.pose.toward, cell.r, at, b);
        if (to) walkTo(to, PLAY_SPEED);
      } else if (info.pose.mode === "look" && resolveAnimation(pet.catalog, "look") === "look") {
        anim = "look";
        look = angleTo(at.x - cell.r.x, at.y - cell.r.y);
      } else if (info.pose.mode === "sleep" && resolveAnimation(pet.catalog, "sleeping") !== "idle") anim = resolveAnimation(pet.catalog, "sleeping");
    } else {
      // Play just ended or was cut short: stand a while, then roam again.
      const r = wasPlaying ? { ...newRoamer(cell.r, now, Math.random), tx: cell.r.x, ty: cell.r.y } : cell.r;
      const out = roam(r, b, now, dt, Math.random);
      cell.r = out.roamer;
      [dx, dy] = [out.dx, out.dy];
    }
    if (Math.abs(dx) > 0.01) cell.facing = Math.sign(dx);
    const moving = dx !== 0 || dy !== 0;
    const dir = moving ? cell.facing : 0;
    if (dir !== cell.dir) [cell.dir, cell.walkSince] = [dir, now];
    if (anim === null && moving && hasWalk) anim = walkAnim(cell.facing);
    return { anim, name: anim ?? "", since: anim === "look" || anim === null ? 0 : anim.startsWith("walk") ? cell.walkSince : 0, look };
  }
}

/** Whether the person asked for less motion. */
export const reducedMotion = (): boolean => globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

/** One shared loop paints every room: a single timer however many pets there are. */
export class FrameLoop {
  private rooms = new Set<Room>();
  private tickers: (() => void)[] = [];
  private running = false;
  private last = 0;

  add(room: Room): void {
    this.rooms.add(room);
  }

  /** Called every frame before painting (for ticks that change membership). */
  onFrame(fn: () => void): void {
    this.tickers.push(fn);
  }

  start(): void {
    if (this.running) return;
    this.running = true;
    const frame = (): void => {
      const now = Date.now();
      // 20 frames a second is plenty: the shortest frame lasts 90 ms.
      if (now - this.last >= 50) {
        this.last = now;
        for (const fn of this.tickers) fn();
        const reduced = reducedMotion();
        for (const room of this.rooms) room.paint(now, reduced);
      }
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  }
}
