// Environment agent names and session titles (src/agents.ts).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { EnvironmentAgent, EnvironmentInfo, InitiativeState } from "@imperium/protocol";

import {
  actLimits,
  agentOfSession,
  agentTitle,
  initiativeHeadline,
  nameLength,
  nameProblem,
  nameTaken,
  notStartedNotice,
  openSuggestions,
  startedByNote,
} from "../src/agents.ts";

const state = (change: Record<string, unknown>) =>
  ({
    environment_id: "e",
    agent_id: "a",
    mode: "suggest",
    reviewing: false,
    waiting: null,
    last_review_at: null,
    next_review_at: null,
    last_error: null,
    suggestions: [],
    digest: null,
    started_today: 0,
    max_per_day: 2,
    running: 0,
    max_concurrent: 1,
    ...change,
  }) as unknown as InitiativeState;

test("the initiative headline says what the agent is doing or why it waits", () => {
  const now = Date.UTC(2026, 0, 1, 12);
  assert.equal(initiativeHeadline(state({ mode: "off" }), now), "Initiative is off.");
  assert.equal(initiativeHeadline(state({ reviewing: true }), now), "Reviewing now…");
  assert.equal(initiativeHeadline(state({}), now), "No review yet.");
  assert.equal(
    initiativeHeadline(state({ last_review_at: now - 2 * 3_600_000, waiting: "the machine is busy" }), now),
    "Last review 2 h ago. Waiting: the machine is busy.",
  );
  assert.match(initiativeHeadline(state({ last_error: "no model" }), now), /The last review failed: no model/);
  assert.equal(actLimits(state({ started_today: 1 })), "Started today 1 of 2 · running 0 of 1");
});

test("only open suggestions are offered to start", () => {
  const suggestion = (id: string, status: string) => ({ id, status, title: id });
  const got = openSuggestions(state({ suggestions: [suggestion("a", "open"), suggestion("b", "started"), suggestion("c", "dismissed")] }));
  assert.deepEqual(got.map((s) => s.id), ["a"]);
  assert.deepEqual(openSuggestions(null), []);
});

test("a session an agent started carries a note naming the agent, why, and from what", () => {
  const by = { environment_id: "e", environment_name: "Work", agent_id: "a", agent_name: "Planner", kind: "initiative", reason: "the roadmap lists it", source: "docs/12.md" };
  assert.equal(
    startedByNote(by),
    "Started by Planner in Work on its own initiative: the roadmap lists it (from docs/12.md)",
  );
  assert.match(startedByNote({ ...by, kind: "suggestion", source: undefined }), /from its suggestion: the roadmap lists it$/);
});

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
