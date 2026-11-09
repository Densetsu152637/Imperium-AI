// The two panels. Each runs in its own frame: `activate` branches on `ctx.panelId` and calls
// one of these. They wire a Nursery to the host (sessions, pets, storage) and the view.

import type { ModContext, SessionSnapshot } from "@imperium/mod-sdk";

import type { Env } from "./arbiter.ts";
import { PetLibrary } from "./library.ts";
import { Nursery } from "./nursery.ts";
import type { Deps, Kind } from "./nursery.ts";
import { MINE, MIX, count, pickPet, sessionPet } from "./pick.ts";
import { allDrawn } from "./pet.ts";
import { CSS, FrameLoop, Room, reducedMotion } from "./view.ts";

const SLEEP_AGENT_MS = 180_000;
const SLEEP_PERSONAL_MS = 60_000;
const SIZE = 112;

type Resident = { uid: string; petId: string; name: string };

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] => {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text) node.textContent = text;
  return node;
};

const option = (value: string, text: string): HTMLOptionElement => {
  const o = el("option", "", text);
  o.value = value;
  return o;
};

/** Fills a select with options, keeping the current value when it is still there. */
function setOptions(select: HTMLSelectElement, options: [string, string][], fallback: string): void {
  const keep = select.value;
  select.replaceChildren(...options.map(([v, t]) => option(v, t)));
  select.value = options.some(([v]) => v === keep) ? keep : fallback;
}

/** The common frame of a panel: styles, a controls bar, a summary line, the room, an empty note. */
function shell(ctx: ModContext, room: Room): { bar: HTMLElement; summary: HTMLElement; empty: HTMLElement } {
  const root = el("div", "nursery");
  const style = el("style");
  style.textContent = CSS;
  const bar = el("div", "bar");
  const summary = el("div", "summary");
  const empty = el("div", "empty");
  root.append(style, bar, summary, empty, room.el);
  ctx.panel(ctx.panelId).render(root);
  return { bar, summary, empty };
}

function labelled(text: string, control: HTMLElement): HTMLElement {
  const wrap = el("label", "", `${text} `);
  wrap.append(control);
  return wrap;
}

/** Agents' and sessions' pets sleep late; your pet and the extras, which hear less, sooner. */
const envFor =
  (lib: PetLibrary) =>
  (petId: string | null, kind: Kind): Env => ({
    catalog: lib.get(petId)?.catalog ?? allDrawn(),
    sleepAfterMs: kind === "agent" || kind === "session" ? SLEEP_AGENT_MS : SLEEP_PERSONAL_MS,
    fidget: true,
  });

/** The "Pets" select: which pet agents (in the nursery, sessions without an environment pet) get. */
function petSetting(ctx: ModContext, lib: PetLibrary, bar: HTMLElement, setting: string, change: (value: string) => void, withMine = false): void {
  const select = el("select");
  const extra: [string, string][] = withMine ? [[MINE, "Same as my pet"]] : [];
  setOptions(select, [[MIX, "A mix of my pets"], ...extra, ...lib.pets.map((p): [string, string] => [p.id, p.name])], MIX);
  select.value = setting === MINE && withMine ? MINE : lib.pets.some((p) => p.id === setting) ? setting : MIX;
  select.addEventListener("change", () => {
    void ctx.storage.set("agentPets", select.value);
    change(select.value);
  });
  bar.append(labelled("Pets", select));
}

/** The "Play" checkbox: whether free pets play with each other (`play`, on by default). */
function playToggle(ctx: ModContext, bar: HTMLElement, nursery: Nursery, on: boolean): void {
  const box = el("input");
  box.type = "checkbox";
  box.checked = on;
  nursery.playEnabled = on;
  box.addEventListener("change", () => {
    nursery.playEnabled = box.checked;
    void ctx.storage.set("play", box.checked);
  });
  bar.append(labelled("Play", box));
}

/** Runs every frame before painting: membership changes, then play. */
function frameWork(loop: FrameLoop, nursery: Nursery, room: Room): void {
  nursery.at = (key) => room.pos(key);
  loop.onFrame(() => {
    const now = Date.now();
    nursery.tick(now);
    nursery.tickPlay(now, reducedMotion());
  });
}

const reading = async <T>(ctx: ModContext, key: string, fallback: T): Promise<T> => {
  try {
    return (await ctx.storage.get<T>(key)) ?? fallback;
  } catch {
    return fallback;
  }
};

/** The session nursery: the pets of the selected session's agents. */
async function sessionPanel(ctx: ModContext): Promise<void> {
  const loop = new FrameLoop();
  const lib = new PetLibrary(ctx.pets);
  let setting = await reading(ctx, "agentPets", MIX);
  const deps: Deps = {
    envFor: envFor(lib),
    assign: ({ key, root }) => pickPet(lib.ids(), setting, key, root),
  };
  const nursery = new Nursery("session", deps);
  const room = new Room(nursery, lib, { size: SIZE, takeable: false, badges: false, onPat: (key) => nursery.pat(key, Date.now()) });
  const { bar, summary, empty } = shell(ctx, room);

  let sessions: SessionSnapshot[] = [];
  let selected: string | null = null;
  const pick = el("select");
  const refresh = (): void => {
    const s = nursery.summary();
    room.sync();
    if (lib.pets.length === 0) empty.textContent = "No pets installed";
    else empty.textContent = sessions.length === 0 ? "No sessions yet" : nursery.target() === null ? "No session selected" : "";
    summary.textContent = nursery.target() === null ? "" : s.subagents === 0 ? "No subagents working" : `${count(s.subagents, "subagent")} working`;
    setOptions(pick, [["", "Follow the selected session"], ...sessions.map((x): [string, string] => [x.sessionId, x.title || x.sessionId])], nursery.pinned ?? "");
  };
  const apply = (): void => {
    nursery.snapshots(sessions, selected, Date.now());
  };
  nursery.onChange = () => queueMicrotask(refresh);

  await lib.load().catch(() => {});
  petSetting(ctx, lib, bar, setting, (value) => {
    setting = value;
    nursery.reassign(Date.now());
  });
  playToggle(ctx, bar, nursery, await reading(ctx, "play", true));
  nursery.pinned = await reading<string | null>(ctx, "pinnedSession", null);
  pick.addEventListener("change", () => {
    nursery.pinned = pick.value || null;
    void ctx.storage.set("pinnedSession", nursery.pinned);
    apply();
  });
  bar.append(labelled("Session", pick));

  ctx.sessions.subscribe((event) => nursery.event(event, Date.now()));
  ctx.sessions.onChange((list, sel) => {
    sessions = list;
    selected = sel;
    apply();
  });
  [sessions, selected] = await Promise.all([ctx.sessions.list(), ctx.sessions.selected()]);
  apply();
  refresh();

  loop.add(room);
  frameWork(loop, nursery, room);
  loop.start();
}

/**
 * The Nursery, which is also the pet panel: your pet (always in, hearing every session), one pet
 * per unarchived session (its environment's pet, else the "Pets" setting), and extra pets you
 * put in and take out.
 */
async function nurseryPanel(ctx: ModContext): Promise<void> {
  const loop = new FrameLoop();
  const lib = new PetLibrary(ctx.pets);
  let setting = await reading(ctx, "agentPets", MIX);
  let mine: string | null = null;
  const deps: Deps = {
    envFor: envFor(lib),
    assign: ({ sessionId, envPet }) => sessionPet(lib.ids(), envPet, setting, sessionId, mine),
  };
  const nursery = new Nursery("nursery", deps);
  const names = new Map<string, string>();
  const save = (): void => {
    const residents: Resident[] = nursery.sorted().flatMap((e) => (e.kind === "extra" && e.petId ? [{ uid: e.key, petId: e.petId, name: names.get(e.key) ?? "" }] : []));
    void ctx.storage.set("residents", residents);
  };
  const room = new Room(nursery, lib, {
    size: SIZE,
    takeable: true,
    badges: true,
    onPat: (key) => nursery.pat(key, Date.now()),
    onTake: (key) => {
      nursery.take(key);
      save();
    },
  });
  const { bar, summary, empty } = shell(ctx, room);

  let sessions: SessionSnapshot[] = [];
  const refresh = (): void => {
    const s = nursery.summary();
    room.sync();
    empty.textContent = lib.pets.length === 0 ? "No pets installed" : "";
    summary.textContent = s.sessions === 0 ? "" : `${count(s.sessions, "session")}, ${s.busy} working, ${count(s.subagents, "subagent")}`;
  };
  nursery.onChange = () => queueMicrotask(refresh);

  await lib.load().catch(() => {});
  const stored = await reading<string | null>(ctx, "resident", null);
  const nameOf = (id: string | null): string => lib.get(id)?.name ?? id ?? "";
  mine = stored !== null && lib.get(stored) ? stored : (lib.pets[0]?.id ?? null);
  nursery.setResident(mine, nameOf(mine), Date.now());

  // Your pet: always in the nursery; this chooses which one it is.
  const yours = el("select");
  setOptions(yours, lib.pets.map((p): [string, string] => [p.id, p.name]), mine ?? "");
  yours.value = mine ?? "";
  yours.addEventListener("change", () => {
    mine = yours.value || null;
    void ctx.storage.set("resident", mine);
    nursery.setResident(mine, nameOf(mine), Date.now());
    nursery.reassign(Date.now());
  });
  bar.append(labelled("Your pet", yours));
  petSetting(
    ctx,
    lib,
    bar,
    setting,
    (value) => {
      setting = value;
      nursery.reassign(Date.now());
    },
    true,
  );
  playToggle(ctx, bar, nursery, await reading(ctx, "play", true));

  // Extra pets, put in and taken out by hand (the old "My nursery"; its `residents` are kept).
  const kept = await reading<Resident[]>(ctx, "residents", []);
  const now = Date.now();
  for (const r of Array.isArray(kept) ? kept : []) {
    if (typeof r?.uid !== "string" || typeof r.petId !== "string") continue;
    const name = typeof r.name === "string" ? r.name : "";
    names.set(r.uid, name);
    nursery.add(r.uid, r.petId, name || lib.get(r.petId)?.name || r.petId, now);
  }
  const which = el("select");
  setOptions(which, lib.pets.map((p): [string, string] => [p.id, p.name]), lib.pets[0]?.id ?? "");
  const nick = el("input");
  nick.placeholder = "Nickname (optional)";
  nick.maxLength = 40;
  const put = el("button", "", "Put in");
  put.type = "button";
  put.disabled = lib.pets.length === 0;
  put.addEventListener("click", () => {
    const pet = lib.get(which.value);
    if (!pet) return;
    const uid = globalThis.crypto?.randomUUID?.() ?? `${Date.now().toString(36)}${Math.random().toString(36).slice(2)}`;
    const name = nick.value.trim();
    names.set(uid, name);
    nursery.add(uid, pet.id, name || pet.name, Date.now());
    nick.value = "";
    save();
  });
  const more = el("details", "more");
  more.append(el("summary", "", "Add a pet"), labelled("Pet", which), nick, put);
  bar.append(more);

  ctx.sessions.subscribe((event) => nursery.event(event, Date.now()));
  ctx.sessions.onChange((list) => {
    sessions = list;
    nursery.snapshots(sessions, null, Date.now());
  });
  sessions = await ctx.sessions.list();
  nursery.snapshots(sessions, null, Date.now());
  refresh();

  loop.add(room);
  frameWork(loop, nursery, room);
  loop.start();
}

export function startPanel(ctx: ModContext): void {
  const run = ctx.panelId === "session" ? sessionPanel(ctx) : nurseryPanel(ctx);
  run.catch((error: unknown) => {
    ctx.panel(ctx.panelId).render(`The nursery failed to start: ${String(error)}`);
  });
}
