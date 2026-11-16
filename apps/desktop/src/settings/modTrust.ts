// Which mods may run in the app's own window instead of a sandboxed frame. The daemon decides
// whether a mod is `trusted` (the user typed a confirmation at a terminal, for exactly this
// content; see docs/06-extensibility.md); this is the app's side of that rule. Main-context
// loading itself is not built yet, so every mod still runs in its sandboxed frame; this is the
// gate that loading will have to pass.

export type ModTrust = {
  /** The manifest asks to run in the app's own window. */
  main_context?: boolean;
  /** The user trusted it, for this content. */
  trusted?: boolean;
  /** Its files changed since the user approved it. */
  modified?: boolean;
};

/**
 * Whether a mod may run in the app's own window: it asks to, the user trusted it, its files are
 * what was trusted, and it is turned on. Turning it off in Settings → Mods revokes it.
 */
export function mayRunInMainContext(mod: ModTrust, enabled: boolean): boolean {
  return enabled && mod.main_context === true && mod.trusted === true && mod.modified !== true;
}
