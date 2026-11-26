// The few daemon calls only the user's own process may make (docs/05-tools-and-isolation.md):
// `config.set` of a runtime binary, path, argument or update-policy key or of
// `mcp.trusted_projects`, `runtime.set_server` with a path, and `models.set_config` with
// `extra_args`. The webview's own connection is loopback TCP, which the daemon treats like a
// remote client for these, so inside the desktop app they go through the app's native
// `user_call` command, which makes them over the daemon's local socket. Anywhere else (a browser
// preview, a test) they are ordinary calls.

import { invoke, isTauri } from "@tauri-apps/api/core";

/** The Tauri command bridge, injected so a test can stand in for it. */
export type Bridge = { inside: boolean; invoke: (command: string, args: Record<string, unknown>) => Promise<unknown> };

/** The daemon client's call. */
export type Caller = { call<Result>(method: string, params?: unknown): Promise<Result> };

/** The real bridge: native inside the desktop app. */
export const appBridge = (): Bridge => ({ inside: isTauri(), invoke });

/** Makes `method` as the user: through the app's `user_call` command inside the app, else directly. */
export async function callAsUser<Result>(client: Caller, method: string, params: unknown, bridge: Bridge = appBridge()): Promise<Result> {
  if (!bridge.inside) return client.call<Result>(method, params);
  return (await bridge.invoke("user_call", { method, params })) as Result;
}
