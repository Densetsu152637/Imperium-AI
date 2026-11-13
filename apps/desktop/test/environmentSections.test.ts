// Which sections the environment window shows, how it saves and closes, and that each section
// still renders its controls (the window itself needs a DOM, so controls are checked against the
// sources that render them).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { agentDraftDirty, draftOfPermissions, suggestionsLabel } from "../src/agents.ts";
import {
  closeAction,
  environmentDirty,
  footerSaveBlocked,
  initialSection,
  paneHidden,
  saveTarget,
  sectionsFor,
  type EnvironmentSectionId,
} from "../src/environmentSections.ts";

const ids = (kind: { creating: boolean; builtin: boolean }) => sectionsFor(kind).map((section) => section.id);
const read = (file: string) => readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");

test("an existing environment shows every section, in order", () => {
  assert.deepEqual(ids({ creating: false, builtin: false }), [
    "general",
    "folders",
    "agents",
    "memory",
    "config",
    "permissions",
    "danger",
  ]);
});

test("a new environment has no working folder, configuration file or delete", () => {
  assert.deepEqual(ids({ creating: true, builtin: false }), ["general", "folders", "agents", "permissions"]);
});

test("Home cannot be deleted but keeps its other sections", () => {
  assert.deepEqual(ids({ creating: false, builtin: true }), [
    "general",
    "folders",
    "agents",
    "memory",
    "config",
    "permissions",
  ]);
});

test("the window opens on General, or on the Danger zone to confirm a delete", () => {
  assert.equal(initialSection({ creating: false, builtin: false }, false), "general");
  assert.equal(initialSection({ creating: false, builtin: false }, true), "danger");
  assert.equal(initialSection({ creating: false, builtin: true }, true), "general");
  assert.equal(initialSection({ creating: true, builtin: false }, true), "general");
});

test("Ctrl+S saves the open agent form; the footer Save waits while it is dirty", () => {
  assert.equal(saveTarget(true, true), "agent");
  assert.equal(saveTarget(true, false), "environment", "an open but clean form leaves the shortcut to the environment");
  assert.equal(saveTarget(false, false), "environment");
  assert.equal(footerSaveBlocked(true), true);
  assert.equal(footerSaveBlocked(false), false);
});

test("an agent form is dirty once something differs from what it opened with", () => {
  const blank = draftOfPermissions(undefined);
  const form = (name: string, role: string, local = false) => ({
    name,
    role,
    access: { ...blank, localOnly: local },
    initial: blank,
  });
  assert.equal(agentDraftDirty(form("", ""), null), false);
  assert.equal(agentDraftDirty(form("Planner", ""), null), true);
  assert.equal(agentDraftDirty(form("", "", true), null), true);
  assert.equal(agentDraftDirty(form("Planner", "Plan"), { name: "Planner", role: "Plan" }), false);
  assert.equal(agentDraftDirty(form("Planner", "Other"), { name: "Planner", role: "Plan" }), true);
});

test("switching sections only hides the Agents pane, so its state is kept", () => {
  const all: EnvironmentSectionId[] = ["general", "folders", "agents", "permissions"];
  assert.equal(paneHidden("agents", "agents"), false);
  for (const other of all.filter((id) => id !== "agents")) assert.equal(paneHidden(other, "agents"), true);
  const source = read("EnvironmentWindow.tsx");
  assert.ok(/<div hidden=\{paneHidden\(current\.id, "agents"\)\}>\s*<EnvironmentAgents/.test(source));
  assert.ok(!source.includes('current.id === "agents"'));
});

test("closing with unsaved edits asks first, and asking again discards", () => {
  assert.equal(closeAction(false, false), "close");
  assert.equal(closeAction(true, false), "confirm");
  assert.equal(closeAction(true, true), "close");
  // Esc never discards: with nothing unsaved it closes, but while asking it only dismisses the question.
  assert.equal(closeAction(false, false, true), "close");
  assert.equal(closeAction(true, false, true), "confirm");
  assert.equal(closeAction(true, true, true), "dismiss");
  assert.equal(environmentDirty({ name: false, agent: false }), false);
  assert.equal(environmentDirty({ name: false, agent: true }), true);
});

test("suggestions waiting show as a badge", () => {
  assert.equal(suggestionsLabel(0), null);
  assert.equal(suggestionsLabel(1), "1 suggestion");
  assert.equal(suggestionsLabel(3), "3 suggestions");
});

/** The controls each section holds (accessible names or text) and the source that renders them. */
const SECTION_CONTROLS: Record<EnvironmentSectionId, { file: string; controls: string[] }> = {
  general: {
    file: "EnvironmentWindow.tsx",
    controls: ["Environment name", "Search icons", 'aria-label="Icon"', 'aria-label="Colour"', 'aria-label="Pet"'],
  },
  folders: {
    file: "EnvironmentWindow.tsx",
    controls: ["Add folder", "Folder inside the runtime", "Add path", "Remove ${folder}", "<EnvironmentRuntimeFields"],
  },
  agents: {
    file: "panels/EnvironmentAgents.tsx",
    controls: [
      "Add agent",
      "Edit ${agent.name}",
      "Delete ${agent.name}",
      "<AgentInitiative",
      "Local files only",
      "MCP servers",
      "Plugins",
      "Save agent",
    ],
  },
  memory: { file: "EnvironmentWindow.tsx", controls: ["Open Memory.md", "Open folder"] },
  config: { file: "panels/EnvironmentConfigSection.tsx", controls: ["Reload", "Applied", "Refused"] },
  permissions: { file: "EnvironmentWindow.tsx", controls: ["<PermissionsEditor"] },
  danger: { file: "EnvironmentWindow.tsx", controls: ["Delete environment", "Keep"] },
};

test("every section lists controls, and each control is rendered by its source", () => {
  const all = sectionsFor({ creating: false, builtin: false }).map((section) => section.id);
  assert.deepEqual(Object.keys(SECTION_CONTROLS).sort(), [...all].sort());
  for (const id of all) {
    const { file, controls } = SECTION_CONTROLS[id];
    const source = read(file);
    for (const control of controls) assert.ok(source.includes(control), `${id}: ${file} renders "${control}"`);
  }
});

test("the window is built on the shared modal and asks before discarding", () => {
  const source = read("EnvironmentWindow.tsx");
  assert.ok(source.includes("<SettingsModal"));
  assert.ok(!source.includes("createPortal"));
  assert.ok(source.includes("Discard changes?"));
  assert.ok(source.includes("if (!event.repeat) requestClose(true)"));
});
