// The environment window's layout rules (docs/plans/ui-polish.md, stage 9): sections use the shared
// Field, the Danger zone is one danger Notice with one destructive button, and agent and folder
// lists are rows at --row-h. Source checks, like the section tests beside this one. Run with
// `pnpm test` in apps/desktop.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (file: string) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");

const FILES = [
  "environments/EnvironmentWindow.tsx",
  "panels/EnvironmentAgents.tsx",
  "panels/EnvironmentConfigSection.tsx",
  "panels/EnvironmentRuntimeFields.tsx",
  "panels/AgentInitiative.tsx",
  "panels/KnowledgeSection.tsx",
];

test("labelled inputs go through Field, not a bare aria-label", () => {
  for (const file of FILES) {
    const source = read(file);
    // The only inputs that keep an aria-label are the icon search and the knowledge ask box (no visible label).
    const named = [...source.matchAll(/<(?:input|textarea)\b[^>]*aria-label=/g)].length;
    assert.ok(named <= 2, `${file}: ${named} inputs carry an aria-label instead of a Field label`);
  }
  assert.ok(read("environments/EnvironmentWindow.tsx").includes('<Field label="Name"'));
  assert.ok(/<Field\s+label="Role"/.test(read("panels/EnvironmentAgents.tsx")));
});

test("errors and notes are Notices, not .error paragraphs", () => {
  for (const file of FILES)
    assert.ok(!/className="[^"]*\berror\b/.test(read(file)), `${file} draws an .error paragraph`);
});

test("the Danger zone is a danger Notice with one destructive button", () => {
  const source = read("environments/EnvironmentWindow.tsx");
  const danger = source.slice(source.indexOf('current.id === "danger"'));
  assert.ok(danger.includes('<Notice\n            tone="danger"'));
  // Asking and confirming each show one destructive (danger) button; Keep is the neutral way out.
  assert.equal((danger.match(/className="danger"/g) ?? []).length, 2);
  assert.ok(!danger.includes("<Row"));
});

test("agent and folder lists are rows at --row-h", () => {
  assert.ok(read("panels/EnvironmentAgents.tsx").includes('className="environment-list"'));
  assert.ok(read("environments/EnvironmentWindow.tsx").includes('className="environment-list"'));
  const css = read("styles/environments.css");
  assert.ok(/\.environment-list > li {[\s\S]*?min-height: var\(--row-h\)/.test(css));
});

test("the runtime folder path input is named, and a Field never wraps a non-control", () => {
  const source = read("environments/EnvironmentWindow.tsx");
  const input = /<input[^>]*aria-label="Folder path"[^>]*>/.exec(source);
  assert.ok(input, "the folder path input has an accessible name");
  assert.ok(!/<Field[^>]*label="Folder path"/.test(source));
  // Every Field wraps an input, textarea or Select directly, so its label points at a control.
  for (const file of FILES) {
    for (const match of read(file).matchAll(/<Field[^>]*>\s*<(\w+)/g)) {
      assert.ok(["input", "textarea", "Select"].includes(match[1]), `${file}: a Field wraps <${match[1]}>`);
    }
  }
});
