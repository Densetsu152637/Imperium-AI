// Nothing a mod, a mod's frame or the mod host can send reaches a terminal (terminal panes, D2,
// D7 and D15 of docs/plans/terminal-file-panes.md). Terminals are only for the user's keystrokes
// in the desktop app; these tests read the sources that define what a mod may call.

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../..", import.meta.url));

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

/** The daemon's terminal methods and the service calls behind them. */
const NAMES =
  /terminal[._](open|create|input|attach|detach|list|resize|close|output|changed)|open_terminal|attach_terminal|close_terminal|TerminalService/;

test("the mod host's request allowlist has no terminal call", () => {
  const frame = readFileSync(join(root, "apps/desktop/src/mods/ModFrame.tsx"), "utf8");
  const start = frame.indexOf("const REQUEST_PERMISSION");
  assert.ok(start >= 0, "the allowlist moved: update this test");
  const allowlist = frame.slice(start, frame.indexOf("};", start));
  assert.ok(allowlist.includes('"sessions.list"'));
  assert.ok(!/terminal/i.test(allowlist), allowlist);
  // The frame answers only what the allowlist names.
  assert.match(frame, /unknown method/);
});

test("nothing a mod is built from, or allowed, names a terminal call", () => {
  const folders = ["apps/desktop/src/mods", "packages/mod-sdk", "apps/desktop/src-tauri/mods", "apps/desktop/src-tauri/src/mod_frames.rs"];
  for (const folder of folders) {
    const files = statSync(join(root, folder)).isDirectory() ? sources(folder) : [join(root, folder)];
    for (const file of files) {
      assert.ok(!NAMES.test(readFileSync(file, "utf8")), `${file} names a terminal call`);
    }
  }
  // The permissions a mod may ask for are a closed list in the daemon, with none for terminals.
  const mods = readFileSync(join(root, "crates/imperium-core/src/mods.rs"), "utf8");
  assert.ok(!NAMES.test(mods.slice(0, mods.indexOf("#[cfg(test)]"))));
});
