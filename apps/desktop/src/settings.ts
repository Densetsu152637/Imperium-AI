// Display settings: three independent scales plus readable line length. Stored per viewer in
// local storage for now; they move into the daemon's configuration with the settings UI.

export type Density = "compact" | "comfortable" | "spacious";

export type DisplaySettings = {
  uiScale: number;
  proseScale: number;
  codeScale: number;
  density: Density;
  maxProseCh: number;
};

export const DEFAULT_SETTINGS: DisplaySettings = {
  uiScale: 1,
  proseScale: 1,
  codeScale: 1,
  density: "comfortable",
  maxProseCh: 90,
};

const KEY = "imperium.display";
const DENSITY_SPACE: Record<Density, number> = { compact: 0.7, comfortable: 1, spacious: 1.35 };

export function loadSettings(): DisplaySettings {
  try {
    const saved = localStorage.getItem(KEY);
    return saved ? { ...DEFAULT_SETTINGS, ...(JSON.parse(saved) as Partial<DisplaySettings>) } : DEFAULT_SETTINGS;
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function saveSettings(settings: DisplaySettings): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(settings));
  } catch {
    // Unavailable storage only means the settings are not remembered.
  }
}

/** Writes the settings to the CSS custom properties every component sizes itself from. */
export function applySettings(settings: DisplaySettings): void {
  const root = document.documentElement.style;
  root.setProperty("--ui-scale", String(settings.uiScale));
  root.setProperty("--prose-scale", String(settings.proseScale));
  root.setProperty("--code-scale", String(settings.codeScale));
  root.setProperty("--space-scale", String(DENSITY_SPACE[settings.density]));
  root.setProperty("--prose-max", `${settings.maxProseCh}ch`);
}
