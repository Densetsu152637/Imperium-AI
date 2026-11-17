import assert from "node:assert/strict";
import { test } from "node:test";

import { noReleaseMessage, switchMessage } from "../src/shared/updateSwitch.ts";

test("a switch to a lower version number says the build is older and names the channel", () => {
  const text = switchMessage(
    { channel: "experimental", version: "0.2.163-experimental" },
    { channel: "main", version: "0.2.0", older: true },
  );
  assert.match(text, /install the main build 0\.2\.0/);
  assert.match(text, /older version number than 0\.2\.163-experimental/);
});

test("a switch to a higher version number does not call it older", () => {
  const text = switchMessage(
    { channel: "main", version: "0.2.0" },
    { channel: "experimental", version: "0.2.164-experimental", older: false },
  );
  assert.match(text, /install the experimental build 0\.2\.164-experimental/);
  assert.doesNotMatch(text, /older/);
});

test("a channel without a release says so", () => {
  assert.equal(noReleaseMessage("main"), "The main channel has no release to install yet.");
});
