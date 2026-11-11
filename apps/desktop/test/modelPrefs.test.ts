// Favourite and hidden models (src/modelPrefs.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { arrangeModels, chooserModels, hiddenSpecs, markOf, toggleMark, withMark, type ModelMarks } from "../src/modelPrefs.ts";

const specs = ["openai:gpt", "llamacpp:qwen", "anthropic:claude", "llamacpp:gemma", "vllm:mistral"];
const self = (spec: string) => spec;
const marks: ModelMarks = { "llamacpp:gemma": "favourite", "openai:gpt": "hidden", "anthropic:claude": "favourite" };

test("marks are set, switched and cleared without touching the others", () => {
  assert.equal(markOf(undefined, "a"), null);
  assert.equal(markOf(marks, "llamacpp:gemma"), "favourite");
  const hidden = toggleMark(marks, "vllm:mistral", "hidden");
  assert.equal(markOf(hidden, "vllm:mistral"), "hidden");
  assert.deepEqual(toggleMark(hidden, "vllm:mistral", "hidden"), marks);
  // Favouriting a hidden model shows it again: a model has one mark at most.
  assert.equal(markOf(toggleMark(marks, "openai:gpt", "favourite"), "openai:gpt"), "favourite");
  assert.deepEqual(withMark(marks, "openai:gpt", null), { "llamacpp:gemma": "favourite", "anthropic:claude": "favourite" });
  // The input is never changed.
  assert.equal(marks["openai:gpt"], "hidden");
});

test("a management list puts favourites first and hidden models last, keeping their order", () => {
  assert.deepEqual(arrangeModels(specs, self, marks), {
    favourites: ["anthropic:claude", "llamacpp:gemma"],
    normal: ["llamacpp:qwen", "vllm:mistral"],
    hidden: ["openai:gpt"],
  });
  assert.deepEqual(arrangeModels(specs, self, undefined), { favourites: [], normal: specs, hidden: [] });
});

test("a session chooser lists favourites first and leaves hidden models out", () => {
  assert.deepEqual(chooserModels(specs, self, marks), [
    { item: "anthropic:claude", group: "favourite" },
    { item: "llamacpp:gemma", group: "favourite" },
    { item: "llamacpp:qwen", group: "normal" },
    { item: "vllm:mistral", group: "normal" },
  ]);
});

test("a hidden model stays in the chooser while it is the session's model, at the end", () => {
  const listed = chooserModels(specs, self, marks, "openai:gpt");
  assert.deepEqual(listed.at(-1), { item: "openai:gpt", group: "hidden" });
  assert.equal(listed.length, 5);
  // A visible current model changes nothing.
  assert.deepEqual(chooserModels(specs, self, marks, "llamacpp:qwen"), chooserModels(specs, self, marks));
});

test("without marks the chooser keeps the list as it was", () => {
  assert.deepEqual(
    chooserModels(specs, self, {}).map((entry) => entry.item),
    specs,
  );
});

test("the hidden specs the daemon keeps a copy of are sorted and leave favourites out", () => {
  assert.deepEqual(hiddenSpecs(undefined), []);
  assert.deepEqual(hiddenSpecs({ ...marks, "anthropic:old": "hidden" }), ["anthropic:old", "openai:gpt"]);
});
