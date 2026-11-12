// Which sections the environment window shows, and that each section still renders its controls
// (the window itself needs a DOM, so the controls are checked against the sources that render them).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { SECTION_CONTROLS, initialSection, sectionsFor, type EnvironmentSectionId } from "../src/environmentSections.ts";

const ids = (kind: { creating: boolean; builtin: boolean }) => sectionsFor(kind).map((section) => section.id);

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
  // No delete to confirm for Home or a new environment.
  assert.equal(initialSection({ creating: false, builtin: true }, true), "general");
  assert.equal(initialSection({ creating: true, builtin: false }, true), "general");
});

test("every section is listed with controls, and each control is rendered by its source", () => {
  const all = sectionsFor({ creating: false, builtin: false }).map((section) => section.id);
  assert.deepEqual(Object.keys(SECTION_CONTROLS).sort(), [...all].sort());
  for (const id of all as EnvironmentSectionId[]) {
    const { file, controls } = SECTION_CONTROLS[id];
    assert.ok(controls.length > 0, `${id} lists controls`);
    const source = readFileSync(new URL(`../src/${file}`, import.meta.url), "utf8");
    for (const control of controls) assert.ok(source.includes(control), `${id}: ${file} renders "${control}"`);
  }
});

test("the window is built on the shared modal", () => {
  const source = readFileSync(new URL("../src/EnvironmentWindow.tsx", import.meta.url), "utf8");
  assert.ok(source.includes("<SettingsModal"));
  assert.ok(!source.includes("createPortal"));
});
