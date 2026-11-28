// The user-only daemon calls (daemon/userCall.ts): inside the app they go through the native
// `user_call` command with the method and params only; outside it they are ordinary calls.

import assert from "node:assert/strict";
import { test } from "node:test";

import { callAsUser } from "../src/daemon/userCall.ts";

test("inside the app a settings call goes through the native command, not the daemon connection", async () => {
  const sent: Array<[string, Record<string, unknown>]> = [];
  const direct: string[] = [];
  const result = await callAsUser<{ ok: boolean }>(
    {
      call: async (method) => {
        direct.push(method);
        return { ok: false } as never;
      },
    },
    "config.set",
    { key: "runtime.llamacpp.custom_path", value: "/opt/llama-server" },
    {
      inside: true,
      invoke: async (command, args) => {
        sent.push([command, args]);
        return { ok: true };
      },
    },
  );
  assert.deepEqual(sent, [["user_call", { method: "config.set", params: { key: "runtime.llamacpp.custom_path", value: "/opt/llama-server" } }]]);
  assert.deepEqual(direct, []);
  assert.equal(result.ok, true);
});

test("outside the app it is an ordinary daemon call", async () => {
  const direct: Array<[string, unknown]> = [];
  const result = await callAsUser<{ ok: boolean }>(
    {
      call: async (method, params) => {
        direct.push([method, params]);
        return { ok: true } as never;
      },
    },
    "runtime.set_server",
    { path: "/opt/llama-server" },
    {
      inside: false,
      invoke: async () => {
        throw new Error("no native command outside the app");
      },
    },
  );
  assert.deepEqual(direct, [["runtime.set_server", { path: "/opt/llama-server" }]]);
  assert.equal(result.ok, true);
});

test("a refusal from the daemon reaches the caller as an error", async () => {
  await assert.rejects(
    callAsUser(
      { call: async () => ({}) as never },
      "config.set",
      { key: "mcp.trusted_projects", value: null },
      {
        inside: true,
        invoke: async () => {
          throw "only a process of your own on the local socket may make the change";
        },
      },
    ),
  );
});
