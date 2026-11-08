// Project icons: a small glyph on a coloured tile. Each project gets a default derived from its
// path, and the user's own choices are remembered per viewer in local storage.

import {
  BookOpen,
  Briefcase,
  Camera,
  ChartColumn,
  Code,
  FlaskConical,
  Folder,
  Globe,
  Music,
  Rocket,
  Star,
  Zap,
  type LucideIcon,
} from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";

/** Glyphs, from the same line-icon set as the rest of the app. The names are what is saved. */
const GLYPHS = {
  folder: Folder,
  code: Code,
  book: BookOpen,
  flask: FlaskConical,
  rocket: Rocket,
  star: Star,
  briefcase: Briefcase,
  chart: ChartColumn,
  music: Music,
  camera: Camera,
  globe: Globe,
  bolt: Zap,
} satisfies Record<string, LucideIcon>;

export type Glyph = keyof typeof GLYPHS;
export const GLYPH_NAMES = Object.keys(GLYPHS) as Glyph[];

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
    return Object.fromEntries(Object.entries(saved).filter(([, choice]) => choice && choice.icon in GLYPHS));
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

export const isGlyph = (name: string | null | undefined): name is Glyph => !!name && name in GLYPHS;

/** An environment's icon: its own choice when set, else a default derived from its id. */
export function environmentIcon(env: { id: string; icon: string | null; color: string | null }): ProjectIconChoice {
  const fallback = defaultIcon(env.id);
  return {
    icon: isGlyph(env.icon) ? env.icon : fallback.icon,
    color: env.color && /^#[0-9a-f]{6}$/i.test(env.color) ? env.color : fallback.color,
  };
}

export const projectName = (path: string): string => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;

export function GlyphSvg({ glyph }: { glyph: Glyph }) {
  const Icon = GLYPHS[glyph];
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

/** A grid of glyphs and colour swatches for one project. Changes apply as they are picked. */
export function IconPicker({
  name,
  choice,
  onChange,
  onReset,
  onClose,
}: {
  name: string;
  choice: ProjectIconChoice;
  onChange: (choice: ProjectIconChoice) => void;
  onReset: () => void;
  onClose: () => void;
}) {
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    root.current?.querySelector<HTMLButtonElement>("button[aria-pressed='true']")?.focus();
  }, []);
  return (
    <div
      ref={root}
      className="icon-picker"
      role="dialog"
      aria-label={`Icon for ${name}`}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="icon-grid" role="group" aria-label="Glyph">
        {GLYPH_NAMES.map((glyph) => (
          <button
            key={glyph}
            title={glyph}
            aria-label={glyph}
            aria-pressed={glyph === choice.icon}
            onClick={() => onChange({ ...choice, icon: glyph })}
          >
            <ProjectIcon choice={{ icon: glyph, color: choice.color }} size="small" />
          </button>
        ))}
      </div>
      <div className="swatches" role="group" aria-label="Colour">
        {ICON_COLORS.map((color, index) => (
          <button
            key={color}
            className="swatch"
            style={{ background: color }}
            aria-label={`Colour ${index + 1}`}
            aria-pressed={color === choice.color}
            onClick={() => onChange({ ...choice, color })}
          />
        ))}
      </div>
      <div className="icon-picker-actions">
        <button onClick={onReset}>Reset</button>
        <button onClick={onClose}>Done</button>
      </div>
    </div>
  );
}
