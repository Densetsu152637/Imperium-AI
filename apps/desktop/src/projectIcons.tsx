// Project icons: a small glyph on a coloured tile. Each project gets a default derived from its
// path, and the user's own choices are remembered per viewer in local storage.

import { useCallback, useEffect, useRef, useState } from "react";

/** Glyphs drawn on a 24×24 grid as stroked paths. */
const GLYPHS = {
  folder: ["M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"],
  code: ["M8 8l-4 4 4 4", "M16 8l4 4-4 4", "M14 5l-4 14"],
  book: ["M5 4h11a3 3 0 0 1 3 3v13H8a3 3 0 0 1-3-3z", "M5 17a3 3 0 0 1 3-3h11"],
  flask: ["M9 3h6", "M10 3v6l-5 9a2 2 0 0 0 1.7 3h10.6a2 2 0 0 0 1.7-3l-5-9V3", "M7.5 15h9"],
  rocket: [
    "M12 3c3 2 5 5.5 5 9.5L15 17H9l-2-4.5C7 8.5 9 5 12 3z",
    "M9 17l-2 4",
    "M15 17l2 4",
    "M13.5 10a1.5 1.5 0 1 1-3 0a1.5 1.5 0 1 1 3 0",
  ],
  star: ["M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1-4.4-4.3 6.1-.9z"],
  briefcase: ["M4 8h16v11H4z", "M9 8V5h6v3", "M4 13h16"],
  chart: ["M4 20h16", "M7 16v-5", "M12 16V7", "M17 16V9"],
  music: ["M9 18V5l11-2v13", "M9 18a3 3 0 1 1-6 0a3 3 0 1 1 6 0", "M20 16a3 3 0 1 1-6 0a3 3 0 1 1 6 0"],
  camera: ["M4 8h3l2-3h6l2 3h3v11H4z", "M15.5 13a3.5 3.5 0 1 1-7 0a3.5 3.5 0 1 1 7 0"],
  globe: [
    "M21 12a9 9 0 1 1-18 0a9 9 0 1 1 18 0",
    "M3 12h18",
    "M12 3c2.5 2.5 3.5 5.5 3.5 9s-1 6.5-3.5 9c-2.5-2.5-3.5-5.5-3.5-9s1-6.5 3.5-9",
  ],
  bolt: ["M13 3L5 14h6l-1 7 8-11h-6z"],
} satisfies Record<string, string[]>;

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

export const projectName = (path: string): string => path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;

export function GlyphSvg({ glyph }: { glyph: Glyph }) {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      {GLYPHS[glyph].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
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
