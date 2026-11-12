// Environment agent names and session titles (src/agents.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { EnvironmentAgent, EnvironmentInfo } from "@imperium/protocol";

import { agentOfSession, agentTitle, nameLength, nameProblem, nameTaken, notStartedNotice } from "../src/agents.ts";

test("an agent that did not start is explained, one that did is not", () => {
  const base = { id: "a", name: "Planner", role: "", session_id: null };
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
  // Separators and bidirectional controls make a name read differently from what it is.
  for (const bad of ["Plan ner", "Plan ner", "Plan‮ner", "‪Planner", "Plan⁦ner⁩"]) {
    assert.match(nameProblem(bad) ?? "", /control characters/, JSON.stringify(bad));
  }
  assert.equal(nameProblem("Planner ‍👩"), null, "joiners in emoji are fine");
});

test("a saved update that did not restart the agent says so in its own words", () => {
  const agent = { id: "a", name: "Planner", role: "", session_id: null, started: false, reason: "no model" };
  assert.match(notStartedNotice(agent, "was saved") ?? "", /Planner” was saved but is not running yet: no model/);
});

test("uniqueness folds Unicode case like the daemon", () => {
  const agents = [{ id: "a", name: "Éditeur", role: "", session_id: null }];
  assert.equal(nameTaken(agents, " éDITEUR "), true);
  assert.equal(nameTaken(agents, "Editeur"), false);
});

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
