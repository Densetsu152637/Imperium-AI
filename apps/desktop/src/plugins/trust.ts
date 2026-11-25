// The trust view's rules, free of React so tests can run them (test/pluginsTrust.test.ts): what a
// preview says a plugin runs, as plain lines; the difference an update makes; and the app's
// `plugin_approve` command, the only way the webview asks for an approval. Every string comes from a
// plugin or its marketplace, so each passes through `plain`.

import type { PluginHookRun, PluginPreview, PluginServerRun } from "@imperium/protocol";
import { plain, type Api } from "./model.ts";

export type ApproveAction = "install" | "update" | "approve";

/** What `plugin_approve` answers (apps/desktop/src-tauri/src/plugin_approval.rs `Outcome`). */
export type Outcome = { approved: boolean; kind: string | null; reason: string | null; cancelled: boolean; installed: boolean };

/** The Tauri command bridge, injected so a test can stand in for it. */
export type Invoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

/**
 * Asks the app to approve a plugin: its own native dialog, then the daemon's presence check. The
 * webview passes a name, an action and an environment, and gets the outcome; it never holds a
 * challenge or a hash of its own.
 */
export async function approvePlugin(invoke: Invoke, name: string, action: ApproveAction, environment?: string | null): Promise<Outcome> {
  const args: Record<string, unknown> = environment ? { name, action, environment } : { name, action };
  return (await invoke("plugin_approve", args)) as Outcome;
}

export const hookLine = (hook: PluginHookRun): string => {
  const what = hook.command ? plain(hook.command) : hook.url ? `HTTP ${plain(hook.url)}` : "";
  return `${plain(hook.event)} [${plain(hook.matcher) || "*"}]: ${what}${hook.launcher ? "  (LAUNCHER: fetches more code)" : ""}`;
};

export function serverLine(server: PluginServerRun): string {
  const where = server.command ? [server.command, ...server.args].map(plain).join(" ") : server.url ? `HTTP ${plain(server.url)}` : "";
  const names = [
    server.env_names.length > 0 ? `env: ${server.env_names.map(plain).join(", ")}` : null,
    server.header_names.length > 0 ? `headers: ${server.header_names.map(plain).join(", ")}` : null,
    server.variables.length > 0 ? server.variables.map((v) => `\${${plain(v)}}`).join(", ") : null,
  ].filter(Boolean);
  return `${plain(server.name)} (${plain(server.transport)}): ${where}${names.length > 0 ? `  [${names.join("; ")}]` : ""}${server.launcher ? "  (LAUNCHER: fetches more code)" : ""}`;
}

/** Every hook and server a preview lists, as plain lines (hooks first). */
export const runLines = (preview: PluginPreview) => ({ hooks: preview.hooks.map(hookLine), servers: preview.servers.map(serverLine) });

export type RunDiff =
  | { kind: "unchanged"; otherFilesChanged: boolean }
  | { kind: "changed"; added: string[]; removed: string[] }
  | { kind: "unknown" };

/**
 * How an update changes what the plugin runs. `before` is the installed version's preview (read at
 * its commit), or null when it could not be read. Equal run digests mean the hooks and servers are
 * the same, whatever else changed; otherwise the lines are compared as text. The commands name the
 * folder the plugin will live in, the same for both versions, so a path never makes a line differ.
 */
export function diffRuns(before: PluginPreview | null, after: PluginPreview): RunDiff {
  if (!before) return { kind: "unknown" };
  if (before.runs_digest === after.runs_digest) return { kind: "unchanged", otherFilesChanged: before.sha256 !== after.sha256 };
  const a = runLines(before);
  const b = runLines(after);
  const old = new Set([...a.hooks, ...a.servers]);
  const now = new Set([...b.hooks, ...b.servers]);
  return { kind: "changed", added: [...now].filter((l) => !old.has(l)), removed: [...old].filter((l) => !now.has(l)) };
}

export type TrustData = { preview: PluginPreview; diff: RunDiff | null };

/** Reads what the trust view shows: the marketplace's current preview and, for an update, the installed version's at its own commit. */
export async function loadTrust(api: Pick<Api, "inspect">, action: ApproveAction, id: string, installedCommit: string | null): Promise<TrustData> {
  const preview = await api.inspect(id);
  if (action !== "update") return { preview, diff: null };
  let before: PluginPreview | null = null;
  if (installedCommit) {
    try {
      before = await api.inspect(id, installedCommit);
    } catch {
      before = null;
    }
  }
  return { preview, diff: diffRuns(before, preview) };
}

/** Whether an update would change anything: the content equals what is installed. */
export const isUpToDate = (preview: PluginPreview): boolean => !!preview.installed?.same_content;

/** A plugin that runs nothing (skills and commands only) has nothing to approve. */
export const needsApproval = (preview: PluginPreview): boolean => preview.hooks.length > 0 || preview.servers.length > 0;

/** The reason a platform cannot approve at all, or null. Windows counts no approval before signed releases (U7). */
export function platformReason(userAgent: string): string | null {
  return /windows/i.test(userAgent) ? "Approvals do not count on Windows until signed releases. The plugin stays withheld." : null;
}

/** Whether an answer's reason says the platform cannot approve (the daemon's own wording), as opposed to a refused prompt. */
export const cannotApprove = (reason: string | null): boolean => !!reason && /no approval counts|cannot approve/i.test(reason);

export const KIND_TEXT: Record<string, string> = {
  counted: "Approved, with your device's check",
  "dialog-only": "Approved from the dialog only (no device check was available)",
  "carried-over": "Carried over from before approvals",
  dev: "Approved by a development build",
};

/** The label for a recorded approval kind, or "Not approved". */
export const kindLabel = (kind: string | null): string => (kind ? (KIND_TEXT[kind] ?? plain(kind)) : "Not approved");

export type Told = { tone: "ok" | "warn" | "info"; text: string };

/** What to tell the user about an answer. */
export function describeOutcome(outcome: Outcome): Told {
  if (outcome.approved) return { tone: "ok", text: `${kindLabel(outcome.kind)}.` };
  if (outcome.cancelled) return { tone: "info", text: "Cancelled. Nothing was changed." };
  const why = plain(outcome.reason) || "The approval was not recorded.";
  return { tone: "warn", text: outcome.installed ? `Installed but withheld: ${why}` : why };
}
