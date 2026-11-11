// Changes by agents and install provenance (src/configChanges.ts). Run with `pnpm test`.

import assert from "node:assert/strict";
import { test } from "node:test";

import type { ConfigChange, InstallSource } from "@imperium/protocol";

import { changeDetails, installedByAgent, sourceLabel, undoLabel } from "../src/configChanges.ts";

const source: InstallSource = {
  kind: "git",
  location: "https://example.com/skills.git",
  revision: "0123456789abcdef0123",
  requested: "main",
  subdir: "pdf",
  installed_at_unix_ms: 1,
  session_id: "s1",
};

const base: ConfigChange = {
  id: 1,
  at_unix_ms: 1,
  session_id: "s1",
  kind: "install",
  summary: "Installed the skill pdf",
  model: null,
  model_kind: null,
  before: null,
  after: null,
  extension_kind: "skill",
  name: "pdf",
  path: "/config/skills/pdf",
  source,
  disabled: false,
  kept_at: null,
  undone_at_unix_ms: null,
};

test("a source shows its URL, folder and short commit", () => {
  assert.equal(sourceLabel(source), "https://example.com/skills.git (folder pdf) at 0123456789ab");
  assert.equal(sourceLabel({ ...source, kind: "path", location: "D:/skills/pdf", revision: null, subdir: null }), "D:/skills/pdf");
});

test("installs list where they came from; cards list before and after", () => {
  assert.deepEqual(changeDetails(base), ["From https://example.com/skills.git (folder pdf) at 0123456789ab", "/config/skills/pdf"]);
  const card: ConfigChange = { ...base, kind: "model_card", source: null, path: null, before: null, after: "Good at:\n coding" };
  assert.deepEqual(changeDetails(card), ["Before: none", "After: Good at: coding"]);
  assert.equal(undoLabel(card), "Remove this card");
  assert.equal(undoLabel(base), "Uninstall the skill");
  assert.equal(undoLabel({ ...base, kind: "uninstall" }), "Restore the skill");
});

test("an extension is the agent's when its source names a session", () => {
  assert.equal(installedByAgent({ installed_from: source }), true);
  assert.equal(installedByAgent({ installed_from: { ...source, session_id: null } }), false);
  assert.equal(installedByAgent({ installed_from: null }), false);
  assert.equal(installedByAgent({}), false);
});
