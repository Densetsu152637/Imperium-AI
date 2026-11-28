// Taking over a session's browser from the Browser panel, free of React so tests can run it
// (test/browserTakeover.test.ts). The daemon answers `session.browser_control` and
// `session.browser_act` only to the desktop app's own process on its local socket: the browser is
// signed in to what the user signed in to, and a driven browser asks no approval. So the panel
// goes through the app's own `browser_control` and `browser_act` commands
// (apps/desktop/src-tauri/src/browser_takeover.rs), never through its daemon client, whose
// loopback connection any mod's page could also open.

import type { BrowserAction, BrowserActResult, BrowserControlResult } from "@imperium/protocol";

/** The Tauri command bridge, injected so a test can stand in for it. */
export type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

/** Takes the session's browser over (`user` true) or gives it back to the agent. */
export async function controlBrowser(invoke: Invoke, sessionId: string, user: boolean): Promise<BrowserControlResult> {
  return (await invoke("browser_control", { session: sessionId, user })) as BrowserControlResult;
}

/** Does what the user did in the session's browser while driving it. */
export async function actInBrowser(invoke: Invoke, sessionId: string, action: BrowserAction): Promise<BrowserActResult> {
  return (await invoke("browser_act", { session: sessionId, action })) as BrowserActResult;
}
