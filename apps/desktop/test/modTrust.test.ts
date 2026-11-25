// Which mods may leave the sandbox (src/settings/modTrust.ts). Run with `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import { mayRunInMainContext } from "../src/settings/modTrust.ts";

const trusted = { main_context: true, trusted: true, modified: false };

test("a mod runs in the main window only when it asked, was trusted, is intact and is on", () => {
  assert.equal(mayRunInMainContext(trusted, true), true);
  assert.equal(mayRunInMainContext(trusted, false), false);
  assert.equal(mayRunInMainContext({ ...trusted, trusted: false }, true), false);
  assert.equal(mayRunInMainContext({ ...trusted, main_context: false }, true), false);
  assert.equal(mayRunInMainContext({ ...trusted, modified: true }, true), false);
  assert.equal(mayRunInMainContext({}, true), false);
});
