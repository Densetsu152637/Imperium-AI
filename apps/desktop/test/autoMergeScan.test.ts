// Nothing a mod, a mod's frame or the app's webview can send turns auto-merge on. Turning it on is
// the daemon's `pull_requests.set_auto_merge` with `enabled: true` and a challenge from
// `pull_requests.auto_merge_challenge`, which only the desktop app's own process (its Rust shell,
// `auto_merge_enable`) may ask for. These tests read the sources that define what a mod may call.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../..", import.meta.url));

/** Every text file under `dir` (relative to the repository), skipping dependencies and builds. */
function sources(dir: string): string[] {
  const found: string[] = [];
  const walk = (path: string) => {
    for (const name of readdirSync(path)) {
      if (name === "node_modules" || name === "dist" || name === "target" || name === "gen") continue;
      const full = join(path, name);
      if (statSync(full).isDirectory()) walk(full);
      else if (/\.(ts|tsx|js|mjs|json|rs|html|md)$/.test(name)) found.push(full);
    }
  };
  walk(join(root, dir));
  return found;
}

const NAMES = /set_auto_merge|auto_merge_challenge|auto_merge_enable|automerge|PullRequestsSetAutoMerge|PullRequestsAutoMergeChallenge/i;

test("the mod host's request allowlist has no pull-request or auto-merge call", () => {
  const frame = readFileSync(join(root, "apps/desktop/src/mods/ModFrame.tsx"), "utf8");
  const start = frame.indexOf("const REQUEST_PERMISSION");
  assert.ok(start >= 0, "the allowlist moved: update this test");
  const allowlist = frame.slice(start, frame.indexOf("};", start));
  assert.ok(allowlist.includes('"sessions.list"'));
  assert.ok(!/pull_request|auto_merge|automerge/i.test(allowlist), allowlist);
  // The frame answers only what the allowlist names.
  assert.match(frame, /unknown method/);
});

test("nothing a mod is built from, or allowed, names the auto-merge calls", () => {
  const folders = ["apps/desktop/src/mods", "packages/mod-sdk", "apps/desktop/src-tauri/mods", "apps/desktop/src-tauri/src/mod_frames.rs"];
  for (const folder of folders) {
    const files = statSync(join(root, folder)).isDirectory() ? sources(folder) : [join(root, folder)];
    for (const file of files) {
      assert.ok(!NAMES.test(readFileSync(file, "utf8")), `${file} names an auto-merge call`);
    }
  }
  // The permissions a mod may ask for are a closed list in the daemon.
  const mods = readFileSync(join(root, "crates/imperium-core/src/mods.rs"), "utf8");
  assert.ok(!/pull_request|auto_merge|automerge/i.test(mods.slice(0, mods.indexOf("#[cfg(test)]"))));
});

test("the webview never asks for the challenge, and only the card turns it off", () => {
  const offenders: string[] = [];
  for (const file of sources("apps/desktop/src")) {
    const text = readFileSync(file, "utf8");
    if (/auto_merge_challenge|PullRequestsAutoMergeChallenge/.test(text)) offenders.push(`${file} asks for the challenge`);
    if (/set_auto_merge/.test(text) && !file.replace(/\\/g, "/").endsWith("src/sessions/autoMerge.ts")) offenders.push(`${file} calls set_auto_merge`);
  }
  assert.deepEqual(offenders, []);
  // The one webview call sends `enabled: false`.
  const call = readFileSync(join(root, "apps/desktop/src/sessions/autoMerge.ts"), "utf8");
  assert.match(call, /enabled: false/);
  assert.ok(!/enabled: true/.test(call));
});
