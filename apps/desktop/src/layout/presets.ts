// Layout presets: a layout as a small JSON file that can be exported, shared and imported. A
// preset is data only: panel ids placed in columns, the closed panels, the modules inside
// panels, and the mods it expects. It carries no code, and importing one never installs or runs
// anything: panels from a mod that is not installed simply stay out of sight until it is.
// Importing is strict: a file that is not exactly this format, or whose version this app does not
// know, is refused with a reason and changes nothing. Pure (test/presets.test.ts).
//
//   { "format": "imperium-layout-preset", "version": 1, "name": "Wide review",
//     "arrangement": [{ "id": "left", "panels": ["projects"] }, ...] | null,
//     "hidden": ["usage"], "modules": { "agents": ["mod:nursery:pets"] },
//     "mods": ["nursery"] }

import { isPanelId, readArrangement, readModules, type LayoutShape } from "./shapes.ts";

export const PRESET_FORMAT = "imperium-layout-preset";
/** The only version this app writes and reads. A different one is refused, never guessed at. */
export const PRESET_VERSION = 1;
/** The largest preset file read, in characters: a layout is tiny, so anything bigger is not one. */
export const PRESET_MAX_CHARS = 64 * 1024;

export type LayoutPreset = {
  format: typeof PRESET_FORMAT;
  version: typeof PRESET_VERSION;
  name: string;
  shape: LayoutShape;
  /** Ids of the mods whose panels the layout places. */
  mods: string[];
};

export type PresetResult = { ok: true; preset: LayoutPreset } | { ok: false; error: string };

const KEYS = ["format", "version", "name", "arrangement", "hidden", "modules", "mods"];

/** The mod a panel id belongs to (`mod:<mod>:<panel>`), or null for a built-in panel. */
export function modOf(panel: string): string | null {
  const parts = panel.split(":");
  return parts[0] === "mod" && parts.length >= 3 && parts[1] ? parts[1] : null;
}

/** The mods a shape places panels from, sorted. */
export function modsUsed(shape: LayoutShape): string[] {
  const ids = [
    ...(shape.arrangement ?? []).flatMap((column) => column.panels),
    ...shape.hidden,
    ...Object.entries(shape.modules).flat(2),
  ];
  return [...new Set(ids.map(modOf).filter((id): id is string => id !== null))].sort();
}

/** A shape as a preset named `name` (blank names read as "Layout"). */
export function makePreset(shape: LayoutShape, name: string): LayoutPreset {
  return {
    format: PRESET_FORMAT,
    version: PRESET_VERSION,
    name: name.trim().slice(0, 80) || "Layout",
    shape: {
      arrangement: shape.arrangement ? shape.arrangement.map((c) => ({ id: c.id, panels: [...c.panels] })) : null,
      hidden: [...shape.hidden],
      modules: Object.fromEntries(Object.entries(shape.modules).map(([host, list]) => [host, [...list]])),
    },
    mods: modsUsed(shape),
  };
}

/** The file's text: stable key order, indented so it reads and diffs well. */
export function presetText(preset: LayoutPreset): string {
  return (
    JSON.stringify(
      {
        format: preset.format,
        version: preset.version,
        name: preset.name,
        arrangement: preset.shape.arrangement,
        hidden: preset.shape.hidden,
        modules: preset.shape.modules,
        mods: preset.mods,
      },
      null,
      2,
    ) + "\n"
  );
}

/** A file name for the preset: letters, digits, dashes, ending in `.json`. */
export function presetFileName(preset: LayoutPreset): string {
  const stem = preset.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 60);
  return `imperium-layout-${stem || "preset"}.json`;
}

const refuse = (error: string): PresetResult => ({ ok: false, error });

/**
 * Reads a preset file. Refuses, with a reason, anything that is not valid JSON, is too large, is
 * not this format, has a version other than the one this app knows, has keys beyond the format's,
 * or whose arrangement, closed panels, modules or mod list are malformed.
 */
export function parsePreset(text: string): PresetResult {
  if (text.length > PRESET_MAX_CHARS) return refuse("This file is too large to be a layout preset.");
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return refuse("This file is not valid JSON.");
  }
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return refuse("This file is not a layout preset.");
  const data = raw as Record<string, unknown>;
  if (data.format !== PRESET_FORMAT) return refuse("This file is not an Imperium layout preset.");
  if (data.version !== PRESET_VERSION) {
    return refuse(
      typeof data.version === "number" && data.version > PRESET_VERSION
        ? `This preset is version ${data.version}, newer than this app understands (version ${PRESET_VERSION}). Update Imperium to use it.`
        : `This preset's version (${String(data.version)}) is not one this app understands.`,
    );
  }
  const extra = Object.keys(data).filter((key) => !KEYS.includes(key));
  if (extra.length > 0) return refuse(`This preset has fields this app does not know: ${extra.slice(0, 3).join(", ")}.`);
  if (typeof data.name !== "string" || data.name.trim() === "" || data.name.length > 80) {
    return refuse("The preset's name must be text of 1 to 80 characters.");
  }
  const arrangement = readArrangement(data.arrangement);
  if (arrangement === undefined) return refuse("The preset's arrangement is malformed (columns of distinct panels, each panel in one column).");
  if (!Array.isArray(data.hidden) || data.hidden.length > 128 || !data.hidden.every(isPanelId) || new Set(data.hidden).size !== data.hidden.length) {
    return refuse("The preset's list of closed panels is malformed.");
  }
  const modules = readModules(data.modules);
  if (modules === undefined) return refuse("The preset's modules are malformed.");
  if (!Array.isArray(data.mods) || data.mods.length > 64 || !data.mods.every((id) => typeof id === "string" && /^[A-Za-z0-9_.-]{1,64}$/.test(id))) {
    return refuse("The preset's list of mods is malformed.");
  }
  return {
    ok: true,
    preset: {
      format: PRESET_FORMAT,
      version: PRESET_VERSION,
      name: data.name.trim(),
      shape: { arrangement, hidden: [...(data.hidden as string[])], modules },
      mods: [...(data.mods as string[])].sort(),
    },
  };
}

/** The mods a preset expects that are not installed (`installed` lists the ids of those that are). */
export function missingMods(preset: LayoutPreset, installed: string[]): string[] {
  return preset.mods.filter((id) => !installed.includes(id));
}
