// Where each panel lives beyond its column: a panel can be a module inside another panel (a
// section under the host's own content), or leave the window for one of its own. Together with
// the arrangement (arrangement.ts) this is a "placement". Everything here is pure, so drags
// and the keyboard alternatives to them (the panel menu's *Move to…* entries) are tested without
// a window (test/placement.test.ts).
//
// Modules are panels that are not built into the app: panels a mod contributes (`isModule`),
// which are self-contained views. A built-in panel hosts modules but is never one itself, and a
// module never hosts another. The arrangement keeps listing a hosted module in its column, so
// taking it out puts it back where it was; while hosted it is simply not drawn there.

import { applyDrop, type Arrangement, type DropTarget } from "./arrangement.ts";

/** Modules by the panel that hosts them, in the order they are shown. */
export type Modules = Record<string, string[]>;

export type Placement = { arrangement: Arrangement; modules: Modules };

/** The result of a drop: the new placement, and whether the panel should open in its own window. */
export type Dropped = Placement & { openWindow: boolean };

/** Whether a panel is a module: panels mods contribute (`mod:<mod>:<panel>`). Built-in panels, the session among them, never are. */
export const isModulePanel = (id: string): boolean => id.startsWith("mod:");

/** The host of `module`, if it is hosted. */
export function hostOf(modules: Modules, module: string): string | null {
  return Object.keys(modules).find((host) => modules[host].includes(module)) ?? null;
}

/** Whether `host` can take `module`: a module is not its own host, a module never hosts, and the session (the canvas) never hosts. */
export function canHost(host: string, module: string, isModule: (id: string) => boolean): boolean {
  return host !== module && host !== "session" && isModule(module) && !isModule(host);
}

/** `modules` without `module`; hosts left with nothing are forgotten. */
export function takeOut(modules: Modules, module: string): Modules {
  const next: Modules = {};
  for (const [host, list] of Object.entries(modules)) {
    const rest = list.filter((id) => id !== module);
    if (rest.length > 0) next[host] = rest;
  }
  return next;
}

/** `module` placed as the last module of `host` (moved from another host if it had one); unchanged when it cannot go there. */
export function placeInto(modules: Modules, module: string, host: string, isModule: (id: string) => boolean): Modules {
  if (!canHost(host, module, isModule)) return modules;
  return { ...takeOut(modules, module), [host]: [...(takeOut(modules, module)[host] ?? []), module] };
}

/**
 * The modules to draw: each host that is on screen (`hostShown`) gets its modules that are on
 * screen too (`moduleShown`). A module whose host is closed, detached or gone is not hosted for
 * now: it shows in its own column place instead, and goes back into the host when it returns.
 */
export function activeModules(modules: Modules, hostShown: (id: string) => boolean, moduleShown: (id: string) => boolean): Modules {
  const active: Modules = {};
  for (const [host, list] of Object.entries(modules)) {
    if (!hostShown(host)) continue;
    const shown = list.filter(moduleShown);
    if (shown.length > 0) active[host] = shown;
  }
  return active;
}

/** Whether two sets of modules are the same hosts with the same modules in the same order. */
export const sameModules = (a: Modules, b: Modules): boolean => JSON.stringify(sorted(a)) === JSON.stringify(sorted(b));
const sorted = (modules: Modules): [string, string[]][] => Object.entries(modules).sort(([x], [y]) => x.localeCompare(y));

/**
 * The placement after dropping `panel` on `target`. A drop in a column or a new column takes the
 * panel out of any host; a drop on a panel hosts it there (if it can be a module and the host
 * can take it); a drop outside the window asks for a window of its own. Dragging happens only in
 * the panel editor (`editing`); the keyboard alternatives pass `true`, as the menu is their
 * editor. Anything that cannot happen leaves the placement as it was.
 */
export function dropPanel(
  placement: Placement,
  panel: string,
  target: DropTarget,
  editing: boolean,
  newColumnId: () => string,
  isModule: (id: string) => boolean,
): Dropped {
  const unchanged: Dropped = { ...placement, openWindow: false };
  if (!editing) return unchanged;
  if (target.kind === "window") return { ...unchanged, openWindow: true };
  if (target.kind === "module") {
    return { ...unchanged, modules: placeInto(placement.modules, panel, target.host, isModule) };
  }
  const arrangement = applyDrop(placement.arrangement, panel, target, editing, newColumnId);
  // A drop that changed nothing (on the panel itself) also leaves a hosted module where it is.
  if (arrangement === placement.arrangement) return unchanged;
  return { arrangement, modules: takeOut(placement.modules, panel), openWindow: false };
}
