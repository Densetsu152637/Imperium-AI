// A subagent's knowledge-agent section in its view (docs/21-knowledge-agents.md): for a finished
// subagent a button that keeps it as a knowledge agent under a topic; for a kept one its badge and
// topic, what its card says (model, sources with the changed ones marked, what a question costs,
// how much of the last one was cached, outside material in its context), the questions asked of it
// with their asker, outcome and cost, an Ask box that puts a question to it as the user, and
// Retire. The logic is in knowledge.ts.

import { BookOpen, Send } from "lucide-react";
import { useState } from "react";

import {
  KNOWLEDGE_BADGE,
  answerMarks,
  askerName,
  badgeTitle,
  canKeep,
  cardFacts,
  outcomeLabel,
  questionProblem,
  questionsFor,
  sourceLabel,
  topicProblem,
  usd,
} from "../knowledge";
import type { SessionState, TaskState } from "../sessions";

type Props = {
  session: SessionState;
  task: TaskState;
  /** Keeps the finished subagent under a topic (`session.agent_knowledge`). */
  onKeep?: (taskId: string, topic: string) => Promise<unknown>;
  /** Makes it an ordinary subagent again. */
  onRetire?: (taskId: string) => Promise<unknown>;
  /** Asks it a question as the user (`session.agent_ask`); the answer arrives as events. */
  onAsk?: (taskId: string, question: string) => Promise<unknown>;
};

export function KnowledgeSection({ session, task, onKeep, onRetire, onAsk }: Props) {
  const card = session.knowledge.cards[task.taskId];
  const archived = session.summary.archived;
  const [opening, setOpening] = useState(false);
  const [topic, setTopic] = useState("");
  const [question, setQuestion] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = (action: () => Promise<unknown>, done?: () => void) => {
    setBusy(true);
    setError(null);
    action()
      .then(() => done?.())
      .catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)))
      .finally(() => setBusy(false));
  };

  if (!card) {
    if (!canKeep(task.status, card) || !onKeep || archived) return null;
    const problem = topicProblem(topic);
    return (
      <section className="knowledge" aria-label="Knowledge agent">
        {opening ? (
          <form
            className="knowledge-keep"
            onSubmit={(event) => {
              event.preventDefault();
              if (problem === null) run(() => onKeep(task.taskId, topic.trim()), () => setOpening(false));
            }}
          >
            <label>
              What it knows about
              <input
                value={topic}
                onChange={(event) => setTopic(event.target.value)}
                placeholder="The billing crate and its invoice rounding"
                autoFocus
              />
            </label>
            <button type="submit" className="primary small" disabled={busy || problem !== null} title={problem ?? undefined}>
              Keep
            </button>
            <button type="button" className="ghost small" onClick={() => setOpening(false)}>
              Cancel
            </button>
          </form>
        ) : (
          <button
            type="button"
            className="ghost small"
            onClick={() => setOpening(true)}
            title="Other agents can then ask it questions instead of reading what it read again"
          >
            <BookOpen aria-hidden="true" /> Keep as knowledge
          </button>
        )}
        {error && <p className="error">{error}</p>}
      </section>
    );
  }

  const titles = Object.fromEntries(Object.values(session.tasks).map((other) => [other.taskId, other.title]));
  const questions = questionsFor(session.knowledge, task.taskId);
  const problem = questionProblem(question);
  const sources = card.sources;
  return (
    <section className="knowledge" aria-label="Knowledge agent">
      <div className="knowledge-head">
        <span className="badge knowledge" title={badgeTitle(card)}>
          {KNOWLEDGE_BADGE}
        </span>
        <span className="knowledge-topic">{card.topic}</span>
        {onRetire && !archived && (
          <button
            type="button"
            className="ghost small"
            disabled={busy}
            onClick={() => run(() => onRetire(task.taskId))}
            title="Stop keeping it as a knowledge agent; it stays a finished subagent"
          >
            Retire
          </button>
        )}
      </div>
      <dl className="knowledge-facts">
        {cardFacts(card).map((fact) => (
          <div key={fact.label}>
            <dt>{fact.label}</dt>
            <dd>{fact.value}</dd>
          </div>
        ))}
      </dl>
      {sources.length > 0 && (
        <details className="knowledge-sources">
          <summary>
            Sources ({sources.length + card.more_sources})
          </summary>
          <ul>
            {sources.map((source) => (
              <li key={`${source.kind}:${source.target}`} className={source.stale ? "stale" : undefined}>
                {sourceLabel(source)}
              </li>
            ))}
            {card.more_sources > 0 && <li className="muted">and {card.more_sources} more</li>}
          </ul>
        </details>
      )}
      <h4 className="knowledge-questions-title">Questions</h4>
      {questions.length === 0 ? (
        <p className="muted">Nobody has asked it anything yet.</p>
      ) : (
        <ul className="knowledge-questions" aria-label="Questions asked of this agent">
          {questions.map((asked) => (
            <li key={asked.id}>
              <div className="knowledge-question-head">
                <span>{askerName(asked.from, titles)}</span>
                <span className={`badge ${asked.outcome ?? "waiting"}`}>{outcomeLabel(asked.outcome)}</span>
                {asked.costUsd !== null && <span className="muted">{usd(asked.costUsd)}</span>}
              </div>
              <p className="knowledge-question-text">{asked.text}</p>
              {asked.answer && <p className="knowledge-answer">{asked.answer}</p>}
              {answerMarks(asked).length > 0 && <p className="muted">{answerMarks(asked).join(" · ")}</p>}
            </li>
          ))}
        </ul>
      )}
      {onAsk && !archived && (
        <form
          className="knowledge-ask"
          onSubmit={(event) => {
            event.preventDefault();
            if (problem === null) run(() => onAsk(task.taskId, question.trim()), () => setQuestion(""));
          }}
        >
          <input
            value={question}
            onChange={(event) => setQuestion(event.target.value)}
            placeholder={`Ask ${task.title} a question`}
            aria-label={`Ask ${task.title} a question`}
          />
          <button type="submit" className="primary small" disabled={busy || problem !== null} title={problem ?? undefined}>
            <Send aria-hidden="true" /> Ask
          </button>
        </form>
      )}
      {error && <p className="error">{error}</p>}
    </section>
  );
}
