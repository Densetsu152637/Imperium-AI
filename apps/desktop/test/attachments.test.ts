// Composer attachment chips (src/attachments.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import {
  addUploading,
  baseName,
  canSend,
  deliveryLabel,
  formatBytes,
  markFailed,
  markReady,
  pastedName,
  readyAttachments,
  removeChip,
} from "../src/attachments.ts";

const staged = {
  id: "a1",
  name: "shot.png",
  kind: "image" as const,
  mime: "image/png",
  size: 2048,
  path: "/data/attachments/s1/a1-shot.png",
  copied: true,
  vision: false,
};

test("a chip uploads, becomes ready, and sends with its thumbnail", () => {
  let chips = addUploading([], "k1", "shot.png");
  assert.equal(chips[0].state, "uploading");
  // Nothing is sent while a file is still uploading, even with text.
  assert.equal(canSend("look", chips), false);
  chips = markReady(chips, "k1", { attachment: staged, vision: true }, "data:image/jpeg;base64,AA==");
  assert.equal(chips[0].state, "ready");
  assert.equal(chips[0].vision, true);
  assert.equal(canSend("", chips), true, "attachments alone can be sent");
  assert.deepEqual(readyAttachments(chips), [{ ...staged, preview: "data:image/jpeg;base64,AA==" }]);
});

test("failed and removed chips are left out", () => {
  let chips = addUploading(addUploading([], "k1", "a.txt"), "k2", "b.bin");
  chips = markFailed(chips, "k2", "too large");
  chips = markReady(chips, "k1", { attachment: { ...staged, id: "a2", name: "a.txt", kind: "text" }, vision: false });
  assert.equal(canSend("", chips), true);
  assert.deepEqual(
    readyAttachments(chips).map((attachment) => attachment.id),
    ["a2"],
  );
  assert.equal(chips[1].error, "too large");
  chips = removeChip(chips, "k1");
  assert.equal(canSend("", chips), false, "a failed chip alone sends nothing");
  // A chip removed while uploading stays removed when its upload finishes.
  chips = markReady(removeChip(chips, "k2"), "k2", { attachment: staged, vision: true });
  assert.deepEqual(chips, []);
});

test("chips say how the model receives each file", () => {
  assert.match(deliveryLabel({ kind: "image" }, true), /image the model can see/);
  assert.match(deliveryLabel({ kind: "image" }, false), /cannot see images/);
  assert.match(deliveryLabel({ kind: "text" }, false), /included in the message/);
  assert.match(deliveryLabel({ kind: "file" }, false), /file path/);
});

test("names and sizes read well", () => {
  assert.equal(baseName("C:\\Users\\me\\report.pdf"), "report.pdf");
  assert.equal(baseName("/home/me/notes.md"), "notes.md");
  assert.equal(pastedName("image/png", 0), "pasted-1.png");
  assert.equal(pastedName("image/jpeg", 1), "pasted-2.jpg");
  assert.equal(pastedName("image/svg+xml", 2), "pasted-3.svg");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(5 * 1024 * 1024), "5.0 MB");
});
