// The model panels and the model configuration dialog sit in narrow side columns and in a dialog, on the
// shared components. These checks keep them there: no native <select>, no bare error or empty
// paragraphs, no fixed minimum width that would clip a control in a 255 px column, and no literals left in
// the two stylesheets (styleTokens.test.ts holds every stylesheet to zero literals).

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const src = (path: string) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");

const SOURCES = [
  "models/ConfigParts.tsx",
  "models/LocalForm.tsx",
  "models/ModelConfigDialog.tsx",
  "models/TableForm.tsx",
  "panels/LocalModelsPanel.tsx",
  "panels/ModelMarks.tsx",
  "panels/ModelSelect.tsx",
  "panels/ModelSetup.tsx",
  "panels/ModelsPanel.tsx",
];

const STYLES = ["styles/models.css", "models/modelConfig.css"];

test("the model files use the shared Select and shared states, not native selects or bare paragraphs", () => {
  const problems: string[] = [];
  for (const file of SOURCES) {
    const source = src(file);
    if (/<select\b/.test(source)) problems.push(`${file}: a native select`);
    if (/className="(error|empty)"/.test(source)) problems.push(`${file}: a bare error or empty paragraph`);
    if (/className="[^"]*\bbadge\b/.test(source)) problems.push(`${file}: a hand-built badge instead of Badge`);
  }
  assert.deepEqual(problems, []);
});

test("no model stylesheet rule gives a control a minimum width a side column cannot hold", () => {
  // A 255 px column leaves about 229 px of content; the widest minimum allowed is the 224 px of the
  // default-model picker, wrapped in min(100%, …) so it still shrinks. Anything in rem is a fixed width.
  const problems: string[] = [];
  for (const file of STYLES) {
    const css = src(file).replace(/\/\*[\s\S]*?\*\//g, "");
    for (const match of css.matchAll(/min-width:\s*([^;]+);/g)) {
      const value = match[1].trim();
      if (/rem\b/.test(value)) problems.push(`${file}: min-width ${value}`);
      if (/^calc\(\s*\d+\s*\*\s*var\(--px\)\s*\)$/.test(value)) {
        const pixels = Number(/\d+/.exec(value)?.[0]);
        if (pixels > 140) problems.push(`${file}: min-width ${value}`);
      }
    }
  }
  assert.deepEqual(problems, []);
});

test("the model tables are the shared .table", () => {
  assert.match(src("panels/ModelsPanel.tsx"), /className="table\b[^"]*models-table"/);
  assert.match(src("panels/LocalModelsPanel.tsx"), /className="table\b[^"]*local-model-table"/);
  assert.match(src("models/ConfigParts.tsx"), /className="table model-table"/);
});

test("the (i) button closes its tooltip on Esc without the dialog seeing the key", () => {
  const source = src("models/ConfigParts.tsx");
  assert.match(source, /<Tooltip\b/);
  // The listener is on the document in the capture phase, so it runs before the tooltip's own and the dialog's.
  assert.match(source, /addEventListener\("keydown",\s*onKey,\s*true\)/);
  assert.match(source, /event\.preventDefault\(\)/);
  // Esc is only taken while the button itself has focus; a hover-only tooltip leaves it to the dialog.
  assert.match(source, /document\.activeElement === button\.current/);
  // A tap opens the help inline, since the Tooltip has no touch trigger.
  assert.match(source, /info-tip-inline/);
  assert.match(source, /pointerType === "touch"/);
});

test("the local model table keeps its table roles when narrow CSS makes it display: block", () => {
  const source = src("panels/LocalModelsPanel.tsx");
  for (const role of ["table", "rowgroup", "row", "columnheader", "cell"]) assert.ok(source.includes(`role="${role}"`), role);
});

test("a narrow Local models row still says the per-agent context", () => {
  assert.match(src("panels/LocalModelsPanel.tsx"), /context\{row\.context > 0 && row\.context !== row\.context_length/);
});
