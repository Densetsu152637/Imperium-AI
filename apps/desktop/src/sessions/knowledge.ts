// Knowledge agents in the desktop (docs/21-knowledge-agents.md): the cards and the question
// history a session's events build up, and how the Agents panel words them. Pure and
// dependency-free (imports are type-only), so the rules are tested without a window
// (test/knowledge.test.ts).

import type { KnowledgeCard, KnowledgeSource, QuestionOutcome, SessionEvent } from "@imperium/protocol";

/** Longest topic, in characters (the daemon refuses more). */
export const MAX_TOPIC = 300;
/** Longest question, in characters (the daemon refuses more). */
export const MAX_QUESTION = 4000;
/** How many questions a session remembers for its lists; older ones are dropped. */
export const MAX_REMEMBERED_QUESTIONS = 200;
/** What the daemon calls the user when they ask. */
export const USER_ASKER = "user";

/** One question asked of a knowledge agent, from its `question_asked` and `question_answered` events. */
export type KnowledgeQuestion = {
  id: string;
  from: string;
  to: string;
  text: string;
  askedAt: number;
  /** Null while it waits or is being answered. */
  outcome: QuestionOutcome | null;
  /** The answer (or why there is none), clipped by the daemon to 2,000 characters. */
  answer: string;
  degraded: boolean;
  untrustedWeb: boolean;
  untrustedTools: boolean;
  costUsd: number | null;
  cachedRatio: number | null;
};

/** What a session knows about its knowledge agents. */
export type KnowledgeState = {
  /** The cards of the agents kept now, by agent id. */
  cards: Record<string, KnowledgeCard>;
  /** Questions, oldest first. */
  questions: KnowledgeQuestion[];
};

export const emptyKnowledge: KnowledgeState = { cards: {}, questions: [] };

/** Applies one session event to the knowledge state; the same object when it changes nothing. */
export function applyKnowledge(state: KnowledgeState, event: SessionEvent): KnowledgeState {
  switch (event.type) {
    case "knowledge_changed": {
      if (event.retired || !event.card) {
        if (!(event.agent_id in state.cards)) return state;
        const rest = Object.fromEntries(Object.entries(state.cards).filter(([id]) => id !== event.agent_id));
        return { ...state, cards: rest };
      }
      return { ...state, cards: { ...state.cards, [event.agent_id]: event.card } };
    }
    case "question_asked": {
      const asked: KnowledgeQuestion = {
        id: event.question_id,
        from: event.from,
        to: event.to,
        text: event.text,
        askedAt: event.at_ms,
        outcome: null,
        answer: "",
        degraded: false,
        untrustedWeb: false,
        untrustedTools: false,
        costUsd: null,
        cachedRatio: null,
      };
      const questions = [...state.questions.filter((question) => question.id !== asked.id), asked];
      return { ...state, questions: questions.slice(-MAX_REMEMBERED_QUESTIONS) };
    }
    case "question_answered": {
      if (!state.questions.some((question) => question.id === event.question_id)) return state;
      return {
        ...state,
        questions: state.questions.map((question) =>
          question.id === event.question_id
            ? {
                ...question,
                outcome: event.outcome,
                answer: event.answer,
                degraded: event.degraded,
                untrustedWeb: event.untrusted_web,
                untrustedTools: event.untrusted_tools,
                costUsd: event.cost_usd,
                cachedRatio: event.cached_ratio,
              }
            : question,
        ),
      };
    }
    default:
      return state;
  }
}

/** The questions asked of `agentId`, newest first. */
export const questionsFor = (state: KnowledgeState, agentId: string): KnowledgeQuestion[] =>
  state.questions.filter((question) => question.to === agentId).reverse();

/** The badge's text on an agent's row. */
export const KNOWLEDGE_BADGE = "Knowledge";

/** The badge's hover text: what the agent knows about and where it stands. */
export const badgeTitle = (card: KnowledgeCard): string => `${card.topic}\n${statusLine(card)}`;

/** Where a knowledge agent stands, in words. */
export function statusLine(card: KnowledgeCard): string {
  switch (card.status) {
    case "ready":
      return "Ready for questions";
    case "answering":
      return card.queued > 0 ? `Answering, ${card.queued} waiting` : "Answering";
    case "busy":
      return "Busy with a follow-up; questions wait for it";
  }
}

/** A share as a whole percent: `50 %`. */
export const percent = (share: number): string => `${Math.round(share * 100)} %`;

/** Dollars: `$0.0030`, with more places below a cent. */
export const usd = (amount: number): string => `$${amount < 0.01 ? amount.toFixed(4) : amount.toFixed(2)}`;

/** Tokens as `812`, `3.1k` or `1.2M`. */
export function tokens(count: number): string {
  if (count >= 1e6) return `${(count / 1e6).toFixed(1)}M`;
  if (count >= 1e3) return `${(count / 1e3).toFixed(1)}k`;
  return String(count);
}

/** One row of a card's details. */
export type Fact = { label: string; value: string };

/** What a card says, row by row, for the agent's view; rows with nothing to say are left out. */
export function cardFacts(card: KnowledgeCard): Fact[] {
  const facts: Fact[] = [
    { label: "Topic", value: card.topic },
    { label: "Model", value: card.local ? `${card.model} (on this machine)` : card.model },
    { label: "Status", value: statusLine(card) },
  ];
  const sources = card.sources.length + card.more_sources;
  const stale = card.sources.filter((source) => source.stale).length;
  facts.push({ label: "Read", value: stale > 0 ? `${sources} sources, ${stale} changed since` : `${sources} sources` });
  if (card.stale_fraction > 0) {
    facts.push({ label: "Stale files", value: `${percent(card.stale_fraction)}; answers may be degraded` });
  }
  facts.push({
    label: "Context",
    value: card.context_window
      ? `${tokens(card.context_tokens)} of ${tokens(card.context_window)} tokens`
      : `${tokens(card.context_tokens)} tokens`,
  });
  if (card.est_question_cost) {
    const cost = card.est_question_cost;
    facts.push({
      label: "One question",
      value: `about ${tokens(cost.tokens)} tokens${cost.cost_usd === null ? "" : `, ${usd(cost.cost_usd)}`}`,
    });
  }
  if (card.last_cached_ratio !== null) {
    facts.push({ label: "Cached last time", value: percent(card.last_cached_ratio) });
  }
  facts.push({
    label: "Answered",
    value: card.cost_usd === null ? `${card.questions}` : `${card.questions}, ${usd(card.cost_usd)} spent`,
  });
  const outside = outsideMaterial(card);
  if (outside) facts.push({ label: "Outside material", value: outside });
  return facts;
}

/** What outside material the agent's context holds, or null. */
export function outsideMaterial(card: Pick<KnowledgeCard, "untrusted">): string | null {
  const parts = [card.untrusted.web ? "web pages" : null, card.untrusted.tools ? "tool results" : null].filter(
    (part): part is string => part !== null,
  );
  return parts.length === 0 ? null : parts.join(" and ");
}

/** A source as the view lists it: its target, marked when it is a folder or changed since it was read. */
export function sourceLabel(source: KnowledgeSource): string {
  const note = source.kind === "dir" ? " (folder)" : "";
  return `${source.target}${note}${source.stale ? " (changed)" : ""}`;
}

/** How a question ended (or that it has not), in words. */
export function outcomeLabel(outcome: QuestionOutcome | null): string {
  switch (outcome) {
    case null:
      return "Waiting";
    case "answered":
      return "Answered";
    case "timeout":
      return "Timed out";
    case "cancelled":
      return "Cancelled";
    case "refused":
      return "Refused";
    case "failed":
      return "Failed";
  }
}

/** The asker as the list names it: the user, a subagent by its title, or the id. */
export function askerName(from: string, titles: Record<string, string>): string {
  if (from === USER_ASKER) return "You";
  if (from === "root") return "The session's agent";
  return titles[from] ?? from;
}

/** The marks on an answer: degraded, and the outside material it may hold. */
export function answerMarks(question: Pick<KnowledgeQuestion, "degraded" | "untrustedWeb" | "untrustedTools">): string[] {
  return [
    question.degraded ? "degraded: its sources changed" : null,
    question.untrustedWeb ? "may hold web material" : null,
    question.untrustedTools ? "may hold tool results" : null,
  ].filter((mark): mark is string => mark !== null);
}

/** Why `topic` cannot be a topic (before the daemon checks it too), or null. */
export function topicProblem(topic: string): string | null {
  const length = [...topic.trim()].length;
  if (length === 0) return "Say what the agent knows about.";
  if (length > MAX_TOPIC) return `A topic is at most ${MAX_TOPIC} characters.`;
  return null;
}

/** Why `question` cannot be sent, or null. */
export function questionProblem(question: string): string | null {
  const length = [...question.trim()].length;
  if (length === 0) return "Type a question.";
  if (length > MAX_QUESTION) return `A question is at most ${MAX_QUESTION} characters.`;
  return null;
}

/**
 * Whether a subagent can be kept as a knowledge agent from its panel: it finished its work (a
 * failed, paused or running one cannot be) and is not one already. The daemon still refuses
 * writers, single-call and chunk agents, and conversations that do not fit, with its reason.
 */
export const canKeep = (status: "running" | "paused" | "finished" | "failed", card: KnowledgeCard | undefined): boolean =>
  status === "finished" && card === undefined;

/** Whether the panel can ask the agent: it is kept and not being resumed for more work. */
export const canAsk = (card: KnowledgeCard | undefined): card is KnowledgeCard => card !== undefined;

/** The row a card adds to a subagent's line in the Agents panel. */
export function rowNote(card: KnowledgeCard | undefined): { badge: string; title: string; topic: string } | null {
  return card ? { badge: KNOWLEDGE_BADGE, title: badgeTitle(card), topic: card.topic } : null;
}
