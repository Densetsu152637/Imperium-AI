// Project icons: a small glyph on a coloured tile. Each project gets a default derived from its
// path, and the user's own choices are remembered per viewer in local storage.

import { Folder } from "lucide-react";
import { useCallback, useState } from "react";

import { iconFor, iconName } from "./icons";

/** A saved icon name from the app's icon registry (icons.ts). */
export type Glyph = string;
/** The glyphs defaults are drawn from: the original project set, so defaults never change. */
export const GLYPH_NAMES: Glyph[] = [
  "folder",
  "code",
  "book",
  "flask",
  "rocket",
  "star",
  "briefcase",
  "chart",
  "music",
  "camera",
  "globe",
  "bolt",
];

/** Tile colours, dark enough for a white glyph in both themes. */
export const ICON_COLORS = [
  "#2f6fdb",
  "#7b4fd6",
  "#c43f86",
  "#cf3f37",
  "#d06a1c",
  "#a67c0b",
  "#2e8b57",
  "#13827f",
  "#4d6a86",
  "#6b5a45",
];

export type ProjectIconChoice = { icon: Glyph; color: string };

const KEY = "imperium.projectIcons";

/** FNV-1a, so a project keeps the same default icon on every machine. */
function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

export function defaultIcon(path: string): ProjectIconChoice {
  const h = hash(path);
  return { icon: GLYPH_NAMES[h % GLYPH_NAMES.length], color: ICON_COLORS[(h >>> 8) % ICON_COLORS.length] };
}

function loadChoices(): Record<string, ProjectIconChoice> {
  try {
    const saved = JSON.parse(localStorage.getItem(KEY) ?? "null") as Record<string, ProjectIconChoice> | null;
    if (!saved || typeof saved !== "object") return {};
    // Drop entries naming a glyph this version does not draw.
    return Object.fromEntries(Object.entries(saved).filter(([, choice]) => choice && isGlyph(choice.icon)));
  } catch {
    return {};
  }
}

function saveChoices(choices: Record<string, ProjectIconChoice>): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(choices));
  } catch {
    // Unavailable storage only means the icons are not remembered.
  }
}

export type ProjectIcons = {
  iconFor: (path: string) => ProjectIconChoice;
  /** Sets a project's icon; `null` returns it to the default. */
  setIcon: (path: string, choice: ProjectIconChoice | null) => void;
};

export function useProjectIcons(): ProjectIcons {
  const [choices, setChoices] = useState(loadChoices);
  const iconFor = useCallback(
    (path: string) => choices[normalizeProject(path)] ?? defaultIcon(normalizeProject(path)),
    [choices],
  );
  const setIcon = useCallback((path: string, choice: ProjectIconChoice | null) => {
    setChoices((all) => {
      const next = { ...all };
      if (choice) next[normalizeProject(path)] = choice;
      else delete next[normalizeProject(path)];
      saveChoices(next);
      return next;
    });
  }, []);
  return { iconFor, setIcon };
}

/** One spelling per folder: Windows paths arrive with either slash, so compare them with forward
 * slashes and no trailing separator. */
export const normalizeProject = (path: string): string => path.replaceAll("\\", "/").replace(/\/+$/, "");

/** Whether two folder paths name the same folder (drive letters and Windows paths ignore case). */
export function sameFolder(a: string, b: string): boolean {
  const left = normalizeProject(a);
  const right = normalizeProject(b);
  if (left === right) return true;
  const windows = /^[a-z]:\//i.test(left) || left.startsWith("//");
  return windows && left.toLowerCase() === right.toLowerCase();
}

export const isGlyph = (name: string | null | undefined): name is Glyph => iconName(name) !== null;

/** An environment's icon: its own choice when set, else a default derived from its id. */
export function environmentIcon(env: { id: string; icon: string | null; color: string | null }): ProjectIconChoice {
  const fallback = defaultIcon(env.id);
  return {
    icon: iconName(env.icon) ?? fallback.icon,
    color: env.color && /^#[0-9a-f]{6}$/i.test(env.color) ? env.color : fallback.color,
  };
}

export const projectName = (path: string): string => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;

export function GlyphSvg({ glyph }: { glyph: Glyph }) {
  const Icon = iconFor(glyph) ?? Folder;
  return <Icon aria-hidden="true" focusable="false" />;
}

/** The coloured tile. Decorative; whatever wraps it supplies the accessible name. */
export function ProjectIcon({ choice, size = "normal" }: { choice: ProjectIconChoice; size?: "normal" | "small" }) {
  return (
    <span className={`project-icon ${size}`} style={{ background: choice.color }}>
      <GlyphSvg glyph={choice.icon} />
    </span>
  );
}
