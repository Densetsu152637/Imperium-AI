// The trust view's rules (plugins/trust.ts): what a preview lists, how an update differs, and that
// the webview asks for an approval only through the app's own command.

import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import type { InstalledPlugin, PluginPreview } from "@imperium/protocol";

import { pluginsApi, type Caller } from "../src/plugins/model.ts";
import {
  approvalSource,
  approvePlugin,
  cannotApprove,
  describeOutcome,
  diffRuns,
  folderName,
  isUpToDate,
  kindLabel,
  loadFolderTrust,
  loadTrust,
  needsApproval,
  offersApprove,
  platformReason,
  runLines,
  type Invoke,
} from "../src/plugins/trust.ts";

const preview = (over: Partial<PluginPreview> = {}): PluginPreview => ({
  id: "fmt@official",
  name: "fmt",
  marketplace: "official",
  marketplace_source: "github:acme/official",
  marketplace_commit: "aaaa",
  source_kind: "path",
  source: "plugins/fmt",
  commit: "bbbb",
  description: "Formats code",
  version: "1.0.0",
  author: "Acme",
  homepage: null,
  license: "MIT",
  hooks: [
    { event: "PreToolUse", matcher: "Bash", command: "/cfg/plugins/fmt/check.sh --strict", url: null, launcher: false },
    { event: "PostToolUse", matcher: "*", command: "", url: "https://hooks.example/notify", launcher: false },
    { event: "Stop", matcher: "*", command: "npx fetch-more", url: null, launcher: true },
  ],
  servers: [
    { name: "db", transport: "stdio", command: "uvx", args: ["db-mcp", "--ro"], env_names: ["DB_USER"], url: null, header_names: [], variables: ["DB_TOKEN"], launcher: true },
    { name: "api", transport: "http", command: null, args: [], env_names: [], url: "https://api.example/mcp", header_names: ["Authorization"], variables: [], launcher: false },
  ],
  launcher: true,
  skills: ["fmt"],
  commands: ["fmt"],
  agents: [],
  commands_with_shell: [],
  sha256: "h1",
  runs_digest: "d1",
  installed: null,
  notes: [],
  ...over,
});

test("every hook and server of a preview is listed, hooks first, with variable names and launcher flags", () => {
  const { hooks, servers } = runLines(preview());
  assert.equal(hooks.length, 3);
  assert.equal(servers.length, 2);
  assert.ok(hooks[0]!.includes("/cfg/plugins/fmt/check.sh --strict") && hooks[0]!.includes("PreToolUse [Bash]"));
  assert.ok(hooks[1]!.includes("HTTP https://hooks.example/notify"));
  assert.ok(hooks[2]!.includes("npx fetch-more") && hooks[2]!.includes("LAUNCHER"));
  assert.ok(servers[0]!.includes("uvx db-mcp --ro") && servers[0]!.includes("env: DB_USER") && servers[0]!.includes("${DB_TOKEN}") && servers[0]!.includes("LAUNCHER"));
  assert.ok(servers[1]!.includes("HTTP https://api.example/mcp") && servers[1]!.includes("headers: Authorization"));
});

test("text from a plugin loses hidden and direction-changing characters", () => {
  const { hooks } = runLines(preview({ hooks: [{ event: "Stop", matcher: "*", command: "echo ‮evil​", url: null, launcher: false }] }));
  assert.equal(hooks[0], "Stop [*]: echo evil");
});

test("a plugin with no hook or server needs no approval", () => {
  assert.ok(needsApproval(preview()));
  assert.ok(!needsApproval(preview({ hooks: [], servers: [] })));
});

test("an update shows what changed in what runs, by digest first and then by line", () => {
  const before = preview({ hooks: preview().hooks.slice(0, 1), servers: [], runs_digest: "d0", sha256: "h0" });
  const after = preview();
  const changed = diffRuns(before, after);
  assert.equal(changed.kind, "changed");
  if (changed.kind === "changed") {
    assert.equal(changed.added.length, 4, "two more hooks and both servers");
    assert.deepEqual(changed.removed, []);
  }
  // Same digest, different content: only other files changed.
  assert.deepEqual(diffRuns(preview({ sha256: "h0" }), after), { kind: "unchanged", otherFilesChanged: true });
  assert.deepEqual(diffRuns(preview(), after), { kind: "unchanged", otherFilesChanged: false });
  // A reorder alone: the digest differs but every line is still there.
  const reordered = diffRuns(preview(), preview({ hooks: [...preview().hooks].reverse(), runs_digest: "d3" }));
  assert.deepEqual(reordered, { kind: "changed", added: [], removed: [] });
  // The installed version could not be read.
  assert.deepEqual(diffRuns(null, after), { kind: "unknown" });
  // A removed hook is listed as removed.
  const dropped = diffRuns(preview(), preview({ hooks: preview().hooks.slice(1), runs_digest: "d2" }));
  assert.equal(dropped.kind === "changed" && dropped.removed.length, 1);
});

test("an update reads the installed version at its own commit, and a failure there leaves the comparison unknown", async () => {
  const calls: unknown[] = [];
  const api = {
    inspect: async (id: string, revision?: string) => {
      calls.push({ id, revision });
      if (revision === "gone") throw new Error("no such commit");
      return revision ? preview({ runs_digest: "d0" }) : preview();
    },
  };
  const known = await loadTrust(api, "update", "fmt@official", "old");
  assert.deepEqual(calls, [{ id: "fmt@official", revision: undefined }, { id: "fmt@official", revision: "old" }]);
  assert.equal(known.diff?.kind, "changed");
  assert.equal((await loadTrust(api, "update", "fmt@official", "gone")).diff?.kind, "unknown");
  assert.equal((await loadTrust(api, "install", "fmt@official", null)).diff, null);
});

test("an update with nothing to change is up to date", () => {
  assert.ok(isUpToDate(preview({ installed: { path: "/p", recorded_sha256: "h1", same_content: true } })));
  assert.ok(!isUpToDate(preview({ installed: { path: "/p", recorded_sha256: "h0", same_content: false } })));
  assert.ok(!isUpToDate(preview()));
});

test("Approve invokes the app's plugin_approve with a name, an action and at most an environment, and nothing else", async () => {
  const calls: { command: string; args: Record<string, unknown> }[] = [];
  const answer = { approved: true, kind: "counted", reason: null, cancelled: false, installed: true };
  const invoke: Invoke = async (command, args) => {
    calls.push({ command, args });
    return answer;
  };
  assert.deepEqual(await approvePlugin(invoke, "fmt@official", "install"), answer);
  await approvePlugin(invoke, "fmt@official", "approve", "env1");
  assert.deepEqual(calls, [
    { command: "plugin_approve", args: { name: "fmt@official", action: "install" } },
    { command: "plugin_approve", args: { name: "fmt@official", action: "approve", environment: "env1" } },
  ]);
});

const row = (over: Partial<InstalledPlugin> = {}): InstalledPlugin => ({
  name: "tools",
  folder: "/cfg/plugins/my-tools",
  id: null,
  origin: null,
  source: null,
  commit: null,
  approval: null,
  enabled: true,
  decided_by: "default",
  withheld: true,
  reason: "not installed by Imperium: approve to load",
  hooks: 1,
  servers: [],
  required_secrets: [],
  missing_secrets: [],
  ...over,
});

test("a withheld folder with no marketplace id is approved by its folder name, and one with an id by its id", () => {
  assert.deepEqual(approvalSource(row()), { name: "my-tools", folder: true });
  assert.deepEqual(approvalSource(row({ folder: "C:\\Users\\me\\plugins\\my-tools\\" })), { name: "my-tools", folder: true });
  assert.deepEqual(approvalSource(row({ id: "fmt@official" })), { name: "fmt@official", folder: false });
  assert.equal(folderName("/a/b/c"), "c");
  assert.equal(folderName(""), "");
});

test("Approve is offered for a withheld folder that runs code, whether or not it came from a marketplace", () => {
  assert.ok(offersApprove(row(), null));
  assert.ok(offersApprove(row({ hooks: 0, servers: ["db"] }), null));
  assert.ok(offersApprove(row({ id: "fmt@official" }), null));
  // Not withheld, no code, or a platform that cannot approve.
  assert.ok(!offersApprove(row({ withheld: false }), null));
  assert.ok(!offersApprove(row({ hooks: 0 }), null));
  assert.ok(!offersApprove(row(), "Approvals do not count on Windows."));
});

test("the trust view for a folder lists what the daemon read from it and compares nothing", async () => {
  const calls: unknown[] = [];
  const api = {
    inspectFolder: async (name: string) => {
      calls.push(name);
      return preview({ id: name, marketplace: "", marketplace_source: "", marketplace_commit: null, source_kind: "folder", source: "/cfg/plugins/my-tools", commit: null, installed: { path: "/cfg/plugins/my-tools", recorded_sha256: null, same_content: true } });
    },
  };
  const data = await loadFolderTrust(api, "my-tools");
  assert.deepEqual(calls, ["my-tools"]);
  assert.equal(data.diff, null);
  assert.equal(data.preview.source_kind, "folder");
  assert.equal(runLines(data.preview).hooks.length, 3);
  // The installed folder is what is previewed, so it is neither stale nor up to date to update.
  assert.ok(needsApproval(data.preview) && isUpToDate(data.preview));
});

test("the approval of a folder goes through the app's own command by folder name", async () => {
  const calls: { command: string; args: Record<string, unknown> }[] = [];
  const invoke: Invoke = async (command, args) => {
    calls.push({ command, args });
    return { approved: true, kind: "counted", reason: null, cancelled: false, installed: false };
  };
  await approvePlugin(invoke, approvalSource(row()).name, "approve");
  assert.deepEqual(calls, [{ command: "plugin_approve", args: { name: "my-tools", action: "approve" } }]);
});

test("a mod frame cannot reach the plugin methods", () => {
  const text = readFileSync(fileURLToPath(new URL("../src/mods/ModFrame.tsx", import.meta.url)), "utf8");
  const table = /const REQUEST_PERMISSION[^=]*=\s*\{([^}]*)\}/.exec(text);
  assert.ok(table, "the mod frame's request table is found");
  const methods = [...table[1]!.matchAll(/"([^"]+)"\s*:/g)].map((match) => match[1]!);
  assert.ok(methods.length > 0);
  for (const method of methods) assert.doesNotMatch(method, /plugin/i, method);
  assert.doesNotMatch(text, /plugins\./);
  assert.doesNotMatch(text, /inspect_folder|inspectFolder/);
});

test("the webview never names the daemon's approval methods", () => {
  const found: string[] = [];
  const scan = (dir: string) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) scan(path);
      else if (/\.(tsx?|css|html)$/.test(entry.name)) {
        const text = readFileSync(path, "utf8");
        for (const name of ["plugins.approve", "plugins.approval_challenge", "approval_challenge", "PluginsApprove"]) if (text.includes(name)) found.push(`${path} names ${name}`);
      }
    }
  };
  scan(fileURLToPath(new URL("../src", import.meta.url)));
  assert.deepEqual(found, []);
});

test("a saved secret is never read back into the window, and its field is a password input", () => {
  const dir = fileURLToPath(new URL("../src/plugins", import.meta.url));
  for (const file of readdirSync(dir).filter((f) => f.endsWith(".tsx") || f.endsWith(".ts"))) {
    const text = readFileSync(join(dir, file), "utf8");
    assert.doesNotMatch(text, /\.secret\b/, `${file} reads a secret result's value`);
    assert.doesNotMatch(text, /\.value\b.*(setSecret|onSet)/, file);
  }
  assert.match(readFileSync(join(dir, "SecretsForm.tsx"), "utf8"), /type="password"/);
});

test("setting a secret sends the value once, and the result is not kept", async () => {
  const sent: unknown[] = [];
  const client: Caller = {
    async call<R>(method: string, params?: unknown) {
      sent.push({ method, params });
      return { name: "p", variable: "KEY", secret: "REFERENCE", changed: true } as R;
    },
  };
  await pluginsApi(client, null).setSecret("p", "KEY", "hunter2");
  assert.deepEqual(sent, [{ method: "plugins.set_secret", params: { name: "p", variable: "KEY", value: "hunter2" } }]);
});

test("answers: approved kinds, a cancel, a refusal, an install left withheld, and a platform that cannot approve", () => {
  assert.equal(describeOutcome({ approved: true, kind: "counted", reason: null, cancelled: false, installed: true }).tone, "ok");
  assert.match(describeOutcome({ approved: true, kind: "dialog-only", reason: null, cancelled: false, installed: true }).text, /dialog only/);
  assert.match(kindLabel("carried-over"), /Carried over/);
  assert.match(kindLabel("dev"), /development/);
  assert.equal(kindLabel(null), "Not approved");
  assert.match(describeOutcome({ approved: false, kind: null, reason: null, cancelled: true, installed: false }).text, /Nothing was changed/);
  const withheld = describeOutcome({ approved: false, kind: null, reason: "the presence check did not pass", cancelled: false, installed: true });
  assert.equal(withheld.tone, "warn");
  assert.match(withheld.text, /Installed but withheld: the presence check did not pass/);
  assert.ok(cannotApprove("no approval counts on Windows until signed per-machine releases: this is not a signed release build"));
  assert.ok(!cannotApprove("the presence check did not pass"));
  assert.match(platformReason("Mozilla/5.0 (Windows NT 10.0; Win64; x64)") ?? "", /Windows/);
  assert.equal(platformReason("Mozilla/5.0 (X11; Linux x86_64)"), null);
});

test("an agent's plugin or skill is approved whether or not it runs code, and a marketplace one without code is not", async () => {
  assert.ok(offersApprove({ withheld: true, hooks: 0, servers: [], origin: "agent" }, null));
  assert.ok(!offersApprove({ withheld: true, hooks: 0, servers: [], origin: "marketplace" }, null));
  assert.ok(!offersApprove({ withheld: false, hooks: 0, servers: [], origin: "agent" }, null));
  assert.ok(needsApproval(preview({ hooks: [], servers: [], source_kind: "skill" })));
  assert.ok(needsApproval(preview({ hooks: [], servers: [], source_kind: "folder" })));
  assert.ok(!needsApproval(preview({ hooks: [], servers: [], source_kind: "github" })));
  const sent: Record<string, unknown>[] = [];
  const invoke = async (_command: string, args: Record<string, unknown>) => {
    sent.push(args);
    return { approved: true, kind: "counted", reason: null, cancelled: false, installed: false };
  };
  await approvePlugin(invoke, "notes", "approve", undefined, "skill");
  await approvePlugin(invoke, "notes", "approve");
  assert.deepEqual(sent, [{ name: "notes", action: "approve", kind: "skill" }, { name: "notes", action: "approve" }]);
});
