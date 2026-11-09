// The desktop app's own settings: appearance, chat, notifications, startup, and updates. They are kept in
// the local `settings` document (store.ts), so they persist across versions; fields a newer
// version added are filled from the defaults, and fields an older one does not know are kept.
// Daemon settings (models, runtimes, tools) live in the daemon's config file instead.

import { createContext, useContext } from "react";

import { getDoc, setDoc, takeLegacy } from "./store";

export type Density = "compact" | "comfortable" | "spacious";
export type Accent =
  | "cyan"
  | "teal"
  | "green"
  | "lime"
  | "amber"
  | "orange"
  | "rose"
  | "magenta"
  | "violet"
  | "indigo"
  | "blue"
  | "silver";
/** Days between automatic update checks. */
export type UpdateInterval = 1 | 7 | 28;
export type ProseFont = "sans" | "serif" | "mono";
export type SendKey = "enter" | "mod-enter";

export type AppSettings = {
  // Appearance
  uiScale: number;
  proseScale: number;
  codeScale: number;
  density: Density;
  maxProseCh: number;
  accent: Accent;
  proseFont: ProseFont;
  reduceMotion: boolean;
  // Chat
  sendKey: SendKey;
  renderMarkdown: boolean;
  wrapCode: boolean;
  autoScroll: boolean;
  // Notifications
  soundOnApproval: boolean;
  soundOnFinish: boolean;
  titleBadge: boolean;
  // Startup
  restoreSession: boolean;
  autoStartDaemon: boolean;
  // Updates (read by the native side at startup too; see src-tauri/src/update.rs)
  updateIntervalDays: UpdateInterval;
  checkUpdatesAtLogin: boolean;
  // Mods: by id; a mod without an entry is on only if the user installed it (not bundled).
  enabledMods: Record<string, boolean>;
};

/** The settings each display control used to be called. */
export type DisplaySettings = AppSettings;

export const DEFAULT_SETTINGS: AppSettings = {
  uiScale: 1,
  proseScale: 1,
  codeScale: 1,
  density: "comfortable",
  maxProseCh: 90,
  accent: "cyan",
  proseFont: "sans",
  reduceMotion: false,
  sendKey: "enter",
  renderMarkdown: true,
  wrapCode: false,
  autoScroll: true,
  soundOnApproval: true,
  soundOnFinish: false,
  titleBadge: true,
  restoreSession: true,
  autoStartDaemon: true,
  updateIntervalDays: 1,
  checkUpdatesAtLogin: false,
  enabledMods: {},
};

/** The saved document: the settings plus a version for future migrations. */
type Saved = Partial<AppSettings> & { version?: number };

const VERSION = 1;

export function loadSettings(): AppSettings {
  let saved = getDoc<Saved>("settings");
  if (!saved) {
    // Before version 1 only the display settings existed, in local storage.
    saved = takeLegacy<Saved>("imperium.display") ?? {};
    setDoc("settings", { ...saved, version: VERSION });
  }
  return { ...DEFAULT_SETTINGS, ...saved };
}

export function saveSettings(settings: AppSettings): void {
  // Keep fields this version does not know, so a downgrade then upgrade loses nothing.
  setDoc("settings", { ...getDoc<Saved>("settings"), ...settings, version: VERSION });
}

/** Bundled mods that are on until the user turns them off; other bundled mods are opt-in. */
export const BUNDLED_ON_BY_DEFAULT = ["imperium-nursery"];

/** Whether a mod's panels show: the user's choice, else on for installed mods and the bundled ones above. */
export function modEnabled(settings: AppSettings, mod: { id: string; bundled: boolean }): boolean {
  return settings.enabledMods[mod.id] ?? (!mod.bundled || BUNDLED_ON_BY_DEFAULT.includes(mod.id));
}

const DENSITY_SPACE: Record<Density, number> = { compact: 0.7, comfortable: 1, spacious: 1.35 };

/**
 * Accent colours, in hue order for the swatch grid: the base, a brighter variant for hover, and
 * (computed) a translucent wash. Each is light enough to read on the dark surfaces.
 */
export const ACCENTS: Record<Accent, { label: string; base: string; strong: string }> = {
  cyan: { label: "Cyan", base: "#1fb2c8", strong: "#45c6d8" },
  teal: { label: "Teal", base: "#22b39a", strong: "#4ccab4" },
  green: { label: "Green", base: "#3fbf7f", strong: "#62d39a" },
  lime: { label: "Lime", base: "#9cc43a", strong: "#b5d965" },
  amber: { label: "Amber", base: "#e0a33a", strong: "#f0bd5f" },
  orange: { label: "Orange", base: "#e57a3c", strong: "#f09763" },
  rose: { label: "Rose", base: "#e0607e", strong: "#ec8399" },
  magenta: { label: "Magenta", base: "#cf62c4", strong: "#de86d5" },
  violet: { label: "Violet", base: "#9b7cf0", strong: "#b49cf5" },
  indigo: { label: "Indigo", base: "#6f7bea", strong: "#939cf2" },
  blue: { label: "Blue", base: "#3d8fe0", strong: "#66a9ec" },
  silver: { label: "Silver", base: "#b9b3a7", strong: "#d9d4ca" },
};

const PROSE_FONTS: Record<ProseFont, string> = {
  sans: "var(--font-ui)",
  serif: 'Charter, "Iowan Old Style", Georgia, Cambria, serif',
  mono: "var(--font-code)",
};

function wash(hex: string, alpha: number): string {
  const value = Number.parseInt(hex.slice(1), 16);
  return `rgb(${(value >> 16) & 255} ${(value >> 8) & 255} ${value & 255} / ${alpha})`;
}

/** Writes the settings to the CSS custom properties and classes every component styles from. */
export function applySettings(settings: AppSettings): void {
  const root = document.documentElement;
  const style = root.style;
  style.setProperty("--ui-scale", String(settings.uiScale));
  style.setProperty("--prose-scale", String(settings.proseScale));
  style.setProperty("--code-scale", String(settings.codeScale));
  style.setProperty("--space-scale", String(DENSITY_SPACE[settings.density]));
  style.setProperty("--prose-max", `${settings.maxProseCh}ch`);
  const accent = ACCENTS[settings.accent] ?? ACCENTS.cyan;
  style.setProperty("--accent", accent.base);
  style.setProperty("--accent-strong", accent.strong);
  style.setProperty("--accent-soft", wash(accent.base, 0.15));
  style.setProperty("--font-prose", PROSE_FONTS[settings.proseFont] ?? PROSE_FONTS.sans);
  root.classList.toggle("reduce-motion", settings.reduceMotion);
  root.classList.toggle("wrap-code", settings.wrapCode);
}

/** The current settings, for components that follow them (chat input, rendering). */
export const SettingsContext = createContext<AppSettings>(DEFAULT_SETTINGS);
export const useSettings = () => useContext(SettingsContext);
