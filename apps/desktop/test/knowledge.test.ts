// Knowledge agents in the desktop: the cards and questions a session's events build up, and how
// the badge, the card details and the question list word them (src/sessions/knowledge.ts). Run with
// `pnpm test` (node's test runner).

import assert from "node:assert/strict";
import { test } from "node:test";

import type { KnowledgeCard, SessionEvent } from "@imperium/protocol";

import {
  MAX_QUESTION,
  MAX_REMEMBERED_QUESTIONS,
  MAX_TOPIC,
  answerMarks,
  applyKnowledge,
  askerName,
  badgeTitle,
  canKeep,
  cardFacts,
  emptyKnowledge,
  outcomeLabel,
  outsideMaterial,
  questionProblem,
  questionsFor,
  rowNote,
  sourceLabel,
  statusLine,
  tokens,
  topicProblem,
  usd,
} from "../src/sessions/knowledge.ts";

function card(extra: Partial<KnowledgeCard> = {}): KnowledgeCard {
  return {
    agent_id: "t1",
    title: "Reader",
    model: "emperor",
    local: true,
    topic: "the billing crate",
    sources: [
      { kind: "file", target: "src/lib.rs", size: 10, modified_ms: 1, stale: false },
      { kind: "dir", target: "src", size: null, modified_ms: null, stale: false },
      { kind: "file", target: "src/invoice.rs", size: 20, modified_ms: 2, stale: true },
    ],
    more_sources: 2,
    untrusted: { web: false, tools: false },
    context_tokens: 12_300,
    context_window: 32_768,
    est_question_cost: { tokens: 3_400, cost_usd: 0.0031 },
    last_cached_ratio: 0.5,
    questions: 3,
    usage: { prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0 },
    cost_usd: 0.0123,
    status: "ready",
    queued: 0,
    stale_fraction: 0.5,
    ...extra,
  };
}

const asked = (id: string, to = "t1", from = "t2", text = "what is rounded?"): SessionEvent => ({
  type: "question_asked",
  question_id: id,
  from,
  to,
  text,
  at_ms: 1000,
});

const answered = (id: string, extra: Partial<Extract<SessionEvent, { type: "question_answered" }>> = {}): SessionEvent => ({
  type: "question_answered",
  question_id: id,
  outcome: "answered",
  answer: "totals, in invoice.rs",
  degraded: false,
  untrusted_web: false,
  untrusted_tools: false,
  usage: { prompt_tokens: 10, completion_tokens: 2, cached_tokens: 5 },
  cost_usd: 0.002,
  cached_ratio: 0.5,
  at_ms: 2000,
  ...extra,
});

test("a card appears with knowledge_changed and goes when the agent is retired", () => {
  let state = applyKnowledge(emptyKnowledge, { type: "knowledge_changed", agent_id: "t1", card: card(), retired: false });
  assert.equal(state.cards.t1.topic, "the billing crate");
  // A later card replaces it.
  state = applyKnowledge(state, { type: "knowledge_changed", agent_id: "t1", card: card({ questions: 4 }), retired: false });
  assert.equal(state.cards.t1.questions, 4);
  state = applyKnowledge(state, { type: "knowledge_changed", agent_id: "t1", card: null, retired: true });
  assert.deepEqual(state.cards, {});
  // Retiring one that is not kept changes nothing.
  assert.equal(applyKnowledge(state, { type: "knowledge_changed", agent_id: "t9", card: null, retired: true }), state);
});

test("a question is listed when asked and filled in when answered, newest first per agent", () => {
  let state = applyKnowledge(emptyKnowledge, asked("q1"));
  assert.equal(state.questions[0].outcome, null);
  assert.equal(outcomeLabel(state.questions[0].outcome), "Waiting");
  state = applyKnowledge(state, asked("q2", "t1", "user", "and the taxes?"));
  state = applyKnowledge(state, asked("q3", "t5"));
  state = applyKnowledge(state, answered("q1", { degraded: true, untrusted_web: true }));
  const list = questionsFor(state, "t1");
  assert.deepEqual(
    list.map((question) => question.id),
    ["q2", "q1"],
  );
  const first = list[1];
  assert.equal(first.outcome, "answered");
  assert.equal(first.answer, "totals, in invoice.rs");
  assert.equal(first.costUsd, 0.002);
  assert.deepEqual(answerMarks(first), ["degraded: its sources changed", "may hold web material"]);
  // An answer to a question this window never saw asked changes nothing.
  assert.equal(applyKnowledge(state, answered("q9")), state);
  // Events about other things change nothing either.
  assert.equal(applyKnowledge(state, { type: "resumed" }), state);
});

test("the list of questions is bounded", () => {
  let state = emptyKnowledge;
  for (let n = 0; n < MAX_REMEMBERED_QUESTIONS + 5; n++) state = applyKnowledge(state, asked(`q${n}`));
  assert.equal(state.questions.length, MAX_REMEMBERED_QUESTIONS);
  assert.equal(state.questions[0].id, "q5");
});

test("the badge says what the agent knows and where it stands", () => {
  assert.equal(badgeTitle(card()), "the billing crate\nReady for questions");
  assert.equal(statusLine(card({ status: "answering", queued: 2 })), "Answering, 2 waiting");
  assert.equal(statusLine(card({ status: "answering", queued: 0 })), "Answering");
  assert.match(statusLine(card({ status: "busy" })), /follow-up/);
  assert.deepEqual(rowNote(card()), {
    badge: "Knowledge",
    title: "the billing crate\nReady for questions",
    topic: "the billing crate",
  });
  assert.equal(rowNote(undefined), null);
});

test("the card's details list the model, sources, staleness, cost and cached share", () => {
  const facts = Object.fromEntries(cardFacts(card()).map((fact) => [fact.label, fact.value]));
  assert.equal(facts.Topic, "the billing crate");
  assert.equal(facts.Model, "emperor (on this machine)");
  assert.equal(facts.Read, "5 sources, 1 changed since");
  assert.equal(facts["Stale files"], "50 %; answers may be degraded");
  assert.equal(facts.Context, "12.3k of 32.8k tokens");
  assert.equal(facts["One question"], "about 3.4k tokens, $0.0031");
  assert.equal(facts["Cached last time"], "50 %");
  assert.equal(facts.Answered, "3, $0.01 spent");
  assert.equal("Outside material" in facts, false);
  // Rows with nothing to say are left out.
  const plain = Object.fromEntries(
    cardFacts(
      card({ stale_fraction: 0, last_cached_ratio: null, est_question_cost: null, cost_usd: null, context_window: null, local: false }),
    ).map((fact) => [fact.label, fact.value]),
  );
  assert.equal("Stale files" in plain, false);
  assert.equal("Cached last time" in plain, false);
  assert.equal("One question" in plain, false);
  assert.equal(plain.Model, "emperor");
  assert.equal(plain.Context, "12.3k tokens");
  assert.equal(plain.Answered, "3");
});

test("outside material in a card is named", () => {
  assert.equal(outsideMaterial(card()), null);
  assert.equal(outsideMaterial(card({ untrusted: { web: true, tools: false } })), "web pages");
  assert.equal(outsideMaterial(card({ untrusted: { web: true, tools: true } })), "web pages and tool results");
  const facts = cardFacts(card({ untrusted: { web: false, tools: true } }));
  assert.deepEqual(facts.at(-1), { label: "Outside material", value: "tool results" });
});

test("sources, askers, tokens and money read as words", () => {
  assert.equal(sourceLabel({ kind: "file", target: "a.rs", size: 1, modified_ms: 1, stale: true }), "a.rs (changed)");
  assert.equal(sourceLabel({ kind: "dir", target: "src", size: null, modified_ms: null, stale: false }), "src (folder)");
  assert.equal(askerName("user", {}), "You");
  assert.equal(askerName("root", {}), "The session's agent");
  assert.equal(askerName("t2", { t2: "Implement the parser" }), "Implement the parser");
  assert.equal(askerName("t7", {}), "t7");
  assert.equal(tokens(812), "812");
  assert.equal(tokens(3_100), "3.1k");
  assert.equal(tokens(1_200_000), "1.2M");
  assert.equal(usd(0.0031), "$0.0031");
  assert.equal(usd(1.5), "$1.50");
  for (const outcome of ["answered", "timeout", "cancelled", "refused", "failed"] as const) {
    assert.ok(outcomeLabel(outcome).length > 0);
  }
});

test("a topic and a question are checked before they are sent", () => {
  assert.equal(topicProblem("  "), "Say what the agent knows about.");
  assert.equal(topicProblem("x".repeat(MAX_TOPIC)), null);
  assert.match(topicProblem("x".repeat(MAX_TOPIC + 1)) ?? "", /at most 300/);
  assert.equal(questionProblem(""), "Type a question.");
  assert.equal(questionProblem("what is rounded?"), null);
  assert.match(questionProblem("x".repeat(MAX_QUESTION + 1)) ?? "", /at most 4000/);
  // Characters, not UTF-16 units: an emoji is one.
  assert.equal(questionProblem("😀".repeat(MAX_QUESTION)), null);
});

test("only a finished subagent that is not kept yet offers Keep as knowledge", () => {
  assert.equal(canKeep("finished", undefined), true);
  assert.equal(canKeep("finished", card()), false);
  for (const status of ["running", "paused", "failed"] as const) assert.equal(canKeep(status, undefined), false);
});
