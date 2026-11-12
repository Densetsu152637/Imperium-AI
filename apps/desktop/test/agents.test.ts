// Environment agent names and session titles (src/agents.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { EnvironmentAgent, EnvironmentInfo } from "@imperium/protocol";

import {
  DEFAULT_PERMISSIONS,
  agentOfSession,
  agentTitle,
  describePermissions,
  draftOfPermissions,
  nameLength,
  nameProblem,
  nameTaken,
  notStartedNotice,
  parseNames,
  permissionsChanged,
  permissionsOfDraft,
} from "../src/agents.ts";

test("an agent that did not start is explained, one that did is not", () => {
  const base = { id: "a", name: "Planner", role: "", session_id: null, permissions: DEFAULT_PERMISSIONS };
  assert.equal(notStartedNotice(null), null);
  assert.equal(notStartedNotice({ ...base, started: true }), null);
  const text = notStartedNotice({ ...base, started: false, reason: "the environment has no folders yet" });
  assert.match(text ?? "", /Planner.*not running yet: the environment has no folders yet/);
});

test("names are measured in characters and checked like the daemon does", () => {
  // 60 emoji are 120 UTF-16 units but 60 characters: allowed, as the daemon counts them.
  assert.equal(nameLength("😀".repeat(60)), 60);
  assert.equal(nameProblem("😀".repeat(60)), null);
  assert.match(nameProblem("😀".repeat(61)) ?? "", /at most 60/);
  assert.match(nameProblem("   ") ?? "", /needs a name/);
  assert.match(nameProblem("Plan\nner") ?? "", /control characters/);
  assert.match(nameProblem("Plan\u001b[2Jner") ?? "", /control characters/);
  assert.equal(nameProblem(" Planner "), null);
});

test("uniqueness folds Unicode case like the daemon", () => {
  const agents = [{ id: "a", name: "Éditeur", role: "", session_id: null, permissions: DEFAULT_PERMISSIONS }];
  assert.equal(nameTaken(agents, " éDITEUR "), true);
  assert.equal(nameTaken(agents, "Editeur"), false);
});

const agent = (id: string, name: string, session_id: string | null = null): EnvironmentAgent => ({
  id,
  name,
  role: "",
  session_id,
  permissions: DEFAULT_PERMISSIONS,
});

test("an agent name is taken ignoring case and padding, except by the agent being edited", () => {
  const agents = [agent("a", "Planner"), agent("b", "Reviewer")];
  assert.equal(nameTaken(agents, " planner "), true);
  assert.equal(nameTaken(agents, "Critic"), false);
  assert.equal(nameTaken(agents, "Planner", "a"), false);
  assert.equal(nameTaken(agents, "Planner", "b"), true);
});

test("a persistent agent's session is found and titled by its agent", () => {
  const environment = { agents: [agent("a", "Planner", "s1"), agent("b", "Reviewer", "s2")] } as EnvironmentInfo;
  assert.equal(agentOfSession(environment, "s2")?.name, "Reviewer");
  assert.equal(agentOfSession(environment, "none"), null);
  assert.equal(agentOfSession(null, "s1"), null);
  assert.equal(agentTitle("Work", agent("a", "Planner")), "Work · Planner");
  assert.equal(agentTitle("Work", null), "Work agent");
});

test("typed server and plugin names are split, trimmed and deduplicated ignoring case", () => {
  assert.deepEqual(parseNames(" github, Slack\n  GitHub ,, docs "), ["github", "Slack", "docs"]);
  assert.deepEqual(parseNames("  "), []);
});

test("an agent with no profile is unrestricted and the editor round-trips a profile", () => {
  assert.deepEqual(permissionsOfDraft(draftOfPermissions(undefined)), DEFAULT_PERMISSIONS);
  assert.equal(describePermissions(undefined), null);
  assert.equal(describePermissions(DEFAULT_PERMISSIONS), null);

  const profile = {
    mcp: "named" as const,
    mcp_servers: ["github", "slack"],
    plugins: "none" as const,
    plugin_names: [],
    local_files_only: false,
  };
  assert.deepEqual(permissionsOfDraft(draftOfPermissions(profile)), profile);
  assert.equal(describePermissions(profile), "MCP: github, slack · Plugins: none");
});

test("names are sent only for a named list, and a named list with no name means none", () => {
  const draft = { ...draftOfPermissions(undefined), mcp: "named" as const, mcpNames: " , ", pluginNames: "docs" };
  const sent = permissionsOfDraft(draft);
  assert.equal(sent.mcp, "none");
  assert.deepEqual(sent.mcp_servers, []);
  // Names typed before switching back to "all" are not kept.
  assert.equal(sent.plugins, "all");
  assert.deepEqual(sent.plugin_names, []);
});

test("local files only is shown as the whole limit, whatever the lists say", () => {
  const locked = { ...DEFAULT_PERMISSIONS, local_files_only: true, mcp: "none" as const };
  assert.equal(describePermissions(locked), "Local files only");
  assert.equal(permissionsOfDraft(draftOfPermissions(locked)).local_files_only, true);
});

test("permissions are sent only when the form changed them", () => {
  const opened = draftOfPermissions(undefined);
  assert.equal(permissionsChanged(opened, { ...opened }), false);
  // Typing names for a list that is not "named" changes nothing that would be sent.
  assert.equal(permissionsChanged(opened, { ...opened, mcpNames: "slack" }), false);
  assert.equal(permissionsChanged(opened, { ...opened, localOnly: true }), true);
  assert.equal(permissionsChanged(opened, { ...opened, plugins: "none" }), true);
});
