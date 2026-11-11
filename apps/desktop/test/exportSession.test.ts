// Session export in the app (src/exportSession.ts): the call and the media types.

import assert from "node:assert/strict";
import { test } from "node:test";

import { exportMediaType, requestExport } from "../src/exportSession.ts";

test("requestExport calls session.export with the format and only the options given", async () => {
  const calls: [string, unknown][] = [];
  const client = {
    call<Result>(method: string, params?: unknown): Promise<Result> {
      calls.push([method, params]);
      return Promise.resolve({ format: "markdown", filename: "s.md", content: "# s" } as Result);
    },
  };
  const result = await requestExport(client, "s1", "markdown");
  assert.equal(result.filename, "s.md");
  await requestExport(client, "s1", "json", { includeSubagents: false });
  assert.deepEqual(calls, [
    ["session.export", { session_id: "s1", format: "markdown" }],
    ["session.export", { session_id: "s1", format: "json", include_subagents: false }],
  ]);
});

test("exports are labelled with their media type", () => {
  assert.equal(exportMediaType("json"), "application/json");
  assert.equal(exportMediaType("markdown"), "text/markdown");
});
