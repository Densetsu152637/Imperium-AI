// Deleting a session for good from its ⋯ menu (sessions/deleteSession.ts): the menu item is there for
// every session, running or archived; the dialog names the session and says it is permanent; and
// confirming goes only through the app's own command, with the session and nothing else.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { deleteConfirmation, deleteItem, eraseSession } from "../src/sessions/deleteSession.ts";

test("the menu offers Delete… for a running and an archived session alike", () => {
  const running = deleteItem({});
  const archived = deleteItem({ persistent: false });
  assert.equal(running.label, "Delete…");
  assert.equal(running.disabled, false);
  assert.equal(running.danger, true);
  assert.deepEqual(archived, running);
});

test("an environment's persistent agent shows the item off, saying why", () => {
  const item = deleteItem({ persistent: true });
  assert.equal(item.label, "Delete…");
  assert.equal(item.disabled, true);
  assert.match(item.title ?? "", /environment/);
});

test("the confirmation names the session and says the delete is permanent", () => {
  const words = deleteConfirmation("Fix the build", false);
  assert.match(words.heading, /Fix the build/);
  const text = words.lines.join(" ");
  assert.match(text, /permanently/);
  assert.match(text, /transcript/);
  assert.match(text, /cannot be undone/);
  assert.equal(words.confirm, "Delete session");
  assert.match(deleteConfirmation("Old work", true).lines.join(" "), /archived session/);
});

test("the confirmation shows a session's title as plain text, and a name when it has none", () => {
  assert.ok(!deleteConfirmation("a\u202eb\nc", false).heading.includes("\u202e"));
  assert.match(deleteConfirmation("  ", false).heading, /Untitled session/);
});

test("confirming asks the app's own command with the session only", async () => {
  const sent: Array<[string, Record<string, unknown>]> = [];
  await eraseSession(async (command, args) => {
    sent.push([command, args]);
    return { erased: true, reason: null };
  }, "s1");
  assert.deepEqual(sent, [["session_erase", { session: "s1" }]]);
});

test("a refusal rejects with the reason, so the dialog stays open and shows it", async () => {
  await assert.rejects(
    eraseSession(async () => ({ erased: false, reason: "only the desktop app can erase a session" }), "s1"),
    /only the desktop app/,
  );
  await assert.rejects(eraseSession(async () => ({ erased: false, reason: null }), "s1"), /could not be deleted/);
});

test("the webview never names the daemon's session.erase: only the app's own shell may send it", () => {
  const files = ["src/sessions/deleteSession.ts", "src/panels/DeleteSessionDialog.tsx", "src/panels/NavigationPanels.tsx", "src/App.tsx"];
  for (const file of files) {
    const text = readFileSync(fileURLToPath(new URL(`../${file}`, import.meta.url)), "utf8");
    // Comments may name it; code never calls it.
    const code = text.replace(/\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, "");
    assert.ok(!/["'`]session\.erase["'`]/.test(code), `${file} calls session.erase`);
  }
});
