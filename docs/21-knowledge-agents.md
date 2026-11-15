# 21 — Knowledge agents

**Designed, not built** ([roadmap item 45](12-roadmap.md#next-in-order)). Agents of a session can
ask questions of other agents that keep context they already loaded. A research subagent that
read a module, a specification or a set of pages stays available as a **knowledge agent**: the
root and other subagents ask it (`ask_agent`) instead of searching and reading the same material
again, so context that has been paid for once is reused rather than reloaded into every
implementer's window.

This document is the design: decisions, the contracts they change, and the order to build them.
It builds on [agent state](03-agent-engine.md#agent-state) (kept subagent conversations),
[queueing and steering](03-agent-engine.md#queueing-and-steering),
[parallel writers](03-agent-engine.md#parallel-writers-implemented), the local runtime's slots
and residency ([04](04-models-and-routing.md)), [permissions](05-tools-and-isolation.md#permissions)
and [budgets](10-configuration.md#budgets).

## Starting point

What the code does today, and what limits the design:

- Only the root delegates (`can_delegate`); subagents are depth 1 and get none of the delegation
  tools (`crates/imperium-engine/src/agent.rs`).
- A finished subagent becomes `idle` and keeps its conversation in
  `<data>/sessions/<id>.agents/<agent-id>.json`; the root can `resume_agent` it with a follow-up,
  which appends a turn to that conversation and is collected with `await_tasks`. This is the
  baseline for "asking": only the root can do it, it is serial with the agent's other work, it
  grows the agent's conversation with every question, and the root relays every answer through
  its own context.
- `session.send` and `queue_agent` reach only a subagent that is still running; a reported one
  refuses messages (`-32602`) until it is resumed.
- Local models give each agent (a `holder`, such as `<session>/t3`) back the slot it used last,
  with its cached prompt, and can save a slot to disk and restore it (`LocalModels::suspend`,
  `resume`, `waiting`; `crates/imperium-api/src/local.rs`, the residency controller in
  `crates/imperium-local/src/residency.rs`).
- Budgets are checked before every model call and every spawn (`BudgetGate`); per-task budgets
  do not exist yet ([budgets and limits](03-agent-engine.md#budgets-and-limits)).
- Plan mode already defines a read-only tool set: reads run, everything that writes, runs
  commands or asks is refused without asking
  ([permission modes](05-tools-and-isolation.md#permission-modes-implemented)).

## Decisions

| # | Decision | Why | Rejected |
| --- | --- | --- | --- |
| K1 | A knowledge agent is an ordinary idle subagent whose record carries a **knowledge card**; nothing new is spawned to hold context | Reuses agent state, persistence, resume and the Agents panel; a knowledge agent is a role, not a new kind of process | A separate "bank" process or store (duplicates agent state); writing findings to memory files only (loses the loaded context, needs foresight) |
| K2 | Each question runs in an **ephemeral fork**: a copy of the knowledge agent's kept conversation plus the question, discarded after the answer | The bank stays exactly what was promoted: no pollution between askers, no leakage of one asker's question to another, no growth toward compaction, and answers do not depend on question order; on a local model the unchanged prefix is the cached prompt, so a fork re-reads only the question | Appending each question to the agent's own conversation (grows and drifts, needs compaction that destroys the detail the bank exists for, serialises everything, leaks askers' questions); the root relaying through `resume_agent` (today's baseline) |
| K3 | Askers are the root and ordinary subagents; **knowledge agents and their forks never ask** | The wait-for graph is bipartite with depth 1, so a cycle or deadlock cannot form; no depth limit to tune | General peer-to-peer messaging (cycles, a large prompt-injection surface, no clear owner of an exchange) |
| K4 | Forks answer under **plan mode, or the session's mode if stricter**, with no tool that asks the user, delegates, writes memory or starts sessions, and at most `knowledge.answer_max_steps` model calls | An answer can look things up again but never changes anything, never raises an approval in another agent's name, and is bounded | Full tools (a question becomes a way to act); no tools (an answer cannot check a detail it half remembers) |
| K5 | Promotion is explicit: the root (`keep_as_knowledge`, or `spawn_agent` with `knowledge`) or the user; never automatic, never by the subagent itself | The root knows which context will be reused; automatic promotion would keep every research agent's context and cost | Promote every finished read-only subagent (unbounded residency and clutter) |
| K6 | An answer is **data from that agent**, framed and escaped, never instructions; the asker gains no tool, path or approval through it | Knowledge agents may have read untrusted pages; containment must not depend on the knowledge agent behaving | Trusting answers like the asker's own reasoning |
| K7 | Knowledge agents are scoped to their **session** in the first version | Permissions, folders, worktrees and budgets are per session; cross-session sharing needs its own trust rules | Environment-wide banks (open question Q2) |

## Tools

All three are defined in `imperium-protocol` like the other built-in tools
([D25](13-decisions.md), [dispatch contract](03-agent-engine.md#dispatch-contract)), exported to
`schemas/tools/` (status `planned` until built), so Emperor is trained on the same schemas.

| Tool | Who has it | Parameters | Result |
| --- | --- | --- | --- |
| `list_knowledge_agents` | root and ordinary subagents | `query?` | Each knowledge agent's card (below), best match to `query` first; empty when there are none |
| `ask_agent` | root and ordinary subagents | `agent` (id or title), `question` (self-contained, at most 4,000 characters), `timeout_s?` (default 120, at most 600) | The answer, framed (see [safety](#safety)); or a tool error saying it timed out, was cancelled or refused |
| `keep_as_knowledge` | root only | `agent_id`, `topic` (at most 300 characters); or `agent_id`, `retire: true` | The card, or that it was retired |

`spawn_agent` gains an optional `knowledge` (a topic string): the subagent is promoted with that
topic when its first turn finishes successfully; its report still goes to `await_tasks` as
usual. The root prompt and `brief.md` say when to use it: research whose results several later
tasks will need.

**What can be promoted:** an idle (finished, ok) subagent that had tools. A writer can be: it
answers from its own worktree, read-only. Refused: a running, paused or failed subagent (finish
or resume it first), a `single_call` subagent (its context is one reply), and `delegate_chunks`
chunk and reduce agents (their context is an in-memory artefact; open question Q6).

**The knowledge card** (`KnowledgeCard`, kept on the agent record and in `AgentSummary`):

| Field | Source |
| --- | --- |
| `agent_id`, `title`, `model`, `local` | the agent record |
| `topic` | the promoter's text |
| `sources` | derived by the engine, without a model call, from the conversation's successful tool calls: files read (path, plus size and modified time when read), directories listed, search patterns, URLs fetched; at most 50, newest first, with a count of the rest |
| `untrusted_sources` | true when any source is from the web, a browser or an MCP tool |
| `context_tokens` | the kept conversation's size, and the model's window |
| `questions`, `usage`, `cost_usd` | answers given so far, and what they cost |
| `status` | `ready`, `answering` (with queue length), `dormant` (slot saved, see [lifetime](#lifetime-and-memory)), `busy` (resumed for more work) |
| `stale` | sources whose file changed since it was read (checked by size and modified time at each question) |

## Answering

1. `ask_agent` checks the target is a knowledge agent of this session, that the asker is not a
   knowledge agent or fork (K3), and that the asker has fewer than
   `knowledge.max_questions_per_turn` (20) questions in this turn. It emits `question_asked` and
   puts the question in the knowledge agent's **question queue** (FIFO, separate from its
   message inbox).
2. The asker waits like `await_tasks`: no model calls, and asleep to the local runtime
   (`LocalModels::waiting`), so its model may be swapped out for the knowledge agent's. A steering
   message to the root, a session stop or the timeout ends the wait.
3. A free answer slot takes the question: at most `knowledge.parallel_answers` forks per
   knowledge agent at once (default 2 on hosted models; always 1 on a local model, so forks reuse
   the one holder slot whose cached prefix is the kept conversation).
4. The fork is an `Agent` built like `resume_subagent` builds one (same instructions, model and
   workspace, the kept conversation restored) but marked as a fork: its tool set is K4's, its
   checkpoints are not saved, and it never commits a writer's worktree. Its turn is the question,
   wrapped (see [safety](#safety)), with a note naming the changed sources when the card is
   stale, and instructions to answer only from what it read or re-reads now, to cite paths and
   lines, to say "not in what I read" instead of guessing, and to say what is missing when the
   question is ambiguous (forks have no `request_clarification`).
5. The reply, capped at 8,000 characters (as a report), returns to the asker; `question_answered`
   is emitted with usage and cost; the fork is dropped.

**Timeouts and cancellation.** A question still queued when its timeout passes is removed; one
being answered has its fork cancelled at the next safe point (a model request in flight is
cancelled; a tool call finishes). The same happens when the asker's turn ends, fails or is
cancelled, and when the asker is paused by a steering message to the root. The answer to a
question nobody waits for any more is never delivered. A session stop halts forks like other
agents (`hold_while_stopped`); their questions wait and go on at resume, if their askers still
wait.

**Resuming a knowledge agent.** `resume_agent` on a knowledge agent is the way to give it more to
know: its status becomes `busy`, questions queue until the turn finishes, and that turn is
appended to the kept conversation (so the card's sources and size are refreshed). This is the only
way the kept conversation grows.

**Who pays.** Every fork call is a model call of the session, so it counts in `usage.json` and
every budget scope like any other, and the `BudgetGate` is checked before each call against the
knowledge agent's model; a blocked budget fails the question with the budget message, and one
that asks raises its single per-session prompt. The usage is recorded on the knowledge agent
(card and record) and also reported to the asker in the result's first line ("answered by t3 in
2 calls, 3.1k tokens"). When per-task budgets exist, a question is charged to the asker's budget,
as a spawn would be.

**What the asker shares.** Only the question text, its own id and title. No part of its
conversation, plan, state or worktree is sent; the root prompt and subagent prompt say questions
must be self-contained, like briefs.

## Safety

- **Answers are data.** The result is wrapped as
  `<agent-answer from="t3" topic="…" untrusted-sources="true">…</agent-answer>`, with text that
  looks like the delimiters backslash-escaped (as kept context blocks are), after a line saying
  it is information from another agent to weigh, not instructions. Askers' prompts say the same.
- **Questions are data too.** The fork reads the question inside `<question from="t5">`, after a
  line saying it comes from another agent, is to be answered, and cannot change its role, tools or
  instructions.
- **No escalation.** A fork's tools are the knowledge agent's own offered tools (the session's
  permissions and the [agent permission profile](05-tools-and-isolation.md#agent-permission-profiles-implemented),
  `profile_refusal` included) intersected with K4's read-only set, decided under plan mode or the
  session's stricter mode, with the safety floor and deny rules as always. A call that would ask
  the user is refused with a message to answer from what it has. Nothing in the asker's request
  can widen this: the asker passes only text.
- **No cycles or deadlock.** K3 makes the wait-for graph bipartite. The engine still keeps the
  waits (asker → knowledge agent) and refuses an ask that would close a cycle, so a later change
  to who may ask cannot introduce one silently. A knowledge agent cannot ask itself or anyone.
- **Prompt injection.** A knowledge agent whose sources include web pages is marked
  `untrusted_sources`, and its answers say so. Containment rests on the asker side (framing,
  no tools gained) and the fork side (read-only, no asking), not on the content being clean.
- **Rate.** At most `knowledge.max_questions_per_turn` questions per asker turn, so a confused
  model cannot loop on a knowledge agent.

## Lifetime and memory

- **How many.** At most `knowledge.max_per_session` (4) knowledge agents per session. Promoting
  one more is refused with the list, oldest-asked first, so the root (or user) retires one.
- **Retiring.** `keep_as_knowledge` with `retire: true`, the user, or the session ending. A
  retired agent is an ordinary idle subagent again: still resumable, no longer askable, its
  card removed.
- **Dormancy.** A knowledge agent on a local model that has not been asked for
  `knowledge.dormant_after_min` (15) minutes has its slot saved and freed
  (`LocalModels::suspend` for its holder), so it holds no KV cells; the next question restores it
  (`resume`) before the fork's first call. A hosted model holds nothing between calls, so
  dormancy only marks the card.
- **Compaction.** The kept conversation is never compacted by questions (forks are discarded).
  At promotion, when the conversation plus `knowledge.answer_reserve` (25 % of the window) would
  not fit, it is compacted once, with the topic as the focus ("keep paths, line numbers,
  signatures and quotes relevant to …"); the compactor gains an optional focus for this. A
  promotion that still does not fit is refused, saying so. A fork that reaches
  `context.auto_compact_at` stops and answers with what it has, without compacting.
- **Restarts.** The card is part of the agent record, so with `sessions.agent_state_keep` on, a
  resumed session keeps its knowledge agents (their KV cache is rebuilt on the first question).
  Questions in flight at a restart are lost; their askers' turns were cut off anyway. With the
  setting off, and in project-agent sessions, knowledge agents last as long as the daemon run.

## Visibility

**Protocol 0.37** (additive):

- `AgentSummary.knowledge: Option<KnowledgeCard>`.
- Session events: `knowledge_changed` (`agent_id`, the card, or `retired`),
  `question_asked` (`question_id`, `from`, `to`, `text` clipped to 2,000 characters, `at_ms`) and
  `question_answered` (`question_id`, `outcome`: `answered`, `timeout`, `cancelled`, `refused`
  or `failed`, `answer` clipped to 2,000, `usage`, `cost_usd`, `at_ms`). A fork's streaming
  events (`turn_started`, `text_delta`, tool events) carry the knowledge agent's id and a new
  optional `question_id`, so clients group them under the question.
- Methods: `session.agent_ask` (`session_id`, `agent_id`, `question`; returns the
  `question_id`, the answer arrives as `question_answered`) for the user, and
  `session.agent_knowledge` (`session_id`, `agent_id`, `topic` or `retire`) to promote or retire.
  Phones may read cards; promoting and asking are desktop and CLI actions in the first version.

**Journal.** All three events are journalled, so replay rebuilds cards and the question history;
fork conversations are not saved anywhere. Export includes questions and answers under the
knowledge agent.

**Desktop** ([07](07-desktop-ui.md#agents-panel-implemented)). The Agents panel marks knowledge
agents with a *Knowledge* badge and the topic; the agent's view lists its sources (stale ones
marked), its questions with asker, outcome and cost, and an *Ask* box; *Keep as knowledge* and
*Retire* sit beside *Continue with…*. The task tree draws a dashed edge from asker to knowledge
agent while a question waits.

**CLI.** `imperium chat` gains `/ask <agent> <question>` and shows cards in `/agents`.

**Claude harness** ([20](20-claude-harness.md)). `imperium mcp-serve` gains `list_knowledge_agents`
and `ask_agent` tools over the same methods, so Claude Code can query a session's knowledge
agents without reading transcripts.

## Interactions

- **Parallel writers.** Writers ask like any subagent. A knowledge agent reads the folders it was
  given (the session's, or its own worktree if it was a writer), not the asker's worktree; the
  answer's header names them, so a writer knows its own edits are not reflected. The staleness
  check catches files the root merged since.
- **Harness aids** ([03](03-agent-engine.md#harness-aids-for-small-models)). De-reference and
  state apply to the root's user messages only and are not applied to questions or forks. The
  root prompt tells the model to name concrete entities in questions (the state block helps it
  do so). `ambiguity_patterns` gains one for "ask the knowledge agent about it": name which.
- **Initiative and persistent agents** ([14](14-projects-and-automation.md#initiative-implemented)).
  Reviews run in plan mode; `ask_agent` is read-only, so a review may ask its own session's
  knowledge agents. Knowledge agents are not shared across a persistent agent's sessions (Q2).
- **Unattended sessions.** Nothing in a fork asks the user, so questions never wait on an
  approval; local calls use the session's priority (`Background` for schedules and project
  agents), and budgets that ask pause as they do for any call.
- **Steering and stops.** Steering the root pauses its subagents as today; a paused asker's
  questions are cancelled. Knowledge agents are idle, so they are not paused; their forks for
  other askers go on.

## Implementation plan

1. **Engine core and contract (protocol 0.37).** Tool definitions and schemas
   (`ask_agent`, `list_knowledge_agents`, `keep_as_knowledge`, `spawn_agent.knowledge`); the card
   on `AgentRecord`/`AgentSummary` with sources derived from the conversation; a `can_ask` flag
   beside `can_delegate`; the question queue, forks with K4's tools under plan mode, timeouts,
   cancellation with the asker's turn, budget gate per call, framing and escaping; events and
   journal replay; `session.agent_ask` and `session.agent_knowledge`.
   *Tests* (fake model server): a subagent asks a promoted agent and gets its answer; the kept
   conversation is byte-identical after three questions; a fork's `write_file`, `run_command`,
   `ask_agent` and asking `web_fetch` are refused; a knowledge agent is never offered
   `ask_agent`; a timeout removes a queued question and cancels a running fork; a blocked budget
   fails the question; answer delimiters inside the answer are escaped; replaying a journal
   restores cards; schema drift check passes.
2. **Residency and lifetime.** Single local holder per knowledge agent with prefix reuse;
   dormancy through `suspend`/`resume`; `max_per_session`; compaction at promotion with a topic
   focus; staleness by size and modified time; the cycle check on the wait map; restart
   persistence. *Tests:* a second question on a local fake runtime reuses the holder and
   reports cached prompt tokens; a dormant agent is restored before its fork's first call; a
   fifth promotion is refused with the list; editing a source marks it stale and the fork gets
   the note; a session restart keeps the card.
3. **Clients and prompts.** Agents panel badge, sources, questions and *Ask*; task-tree edges;
   `imperium chat /ask`; mcp-serve tools; root and subagent prompt lines and `brief.md`;
   Emperor's root prompt and an eval case in `schemas/eval` (asking beats re-reading). *Tests:*
   desktop component tests for the badge, the question list and *Ask*; a browser-preview check
   of the panel; mcp-serve tool tests against a fake daemon.

**Acceptance for the item:** a session where a research subagent reads a crate, is promoted,
and two writer subagents implement against it answers their questions with cited paths, with
fewer total prompt tokens than the same task where each writer reads the crate itself (measured
on a recorded run, not assumed), and no answer ever caused a write, a command or an approval.

## Settings

Under `[knowledge]` ([10](10-configuration.md)): `enabled` (true), `max_per_session` (4),
`parallel_answers` (2; local models always 1), `answer_max_steps` (6),
`max_questions_per_turn` (20), `dormant_after_min` (15), `answer_reserve` (0.25),
`default_timeout_s` (120).

## Open questions

- **Q1. Keep nothing from questions?** Forks are discarded (K2), so a knowledge agent never
  learns from what it looked up while answering. Alternative: append a short Q&A digest (question
  and a two-line answer) to its kept conversation. Default: discard.
- **Q2. Beyond the session?** Should a persistent agent's knowledge agents be askable from other
  sessions of the same environment (with the stricter of both sessions' permissions)? Default:
  no, session only.
- **Q3. Web while answering?** Forks may use `web_search` and allowed-domain `web_fetch` (they
  never ask). Should answering be limited to local files and what was already read, to keep
  answers cheap and the injection surface small? Default: allowed, subject to the profile.
- **Q4. Promotion.** Explicit only (K5), or should the root be nudged to promote any read-only
  subagent whose report is longer than a threshold?
- **Q5. Limits.** Are 4 knowledge agents per session, 120 s timeout and 6 steps per answer the
  right defaults for small local models?
- **Q6. Chunk agents as banks.** A `delegate_chunks` agent holding one chunk of a huge input is
  a natural bank ("ask the agent with chunk 7"). Allow promotion once artefacts survive restarts?
- **Q7. Charging.** When per-task budgets exist: charge the asker (as designed) or the knowledge
  agent's own allowance?
