// The parts of a layout a project can have its own copy of, and which copy applies where. The
// global layout is the default for every environment; an environment uses its own only after the
// user chose *Save as project layout* for it, and falls back to the global one when that is
// removed (*Use global layout*) or the environment is gone. Overrides are kept in the local
// `layout` document (state.ts) beside the global layout, keyed by environment id. Pure
// (test/shapes.test.ts).
//
// A shape covers what is arranged: which panels sit in which columns, which are closed, and
// which are modules inside others. Sizes, folded panels, and detached windows are about the
// screen the user has, not the project, so they stay global.

import type { Arrangement } from "./arrangement.ts";
import { canHost, isModulePanel, type Modules } from "./placement.ts";

export type LayoutShape = {
  /** Which panels sit in which columns; null for the default arrangement. */
  arrangement: Arrangement | null;
  /** Panels the user closed. */
  hidden: string[];
  /** Module panels placed inside other panels, by host. */
  modules: Modules;
};

/** Per-environment shapes, by environment id. */
export type Overrides = Record<string, LayoutShape>;

/** The global shape and the environments' own. */
export type Shapes = { global: LayoutShape; overrides: Overrides };

export const DEFAULT_SHAPE: LayoutShape = { arrangement: null, hidden: [], modules: {} };

/** Ids that must never become object keys: they would reach the prototype. */
const UNSAFE = new Set(["__proto__", "constructor", "prototype"]);

/**
 * Panel and environment ids are whatever the app and the mods make them (a mod's panel is
 * `mod:<mod id>:<panel id>` and mods choose their ids), so the only limits are the ones that keep
 * the data safe: some length, no control characters, and no name that would reach the prototype.
 */
export const isPanelId = (value: unknown): value is string =>
  typeof value === "string" && value.length > 0 && value.length <= 256 && !UNSAFE.has(value) && !/[\u0000-\u001f\u007f]/.test(value);

const isId = isPanelId;

/** A list of distinct panel ids, or null when it is not one. */
function idList(value: unknown, max: number): string[] | null {
  if (!Array.isArray(value) || value.length > max || !value.every(isPanelId)) return null;
  return new Set(value).size === value.length ? value : null;
}

/** An arrangement from untrusted data: columns of distinct panels, each panel in one column only; null when malformed. */
export function readArrangement(value: unknown): Arrangement | null | undefined {
  if (value === null) return null;
  if (!Array.isArray(value) || value.length > 12) return undefined;
  const seenPanels = new Set<string>();
  const seenColumns = new Set<string>();
  const columns: Arrangement = [];
  for (const column of value) {
    if (typeof column !== "object" || column === null || Array.isArray(column)) return undefined;
    const { id, panels, ...extra } = column as Record<string, unknown>;
    const list = idList(panels, 64);
    if (Object.keys(extra).length > 0 || !isPanelId(id) || seenColumns.has(id) || !list) return undefined;
    if (list.some((panel) => seenPanels.has(panel))) return undefined;
    seenColumns.add(id);
    list.forEach((panel) => seenPanels.add(panel));
    columns.push({ id, panels: [...list] });
  }
  return columns.length === 0 ? undefined : columns;
}

/**
 * Modules from untrusted data: hosts with distinct modules, no module in two hosts. Only mod
 * panels are modules and only other panels host them (`canHost`), so a built-in panel is never
 * "hosted" (it would be drawn twice). Strict (a preset): anything malformed or not allowed
 * refuses it all (undefined). Lenient (saved layouts): each entry that is not sound is dropped and
 * the rest kept.
 */
export function readModules(value: unknown, strict = true): Modules | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return strict ? undefined : {};
  const entries = Object.entries(value);
  if (entries.length > 64) return strict ? undefined : {};
  const result: Modules = {};
  const hosted = new Set<string>();
  for (const [host, list] of entries) {
    const modules = idList(list, 32);
    const sound =
      isPanelId(host) && modules !== null && modules.length > 0 && modules.every((id) => canHost(host, id, isModulePanel) && !hosted.has(id));
    if (!sound || modules === null) {
      if (strict) return undefined;
      continue;
    }
    modules.forEach((id) => hosted.add(id));
    Object.defineProperty(result, host, { value: [...modules], enumerable: true, writable: true, configurable: true });
  }
  return result;
}

/**
 * A shape from saved data, keeping what is sound and dropping what is not: a damaged field falls
 * back to its default rather than losing the whole layout.
 */
export function sanitizeShape(value: unknown): LayoutShape {
  const raw = typeof value === "object" && value !== null ? (value as Record<string, unknown>) : {};
  const arrangement = readArrangement(raw.arrangement);
  const hidden = Array.isArray(raw.hidden) ? raw.hidden.filter(isPanelId) : [];
  return {
    arrangement: arrangement ?? null,
    hidden: [...new Set(hidden)],
    modules: readModules(raw.modules, false) ?? {},
  };
}

/** Overrides from saved data; entries that are not an object keyed by a sane environment id are dropped. */
export function sanitizeOverrides(value: unknown): Overrides {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return {};
  const result: Overrides = {};
  for (const [environment, shape] of Object.entries(value)) {
    if (!isId(environment) || typeof shape !== "object" || shape === null) continue;
    Object.defineProperty(result, environment, { value: sanitizeShape(shape), enumerable: true, writable: true, configurable: true });
  }
  return result;
}

/** Whether `environment` has a layout of its own. */
export function hasOverride(shapes: Shapes, environment: string | null): boolean {
  return environment !== null && Object.prototype.hasOwnProperty.call(shapes.overrides, environment);
}

/** The shape that applies to `environment`: its own when it has one, else the global one. */
export function activeShape(shapes: Shapes, environment: string | null): LayoutShape {
  return hasOverride(shapes, environment) ? shapes.overrides[environment as string] : shapes.global;
}

/** The shapes after changing the one that applies to `environment`: edits go to its override when it has one, else to the global layout. */
export function editShape(shapes: Shapes, environment: string | null, change: Partial<LayoutShape>): Shapes {
  const next = { ...activeShape(shapes, environment), ...change };
  if (hasOverride(shapes, environment)) return { ...shapes, overrides: { ...shapes.overrides, [environment as string]: next } };
  return { ...shapes, global: next };
}

/** *Save as project layout*: `environment` gets a copy of the shape it shows now, which it then changes on its own. */
export function saveOverride(shapes: Shapes, environment: string | null): Shapes {
  if (environment === null) return shapes;
  return { ...shapes, overrides: { ...shapes.overrides, [environment]: activeShape(shapes, environment) } };
}

/** *Use global layout*: `environment` forgets its own layout and follows the global one again. */
export function clearOverride(shapes: Shapes, environment: string | null): Shapes {
  if (!hasOverride(shapes, environment)) return shapes;
  const { [environment as string]: _gone, ...rest } = shapes.overrides;
  return { ...shapes, overrides: rest };
}
