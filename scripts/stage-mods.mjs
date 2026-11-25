// Copies what each bundled mod ships (its mod.json, entry, readme and notices) from mods/<id>/
// into apps/desktop/src-tauri/mods/<id>/, the folder the installers bundle. Run after the mods
// are built (pnpm run build:mods does both); a mod whose entry is missing fails the build rather
// than shipping without its code.

import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "mods");
const target = join(root, "apps", "desktop", "src-tauri", "mods");
const NOTICES = ["README.md", "NOTICE", "LICENSE"];

for (const entry of readdirSync(target)) {
  if (entry !== ".gitkeep") rmSync(join(target, entry), { recursive: true, force: true });
}

for (const dir of readdirSync(source, { withFileTypes: true })) {
  const from = join(source, dir.name);
  if (!dir.isDirectory() || !existsSync(join(from, "mod.json"))) continue;
  const manifest = JSON.parse(readFileSync(join(from, "mod.json"), "utf8"));
  const entry = manifest.entry ?? "index.js";
  if (!existsSync(join(from, entry))) {
    console.error(`mods/${dir.name}: ${entry} is missing; build the mod first`);
    process.exit(1);
  }
  const files = readdirSync(from).filter(
    (name) => name === "mod.json" || name === entry || NOTICES.some((notice) => name.startsWith(notice)),
  );
  mkdirSync(join(target, dir.name), { recursive: true });
  for (const name of files) copyFileSync(join(from, name), join(target, dir.name, name));
  console.log(`staged mods/${dir.name}: ${files.join(", ")}`);
}
