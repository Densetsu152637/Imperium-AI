// The loading screen in index.html: what shows from the first paint until the panels have
// rendered, and the theme it (and each window's native background) uses before the saved
// settings are read.

/** `--bg` of each theme (styles.css); also the native window backgrounds (main.rs). */
export const BOOT_BACKGROUND = { dark: "#0b0c0e", light: "#f4f5f7" } as const;

/** localStorage key holding the last theme, read by the inline script in index.html. */
export const THEME_KEY = "imperium.theme";

/** How long the loading screen fades out. */
export const FADE_MS = 180;

/** The loading screen leaves after this even if the panels never report in, so it cannot stick. */
export const FALLBACK_MS = 8000;

/** Remembers the chosen theme ("system" included) for the next launch's first paint. */
export function rememberTheme(theme: string): void {
  try {
    localStorage.setItem(THEME_KEY, theme);
  } catch {
    // Unavailable storage only means the first paint follows the operating system.
  }
}

/** The theme to paint before the settings load: the remembered one, else the system's. */
export function bootTheme(remembered: string | null, systemLight: boolean): "light" | "dark" {
  if (remembered === "light" || remembered === "dark") return remembered;
  return systemLight ? "light" : "dark";
}

/** What the loading screen changes; the real DOM satisfies it, and tests pass fakes. */
export interface SplashParts {
  splash: { dataset: Record<string, string | undefined>; remove(): void } | null;
  /** The element marked `aria-busy` while loading. */
  busy: { removeAttribute(name: string): void } | null;
}

export interface SplashTimers {
  after(ms: number, run: () => void): unknown;
  cancel(handle: unknown): void;
}

/**
 * Arms the loading screen's fallback and returns how to dismiss it: the screen fades out
 * (`data-state="leaving"`), then leaves the page. Dismissing twice, or after the fallback has,
 * does nothing.
 */
export function startSplash(parts: SplashParts, timers: SplashTimers, fallbackMs = FALLBACK_MS): () => void {
  let done = false;
  let fallback: unknown;
  const dismiss = (): void => {
    if (done) return;
    done = true;
    timers.cancel(fallback);
    parts.busy?.removeAttribute("aria-busy");
    const splash = parts.splash;
    if (!splash) return;
    splash.dataset.state = "leaving";
    timers.after(FADE_MS, () => splash.remove());
  };
  fallback = timers.after(fallbackMs, dismiss);
  return dismiss;
}

let dismissSplash: (() => void) | null = null;

/** Arms the real loading screen; call first thing in the entry module. */
export function armSplash(): void {
  dismissSplash = startSplash(
    { splash: document.getElementById("boot") as HTMLElement | null, busy: document.body },
    { after: (ms, run) => setTimeout(run, ms), cancel: (handle) => clearTimeout(handle as number) },
  );
}

/** Runs `dismiss` after two frames, so the panels have painted first. */
export function afterPaint(dismiss: () => void, frame: (run: () => void) => void): void {
  frame(() => frame(dismiss));
}

/** The panels have rendered: fade the loading screen out (after the next paint). */
export function splashDone(): void {
  const dismiss = dismissSplash;
  if (!dismiss) return;
  if (typeof requestAnimationFrame === "function") afterPaint(dismiss, (run) => requestAnimationFrame(run));
  else dismiss();
}
