// The nursery's DOM: a responsive grid of small sprites and the one shared loop that animates
// them. Every string from outside (session titles, agent briefs, nicknames) goes in through
// `textContent`, never markup.

import { lookIndex } from "./arbiter.ts";
import type { LoadedPet, PetLibrary } from "./library.ts";
import type { Entry, Nursery } from "./nursery.ts";
import { resolveAnimation } from "./pet.ts";
import { busyOf, busySubs } from "./route.ts";
import { fitSize, frameAt } from "./render.ts";
import { angleTo, reachPx } from "./play.ts";
import { PLAY_SPEED, newWalker, nextDelay, stepWalker } from "./wander.ts";
import type { Walker } from "./wander.ts";

export const CSS = `
:root { color-scheme: light dark; }
.nursery { display: flex; flex-direction: column; gap: var(--space, 8px); font-size: 13px; }
.bar { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
.bar label { color: var(--muted); }
.bar select, .bar input, .bar button { font: inherit; color: var(--fg); background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius, 6px); padding: 3px 6px; max-width: 100%; }
.bar button { cursor: pointer; }
.bar button:hover:not(:disabled) { border-color: var(--accent); }
.summary { color: var(--muted); }
.empty { color: var(--muted); padding: 16px 0; text-align: center; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(104px, 1fr)); gap: 8px; }
.cell { position: relative; display: flex; flex-direction: column; align-items: center; gap: 2px; padding: 8px 4px 6px; background: var(--panel); border: 1px solid var(--border); border-radius: var(--radius, 6px); min-width: 0; }
.cell[data-state="working"] { border-color: var(--accent); }
.cell[data-state="waiting"] { border-color: var(--danger); }
.stage { display: flex; align-items: flex-end; justify-content: center; width: 100%; }
.cell[data-play] { z-index: 1; }
.sprite { image-rendering: pixelated; background-repeat: no-repeat; cursor: pointer; flex: none; }
.sprite.missing { border: 1px dashed var(--border); box-sizing: border-box; }
.label { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: center; }
.anim { color: var(--muted); font-size: 11px; }
.badge { position: absolute; top: 4px; right: 4px; min-width: 16px; padding: 0 4px; border-radius: 8px; background: var(--accent); color: var(--bg); font-size: 11px; line-height: 16px; text-align: center; }
.badge[hidden] { display: none; }
.take { position: absolute; top: 2px; left: 2px; width: 20px; height: 20px; padding: 0; line-height: 1; border: 0; background: transparent; color: var(--muted); cursor: pointer; border-radius: 4px; }
.take:hover { color: var(--danger); }
`;

export type GridOptions = {
  /** Sprite size in px (the cell is fitted inside a size x size square). */
  size: number;
  /** A button takes a pet out ("My nursery"). */
  mine: boolean;
  /** Show the count of busy subagents on a pet (global nursery). */
  badges: boolean;
  onPat(key: string): void;
  onTake?(key: string): void;
};

type Cell = {
  el: HTMLElement;
  sprite: HTMLElement;
  label: HTMLElement;
  anim: HTMLElement;
  badge: HTMLElement;
  walker: Walker | null;
  dir: number;
  walkSince: number;
  url: string;
  pos: string;
  sized: string;
  text: string;
  state: string;
};

const make = (tag: string, cls: string, text = ""): HTMLElement => {
  const el = document.createElement(tag);
  el.className = cls;
  if (text) el.textContent = text;
  return el;
};

export class Grid {
  readonly el = make("div", "grid");
  private cells = new Map<string, Cell>();
  private last = 0;

  private nursery: Nursery;
  private lib: PetLibrary;
  private opts: GridOptions;

  constructor(nursery: Nursery, lib: PetLibrary, opts: GridOptions) {
    this.nursery = nursery;
    this.lib = lib;
    this.opts = opts;
  }

  /** Makes the cells match the nursery's pets, in its order. */
  sync(): void {
    const entries = this.nursery.sorted();
    const keep = new Set(entries.map((e) => e.key));
    for (const [key, cell] of this.cells) {
      if (keep.has(key)) continue;
      cell.el.remove();
      this.cells.delete(key);
    }
    let prev: Element | null = null;
    for (const entry of entries) {
      let cell = this.cells.get(entry.key);
      if (!cell) {
        cell = this.build(entry.key);
        this.cells.set(entry.key, cell);
      }
      if (cell.text !== entry.label) {
        cell.text = entry.label;
        cell.label.textContent = entry.label;
        cell.el.title = entry.label;
      }
      const at: Element | null = prev ? prev.nextElementSibling : this.el.firstElementChild;
      if (at !== cell.el) this.el.insertBefore(cell.el, at);
      prev = cell.el;
    }
  }

  private build(key: string): Cell {
    const el = make("div", "cell");
    const stage = make("div", "stage");
    const sprite = make("div", "sprite");
    sprite.addEventListener("click", () => this.opts.onPat(key));
    stage.append(sprite);
    const label = make("div", "label");
    const anim = make("div", "anim");
    const badge = make("div", "badge");
    badge.hidden = true;
    el.append(stage, label, anim, badge);
    if (this.opts.mine) {
      const take = make("button", "take", "×") as HTMLButtonElement;
      take.type = "button";
      take.title = "Take out of the nursery";
      take.setAttribute("aria-label", "Take out of the nursery");
      take.addEventListener("click", () => this.opts.onTake?.(key));
      el.append(take);
    }
    return { el, sprite, label, anim, badge, walker: null, dir: 0, walkSince: 0, url: "", pos: "", sized: "", text: "\0", state: "" };
  }

  /** Draws every pet at `now` (ms since epoch). */
  paint(now: number, reduced: boolean): void {
    const dt = this.last ? now - this.last : 0;
    this.last = now;
    for (const [key, cell] of this.cells) {
      const entry = this.nursery.entries.get(key);
      if (entry) this.draw(cell, entry, now, dt, reduced);
    }
  }

  private draw(cell: Cell, entry: Entry, now: number, dt: number, reduced: boolean): void {
    const pet = this.lib.get(entry.petId);
    const state = busyOf(entry.arbiter) ?? "idle";
    if (state !== cell.state) {
      cell.state = state;
      cell.el.dataset.state = state;
    }
    if (this.opts.badges) {
      const n = busySubs(entry.arbiter);
      cell.badge.hidden = n === 0;
      if (n > 0) {
        cell.badge.textContent = String(n);
        cell.badge.title = `${n} subagent${n === 1 ? "" : "s"} working`;
      }
    }
    if (!pet) {
      cell.sprite.className = "sprite missing";
      this.size(cell, { width: this.opts.size, height: this.opts.size }, 1);
      cell.anim.textContent = entry.petId ? "pet not installed" : "no pet";
      return;
    }
    cell.sprite.className = "sprite";
    const cur = this.nursery.view(entry, now);
    let name = cur.name;
    let anim = cur.kind === "look" ? "look" : resolveAnimation(pet.catalog, cur.name);
    let startedAt = cur.startedAt;
    let look: number | null = cur.look;
    let offset = 0;
    if (reduced) {
      cell.walker = null;
      delete cell.el.dataset.play;
    } else {
      const idle = cur.name === "idle" && cur.kind === "loop";
      const moved = this.move(cell, entry.key, pet, idle, now, dt);
      offset = moved.x;
      if (moved.anim) {
        anim = moved.anim;
        name = moved.name;
        startedAt = moved.since;
        look = moved.look ?? look;
      }
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
    cell.sprite.style.transform = offset ? `translateX(${offset.toFixed(1)}px)` : "";
    if (cell.anim.textContent !== name) cell.anim.textContent = name;
  }

  private size(cell: Cell, fit: { width: number; height: number }, frames: number): void {
    const sized = `${fit.width}x${fit.height}x${frames}`;
    if (sized === cell.sized) return;
    cell.sized = sized;
    cell.sprite.style.width = `${fit.width}px`;
    cell.sprite.style.height = `${fit.height}px`;
    cell.sprite.style.backgroundSize = `${fit.width * frames}px ${fit.height}px`;
  }

  private centre(cell: Cell): { x: number; y: number } {
    return { x: cell.el.offsetLeft + cell.el.offsetWidth / 2, y: cell.el.offsetTop };
  }

  /** The pets standing left and right of `key` in its row of the grid. */
  neighbors(key: string): string[] {
    const keys = this.nursery.sorted().map((e) => e.key);
    const i = keys.indexOf(key);
    const here = this.cells.get(key);
    if (!here) return [];
    return [keys[i - 1], keys[i + 1]].flatMap((k) => {
      const other = k === undefined ? undefined : this.cells.get(k);
      return k !== undefined && other && Math.abs(other.el.offsetTop - here.el.offsetTop) < 4 ? [k] : [];
    });
  }

  // Where the pet stands this frame. While it plays (and the arbiter shows idle) the script's pose
  // drives it: walk toward or away from the partner, face it, or sleep. Otherwise an idle pet
  // drifts to a spot now and then, and a busy one eases back to its slot.
  private move(cell: Cell, key: string, pet: LoadedPet, idle: boolean, now: number, dt: number): { x: number; anim: string | null; name: string; since: number; look: number | null } {
    const prev = (cell.walker ??= newWalker(now, Math.random));
    const info = idle ? this.nursery.playInfo(key) : null;
    const walkAnim = (dir: number): string => resolveAnimation(pet.catalog, dir > 0 ? "walk-right" : "walk-left");
    const hasWalk = walkAnim(1) !== "idle" || walkAnim(-1) !== "idle";
    let anim: string | null = null;
    let look: number | null = null;
    let next: typeof prev;
    let dir = 0;
    if (info) {
      const other = this.cells.get(info.partner);
      const me = this.centre(cell);
      const at = other ? this.centre(other) : me;
      const dx = at.x - me.x;
      const target = info.pose.mode === "walk" ? Math.round(info.pose.toward * reachPx(dx, this.opts.size)) : prev.x;
      const r = stepWalker({ ...prev, target, nextAt: Infinity }, now, dt, true, Math.random, 0, PLAY_SPEED);
      next = r.walker;
      dir = r.dir;
      if (info.pose.mode === "look" && resolveAnimation(pet.catalog, "look") === "look") {
        anim = "look";
        look = angleTo(dx, at.y - me.y);
      } else if (info.pose.mode === "sleep" && resolveAnimation(pet.catalog, "sleeping") !== "idle") anim = resolveAnimation(pet.catalog, "sleeping");
      else if (dir !== 0 && walkAnim(dir) !== "idle") anim = walkAnim(dir);
      cell.el.dataset.play = info.pose.mode;
    } else {
      delete cell.el.dataset.play;
      let w = prev;
      // Play just ended or was cut short: head back to the slot, then wander again.
      if (prev.nextAt === Infinity) w = { ...prev, target: 0, nextAt: now + nextDelay(Math.random) };
      // A pet that draws no walk animation never wanders; it only slides home.
      if (!hasWalk) w = { ...w, nextAt: Math.max(w.nextAt, now + 5000) };
      const r = stepWalker(w, now, dt, idle, Math.random);
      next = r.walker;
      dir = r.dir;
      if (idle && dir !== 0 && walkAnim(dir) !== "idle") anim = walkAnim(dir);
    }
    if (dir !== cell.dir) [cell.dir, cell.walkSince] = [dir, now];
    cell.walker = next;
    return { x: next.x, anim, name: anim ?? "", since: anim === "look" || anim === null ? 0 : anim.startsWith("walk") ? cell.walkSince : 0, look };
  }
}

/** Whether the person asked for less motion. */
export const reducedMotion = (): boolean => globalThis.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

/** One shared loop paints every grid: a single timer however many pets there are. */
export class FrameLoop {
  private grids = new Set<Grid>();
  private tickers: (() => void)[] = [];
  private running = false;
  private last = 0;

  add(grid: Grid): void {
    this.grids.add(grid);
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
        for (const grid of this.grids) grid.paint(now, reduced);
      }
      requestAnimationFrame(frame);
    };
    requestAnimationFrame(frame);
  }
}
