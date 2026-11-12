// Environment agent names and session titles (src/agents.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { EnvironmentAgent, EnvironmentInfo } from "@imperium/protocol";

import { agentOfSession, agentTitle, nameTaken } from "../src/agents.ts";

const agent = (id: string, name: string, session_id: string | null = null): EnvironmentAgent => ({
  id,
  name,
  role: "",
  session_id,
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
