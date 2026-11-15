# 21 — Knowledge agents

**Stages 1 and 2 implemented** (engine core and contract, protocol 0.38; local residency, no
protocol change); stage 3 is designed, not built ([roadmap item 45](12-roadmap.md#next-in-order)). Agents of a session can
ask questions of other agents that keep context they already loaded. A research subagent that
read a module, a specification or a set of pages stays available as a **knowledge agent**: the
root and other subagents ask it (`ask_agent`) instead of searching and reading the same material
again, so context that has been paid for once is reused rather than reloaded into every
implementer's window.

This document is the design: decisions, the contracts they change, and the order to build them.
Each stage below says whether it is built, and [stage 1 as built](#stage-1-as-built) lists where
the code differs from, or settles, what the design left open.
It builds on [agent state](03-agent-engine.md#agent-state) (kept subagent conversations),
[queueing and steering](03-agent-engine.md#queueing-and-steering), the local runtime's slots and
[saved-slot cache](04-models-and-routing.md#runtimes), [permissions](05-tools-and-isolation.md#permissions)
and [budgets](10-configuration.md#budgets).

## Starting point

What the code does today, and what limits the design:

- Only the root delegates (`can_delegate`); subagents are depth 1 and get none of the delegation
  tools (`crates/imperium-engine/src/agent.rs`).
- A finished subagent becomes `idle` and keeps its conversation, system message included, in
  `<data>/sessions/<id>.agents/<agent-id>.json`; the root can `resume_agent` it with a follow-up,
  which appends a turn to that conversation and is collected with `await_tasks`. This is the
  baseline for "asking": only the root can do it, it is serial with the agent's other work, it
  grows the agent's conversation with every question, and the root relays every answer through
  its own context.
- `session.send` and `queue_agent` reach only a subagent that is still running; a reported one
  refuses messages (`-32602`) until it is resumed.
- Local models give each agent (a `holder`, such as `<session>/t3`) back the slot it used last,
  with its cached prompt, and can save a slot to disk and restore it (`LocalModels::suspend`,
  `resume`, `waiting`; `crates/imperium-api/src/local.rs`). Only slots of agents that can come
  back are saved on a swap; a finished subagent's is not, and the saved-slot cache
  (`swap_cache_mb`, `crates/imperium-local/src/slot_files.rs`) trims the files of agents that
  cannot come back first. Saved files do not survive a daemon restart.
- Budgets are checked before every model call and every spawn (`BudgetGate`); per-task budgets
  do not exist yet ([budgets and limits](03-agent-engine.md#budgets-and-limits)).

## Decisions

| # | Decision | Why | Rejected |
| --- | --- | --- | --- |
| K1 | A knowledge agent is an ordinary idle subagent whose record carries a **knowledge card**; nothing new is spawned to hold context | Reuses agent state, persistence, resume and the Agents panel; a knowledge agent is a role, not a new kind of process | A separate "bank" process or store (duplicates agent state); writing findings to memory files only (loses the loaded context, needs foresight) |
| K2 | Each question runs in an **ephemeral fork**: the kept conversation, unchanged, plus the question; the fork is discarded after the answer, and nothing from it is kept | The bank stays exactly what was promoted: no pollution between askers, no leakage of one asker's question to another, no growth toward compaction, and answers do not depend on question order; on a local model the unchanged prefix is the cached prompt, so a fork reads only the question | Appending each question to the agent's own conversation (grows and drifts, needs compaction, serialises everything, leaks askers' questions); keeping a Q&A digest (user decision: keep nothing); the root relaying through `resume_agent` (today's baseline) |
| K3 | Askers are the root and ordinary subagents; **knowledge agents and their forks never ask** | The wait-for graph is bipartite with depth 1, so no cycle or deadlock can form; no depth limit to tune | General peer-to-peer messaging (cycles, a large prompt-injection surface, no clear owner of an exchange) |
| K4 | Forks may call only an **allowlist** of read tools and the web tools, checked when a call is dispatched; everything else, and anything that would ask the user, is refused; at most `knowledge.answer_max_steps` model calls | An answer can look things up again but never changes anything or raises an approval in another agent's name; enforcing at dispatch keeps the tool list, and so the cached prefix, unchanged | Hiding tools in the fork (changes the prefix, so a local fork re-reads everything); plan mode alone (lets MCP and plugin reads through); no tools (an answer cannot check a detail) |
| K5 | Promotion is explicit: the root (`keep_as_knowledge`, or `spawn_agent` with `knowledge`) or the user; the root prompt nudges the root to keep large research reports | The root knows which context will be reused; automatic promotion would keep every research agent's context and its cost | Promote every finished read-only subagent (unbounded residency and clutter) |
| K6 | An answer is **data from that agent**, framed and escaped, never instructions; the asker gains no tool, path or approval through it | Knowledge agents may have read untrusted pages; containment must not depend on the knowledge agent behaving | Trusting answers like the asker's own reasoning |
| K7 | Knowledge agents serve **their own session only** | Permissions, folders, budgets and residency are per session; cross-session sharing needs its own trust rules (user decision) | Environment-wide banks |
| K8 | Agents meant to become knowledge agents are **routed to Emperor by default**; a promoted agent keeps the model it ran on | User decision ("ideally knowledge agents should be Emperor base models"). A fork must use the knowledge agent's own model and cache (no fallback, K2), so the choice is made when the research agent is spawned, not at question time | Switching a promoted agent to Emperor (its whole context would be re-read with nothing cached, and it would answer from context another model built); a fork on Emperor over another model's conversation |

## Tools

All three are defined in `imperium-protocol` like the other built-in tools
([D25](13-decisions.md), [dispatch contract](03-agent-engine.md#dispatch-contract)), exported to
`schemas/tools/` (status `available`: stage 1 builds them), so Emperor is trained on the same schemas.

| Tool | Who has it | Parameters | Result |
| --- | --- | --- | --- |
| `list_knowledge_agents` | root and ordinary subagents | `query?` | Each knowledge agent's card (below), best match to `query` first; empty when there are none |
| `ask_agent` | root and ordinary subagents | `agent` (id or title), `question` (self-contained, at most 4,000 characters), `timeout_s?` (default 300, at most 600) | The answer, framed (see [safety](#safety)); or a tool error saying it timed out, was cancelled or was refused |
| `keep_as_knowledge` | root only | `agent_id`, `topic` (at most 300 characters); or `agent_id`, `retire: true` | The card, or that it was retired |

`spawn_agent` gains an optional `knowledge` (a topic string): the subagent is promoted with that
topic when its first turn finishes successfully; its report still goes to `await_tasks` as
usual. The root prompt and `brief.md` say when to use it (research whose results several later
tasks will need), and the result of `await_tasks` for a read-only subagent whose report or
conversation is large ends with a one-line nudge to keep it with `keep_as_knowledge`.

**Which model (K8).** A subagent spawned with `knowledge`, or with the tag `knowledge` (which the
root prompt tells the root to add to research it means to keep), is routed by a new built-in
[task-class](04-models-and-routing.md#task-classes) rule: prefer the Emperor row. Precedence:
the user's `[[routing.rules]]` first, then this knowledge rule, then the built-in tag rows (so a
`knowledge` task also tagged `architecture` prefers Emperor, not a cloud `frontier` row). It is a
preference rather than a requirement, and it honours the task's tier floor: a task whose `tier`
(or a user rule's) is above Emperor's row skips the preference and routes normally. It falls back
to the normal routing when no Emperor row is installed or it is
[turned off](04-models-and-routing.md#disabled-models-implemented). `routing.knowledge_model`
names another model to prefer instead; when that model is turned off, missing from the model
folders, or not in the catalog, it falls back exactly like a missing Emperor (with a routing
note). `routing.builtin_rules = false` turns the knowledge rule off with the others. An explicit
`model` or `location` on the call still wins. Promoting an existing agent
with `keep_as_knowledge` keeps the model it ran on, and the card shows that model. Emperor is
still in training ([house models](07-desktop-ui.md#house-models-on-first-run-implemented)), so
today every knowledge agent gets the fallback.

**What can be promoted:** an idle (finished, ok), read-only subagent that had tools, whose kept
conversation plus `knowledge.answer_reserve` (25 % of its model's window) fits that window.
Refused, with the reason: a running, paused or failed subagent (finish or resume it first); a
writer (its worktree and branch outlive the question model; open question Q1); a `single_call`
subagent (its context is one reply); `delegate_chunks` chunk and reduce agents (their context is
an in-memory artefact); a conversation that does not fit (it is never compacted to make it fit,
since compaction drops the detail the bank exists for).

**What is frozen at promotion.** The kept conversation, its first (system) message included, and
the `ToolSpec`s the agent was offered, exactly as last sent, are recorded with the card. Every
fork sends them unchanged, so a fork's request differs from the last request the agent made only
by the question at the end.

**The knowledge card** (`KnowledgeCard`, kept on the agent record and in `AgentSummary`):

| Field | Source |
| --- | --- |
| `agent_id`, `title`, `model`, `local` | the agent record |
| `topic` | the promoter's text |
| `sources` | derived by the engine, without a model call, from the conversation's successful tool calls: files read (path, plus size and modified time when read), directories listed, search patterns, URLs fetched; at most 50, newest first, with a count of the rest |
| `untrusted` | `web` and `tools` flags: `web` when any result in the kept conversation came from `web_search`, `web_fetch` or a browser tool; `tools` when any came from an MCP or plugin tool |
| `context_tokens` | the kept conversation's size, and the model's window |
| `est_question_cost` | the estimated tokens and cost of one question: the uncached share of the prefix (from `last_cached_ratio`) plus a typical question and answer; on a hosted model, when the last question is older than the provider's prompt-cache lifetime (from the model table, else 5 minutes), the whole prefix at the uncached price |
| `last_cached_ratio` | the share of the latest fork's prompt that the provider or runtime reported as cached |
| `questions`, `usage`, `cost_usd` | answers given so far, and what they cost |
| `status` | `ready`, `answering` (with queue length), `busy` (resumed for more work) |
| `stale` | sources whose file changed since it was read (checked by size and modified time at each question), and the stale fraction of its file sources |

## Answering

1. `ask_agent` checks that the target is a knowledge agent of this session, that the asker is
   not a knowledge agent or a fork (K3; a debug assertion also checks the asker is not the
   target and waits on nothing), and that the asker has fewer than
   `knowledge.max_questions_per_turn` (20) questions in this turn. It emits `question_asked` and
   puts the question in the knowledge agent's **question queue** (FIFO, separate from its
   message inbox).
2. The knowledge agent's model must be available: a hosted model in the catalog, or a local
   model the model folders have, and not
   [turned off](04-models-and-routing.md#disabled-models-implemented) since promotion
   (`ModelSettings::is_disabled`). Otherwise the question is refused, saying the model is missing
   or "turned off"; it never falls back to another model, which would read the whole prefix with nothing
   cached and answer from a context the card does not describe. The engine also checks, before
   the fork's first call, that the frozen prefix plus the question fits the model's window, and
   refuses the question if not.
3. The asker waits like `await_tasks`, with no model calls. It keeps its local slot while the
   knowledge agent's model has another free slot for the fork; only when the fork needs that
   slot or a model swap is the asker marked asleep (`LocalModels::waiting`). A steering message to
   the root, the asker's turn ending, or the timeout ends the wait.
4. A free answer slot takes the question: at most `knowledge.parallel_answers` forks per
   knowledge agent at once (default 2 on hosted models; always 1 on a local model, so every fork
   uses the one holder whose cached prompt is the prefix).
5. The fork is built from the frozen system message, conversation and tool specs, under a
   derived id (`t3#q7`). Its turn is the question, wrapped (see [safety](#safety)), with a note
   naming the changed sources when the card is stale, and instructions to answer only from what it
   read or re-reads now, to cite paths, lines and URLs, to say "not in what I read" instead of
   guessing, and to say what is missing when the question is ambiguous. Every tool call goes
   through K4's check at dispatch. Its checkpoints are never saved.
6. The reply, capped at 8,000 characters (as a report), returns to the asker; `question_answered`
   is emitted with usage, cost and the cached share; the fork is dropped.

**Degraded answers.** When the stale fraction of the card's file sources is over
`knowledge.stale_threshold` (0.3), the answer's header says it is degraded, and the asker's result
(and the root's, when the root did not ask) advises refreshing the agent with `resume_agent` or
retiring it.

**Timeouts and cancellation.** A question still queued when its timeout passes is removed; one
being answered has its fork cancelled at the next safe point (a model request in flight is
cancelled; a tool call finishes). The same happens when the asker's turn ends, fails or is
cancelled, and when the asker is paused by a steering message to the root. The answer to a
question nobody waits for any more is never delivered. A session stop halts forks like other
agents (`hold_while_stopped`), and **timeouts do not run while the session is stopped**: the
question waits and goes on at resume.

**Resuming a knowledge agent.** `resume_agent` on a knowledge agent is the way to give it more to
know or refresh stale sources: its status becomes `busy`, questions queue until the turn
finishes, and the turn is appended to the kept conversation; the frozen prefix, tool specs and
card are then taken again (a conversation that no longer fits with the reserve retires it, saying
so). This is the only way the kept conversation grows.

**Who pays.** Every fork call is a model call of the session, so it counts in `usage.json` and
every budget scope like any other, and the `BudgetGate` is checked before each call against the
knowledge agent's model; a blocked budget fails the question with the budget message, and one
that asks raises its single per-session prompt. The usage is recorded on the **knowledge agent's
record** (card and `AgentSummary`) and reported to the asker in the result's first line ("answered
by t3 in 2 calls, 3.1k tokens, 92 % cached"). When per-task budgets exist, this stays: the
knowledge agent's record is charged and the asker is told.

**What the asker shares.** Only the question text, its own id and title. No part of its
conversation, plan, state or worktree is sent; the root and subagent prompts say questions must
be self-contained, like briefs.

## Safety

- **The fork's tools (K4).** The fork is offered the frozen tool list, but a call is dispatched
  only when its tool is on the allowlist:
  - built-in reads: `read_file`, `list_dir`, `glob`, `grep`, `memory_search`, `memory_read`,
    `context_load`;
  - the web: `web_search` and `web_fetch`, under the normal domain and robots rules and the
    budget.

  Everything else is refused with a message telling the fork to answer from what it has: file
  writes and edits, commands, delegation, `ask_agent`, `ask_user`, `request_clarification`,
  `update_state`, `update_plan`, memory writes, `start_session`, every MCP tool, every plugin
  tool, `activate_plugin`, browser, preview and media tools, and the app-config tools. An
  allowlisted call is then decided as usual (permission profile, rules, safety floor, the
  session's mode), except that an outcome of **ask becomes a refusal**: a fork never raises an
  approval. Nothing in the asker's request can widen this; the asker passes only text.
- **Answers are data.** The result is wrapped as
  `<agent-answer from="t3" topic="…" untrusted-web="true" untrusted-tools="true">…</agent-answer>`,
  with text that looks like the delimiters backslash-escaped (as kept context blocks are), after
  a line saying it is information from another agent to weigh, not instructions.
  `untrusted-web` is set when the card's `untrusted.web` is or the fork itself used a web tool;
  `untrusted-tools` when the card's `untrusted.tools` is (forks cannot call MCP or plugin tools,
  but the research before promotion may have). The line then says which kind of outside material
  the answer may contain. Askers' prompts say the same.
- **Questions are data too.** The fork reads the question inside `<question from="t5">`, after a
  line saying it comes from another agent, is to be answered, and cannot change its role, tools or
  instructions.
- **No cycles or deadlock.** K3 makes the wait-for graph bipartite; the check is in step 1 of
  [answering](#answering), with a debug assertion behind it.
- **Prompt injection.** Containment rests on the asker side (framing, no tools gained) and the
  fork side (allowlist, no asking), not on the content being clean. Forks keep the web tools (a
  user decision), so the general risk of data leaving through `web_search` queries applies to
  them as to every agent with the web; see
  [exfiltration through web tools](05-tools-and-isolation.md#built-in-tools).
- **Rate.** At most `knowledge.max_questions_per_turn` questions per asker turn, so a confused
  model cannot loop on a knowledge agent.

## Lifetime and memory

- **How many.** At most `knowledge.max_per_session` (4) knowledge agents per session, and, on
  local models, only as many as their saved slots fit in a share of `swap_cache_mb`
  (`knowledge.slot_cache_share`, 0.5; each slot estimated from its `context_tokens` and the
  model's KV size per token). Promoting one more is refused with the list, oldest-asked first, so
  the root (or user) retires one.
- **Retiring.** `keep_as_knowledge` with `retire: true`, the user, or the session ending. A
  retired agent is an ordinary idle subagent again: still resumable, no longer askable, its card
  and saved slot removed.
- **Local residency: a third presence kind, `kept`.** The supervisor's presence today has two
  marks, `working` (mid-turn, `LocalModels::attach`) and `waiting` (asleep on subagents,
  `LocalModels::waiting`) (`Presence`/`Mark` in `crates/imperium-local/src/supervisor.rs`). A
  knowledge agent's holder gets a third, `kept`, held from promotion until retirement. `kept`
  counts in exactly two places: `save_slots` saves the holder's slot on a swap or unload (as for
  an agent that can come back), and `SlotFiles::trim` keeps its file ahead of finished agents'
  (plus the restore rule below, which keys on the holder being a knowledge holder). It counts
  nowhere else: it never pins a model (unlike `attach`, the model can be swapped or
  unloaded for anyone), never counts as working or waiting (not in `critic_may_have_gpu`, no
  `yield_waiters` or suspend-to-yield on its behalf, and it does not hold off the idle unload).
  A fork itself is `working` under the holder for its turn, as any agent is.
- **Saving after every fork.** When a fork ends, whatever the outcome (answered, timed out,
  cancelled, refused or failed), the holder's slot is written to its file **without erasing its
  cells**. There is no idle timer. A hosted model holds nothing between calls; its provider's
  prompt cache is what `last_cached_ratio` reports.
- **Restoring only when needed.** Today `Supervisor::restore` (called on every lease) restores
  whenever the holder has a saved file and then forgets the file (the `Forget` guard), which
  would make a saved-but-unerased slot never a hot hit and protect it for one restore only. For
  `kept` holders it changes in two ways:
  - it restores only when the leased slot's last holder is not this holder, or its cells were
    freed (erased, or the model was restarted or swapped) since; otherwise the lease goes ahead on
    the cells as they are, recording no `RestoreSlot` move;
  - the file is kept after it is read, and removed only when the agent is retired or resumed
    (`resume_agent` grows the conversation, so the prefix and its file are made again) or by
    `SlotFiles::trim` under its priority. Other holders keep today's behaviour.
- **Compaction.** The kept conversation is never compacted: questions do not grow it (forks are
  discarded), and promotion refuses a conversation that does not fit with the reserve. A fork
  that reaches `context.auto_compact_at` stops and answers with what it has.
- **Restarts.** The card and the frozen prefix are part of the agent record, so with
  `sessions.agent_state_keep` on, a resumed session keeps its knowledge agents; saved slot files
  do not survive a restart, so the first question reads the prefix again. Questions in flight at
  a restart are lost; their askers' turns were cut off anyway. With the setting off, and in
  project-agent sessions, knowledge agents last as long as the daemon run.
- **Branches and rewinds.** A session made by `session.branch` or `session.rewind` does not carry
  knowledge agents: its copied agent records keep no card, so they are ordinary idle subagents.

## Visibility

**Protocol 0.38** (additive):

- `AgentSummary.knowledge: Option<KnowledgeCard>`.
- Session events: `knowledge_changed` (`agent_id`, the card, or `retired`),
  `question_asked` (`question_id`, `from`, `to`, `text` clipped to 2,000 characters, `at_ms`) and
  `question_answered` (`question_id`, `outcome`: `answered`, `timeout`, `cancelled`, `refused`
  or `failed`, `answer` clipped to 2,000, `degraded`, `untrusted_web`, `untrusted_tools`, `usage`, `cost_usd`,
  `cached_ratio`, `at_ms`).
- A fork emits **no** `turn_started`, `text_delta` or model-call events in the first version. Its
  tool events (`tool_started`, `tool_finished`, permission refusals) carry the derived id
  (`t3#q7`), which the agent registry ignores, so no agent record is created for a fork.
- Method: `session.agent_knowledge` (`session_id`, `agent_id`, `topic` or `retire`) for the user
  to promote or retire.

**Journal.** `knowledge_changed`, `question_asked`, `question_answered` and the forks' tool events
are journalled like every session event, so replay rebuilds cards and the question history. A
fork's conversation is not saved as a conversation (no `.agents` record, no messages file), but
its question, tool calls and answer are in the journal. Export and sharing
([19](19-sharing-and-sync.md)) treat questions and answers like other transcript content,
including redaction.

**Desktop** ([07](07-desktop-ui.md#agents-panel-implemented)). The Agents panel marks knowledge
agents with a *Knowledge* badge and the topic; the agent's view lists its sources (stale ones
marked), its card's cost estimate and cached share, and its questions with asker, outcome and
cost; *Keep as knowledge* and *Retire* sit beside *Continue with…*.

**CLI.** `imperium chat` shows cards in `/agents`.

## Interactions

- **Parallel writers.** Writers ask like any subagent but cannot be knowledge agents in the first
  version (see [what can be promoted](#tools)). A knowledge agent reads the session's folders, not the asker's worktree; the answer's
  header names them, so a writer knows its own edits are not reflected. The staleness check
  catches files the root merged since.
- **Harness aids** ([03](03-agent-engine.md#harness-aids-for-small-models)). De-reference and
  state apply to the root's user messages only and are not applied to questions or forks. The
  root prompt tells the model to name concrete entities in questions (the state block helps it
  do so).
- **Initiative and persistent agents** ([14](14-projects-and-automation.md#initiative-implemented)).
  Reviews may ask their own session's knowledge agents (asking changes nothing). Knowledge agents
  are not shared across a persistent agent's sessions (K7).
- **Unattended sessions.** Nothing in a fork asks the user, so questions never wait on an
  approval; local calls use the session's priority (`Background` for schedules and project
  agents), and budgets that ask pause as they do for any call.
- **Steering and stops.** Steering the root pauses its subagents as today; a paused asker's
  questions are cancelled. Knowledge agents are idle, so they are not paused; their forks for
  other askers go on.

## Implementation plan

1. **Engine core and contract (protocol 0.38) (implemented).** Tool definitions and schemas
   (`ask_agent`, `list_knowledge_agents`, `keep_as_knowledge`, `spawn_agent.knowledge`); the card
   and the frozen prefix and tool specs on the agent record, `AgentSummary.knowledge`, sources from
   the conversation; a `can_ask` flag beside `can_delegate`; the question queue; forks under a
   derived id with K4's dispatch-time allowlist and ask-as-refusal; model availability and fit
   checks; timeouts (paused while stopped) and cancellation with the asker's turn; the budget
   gate per call; framing, escaping and the degraded and web markers; events and journal replay;
   `session.agent_knowledge`; the `await_tasks` nudge; the `knowledge` routing rule (K8).
   *Tests* (fake model server):
   - a subagent asks a promoted agent and gets its answer;
   - the fork's request is byte-identical to the agent's last request up to the question (system
     message, conversation and tools), and the kept conversation is unchanged after three
     questions;
   - a fork's `write_file`, `run_command`, `ask_agent`, MCP, plugin and `activate_plugin` calls are
     refused at dispatch, a `web_fetch` the rules would ask about is refused, and an allowed
     `web_fetch` runs and marks the answer `untrusted_web`;
   - an agent that used an MCP tool during its research is promoted with `untrusted.tools` set
     and its answers carry `untrusted-tools`; one that used `web_fetch` has `untrusted.web`;
   - a knowledge agent's own `ask_agent` call is refused;
   - a timeout removes a queued question and cancels a running fork; a stopped session does not
     time a question out;
   - an unavailable model, or a prefix that does not fit, refuses the question with no fallback;
     a model turned off after promotion refuses it with "turned off";
   - promoting a writer, a chunk agent or an oversized conversation is refused;
   - a blocked budget fails the question; usage lands on the knowledge agent's record;
   - answer delimiters inside the answer are escaped;
   - replaying a journal restores cards and does not create records for `t3#q7`; a branched
     session has no knowledge agents;
   - `spawn_agent` with `knowledge` routes to an Emperor row when the catalog has one, to the
     normal route when it has none or it is turned off, and to the named model with an explicit
     `model`; `keep_as_knowledge` keeps the agent's model;
   - a task tagged both `knowledge` and `architecture` prefers Emperor over the cloud `frontier`
     row; a user rule for `knowledge` beats the built-in rule; a tier floor above Emperor's row
     routes normally; a `routing.knowledge_model` that is turned off, missing or outside the
     catalog falls back like a missing Emperor;
   - the schema drift check passes.
2. **Local residency (implemented).** The `kept` presence kind; the slot saved after every fork, whatever its
   outcome, without erasing its cells; for `kept` holders, `Supervisor::restore` restoring only
   when the leased slot's last holder is someone else or its cells were freed, and keeping the
   file after reading it (removed on retirement or resume); slot files counted against
   `swap_cache_mb` in `max_per_session`; the asker keeping its slot when a free one exists;
   `last_cached_ratio` and `est_question_cost` (with the cache-lifetime rule). *Tests* (fake local
   runtime):
   - a second question with nothing in between is a hot hit: it reports cached tokens and records
     no `RestoreSlot` move;
   - with a third agent using the model between two questions, the second still reports cached
     tokens (restored from the saved file), and the file still exists after that restore, so a
     third question after more interference is restored again;
   - a fork cancelled mid-answer, then another agent using the model: the next question still
     reports cached tokens (the cancelled fork's slot was saved);
   - retiring or resuming the knowledge agent removes its file;
   - `kept` holders' slots are saved by `save_slots` on a swap and on an idle unload;
   - `SlotFiles::trim` drops finished agents' files before `kept` holders' files;
   - a model whose only presence is `kept` can be swapped out for another load and is unloaded
     at its idle timeout (`kept` never pins it or holds off the idle unload);
   - `kept` does not count as working or waiting: `critic_may_have_gpu` ignores it, and no
     `yield_waiters` or suspend-to-yield runs on its behalf;
   - a promotion over the slot-cache share is refused with the list;
   - `est_question_cost` on a hosted model uses the uncached price after the cache lifetime.
3. **Clients and prompts (not built).** Agents panel badge, sources, cost and questions; *Keep as knowledge*
   and *Retire*; `/agents` cards in `imperium chat`; root and subagent prompt lines, `brief.md`,
   Emperor's root prompt and an eval case in `schemas/eval` (asking beats re-reading). *Tests:*
   desktop component tests for the badge and the question list; a browser-preview check of the
   panel.

**Later, not in the first version:** fork streaming events, an *Ask* box and
`session.agent_ask` for the user, phone access, `ask_agent` in `imperium mcp-serve`
([20](20-claude-harness.md)), task-tree edges for waiting questions, and a wait-for map for deeper
asking.

**Acceptance for the item.** One task (a research subagent reads a crate; two subagents then
implement against it) run twice on the same model, once with the research agent kept as a
knowledge agent and asked, and once with the baseline (the root relaying questions through
`resume_agent`), on one local and one hosted model. Measured from the recorded runs, not assumed:
uncached prompt tokens and cost, including the knowledge agent's original research; the askers'
peak context; the number of read and search calls; and wall time. The item passes when the
knowledge-agent run uses fewer uncached prompt tokens and smaller asker peak context than the
baseline on both models, and no answer caused a write, a command or an approval.

## Stage 1 as built

Everything in the stage 1 list above is built and covered by `crates/imperium-engine/tests/knowledge.rs`
and the routing tests in `crates/imperium-engine/src/model.rs`. Where the code settles a detail the
design left open, or differs from it:

- **Model-call events.** A fork emits no `turn_started`, `text_delta`, context breakdown or turn
  events, but each of its model calls is reported as a `model_call_completed` under the **knowledge
  agent's** id: that is what budgets, `usage.json` and the resource view count, and what charges the
  knowledge agent's record.
- **Which tools are offered.** `keep_as_knowledge` is offered to the root whenever
  `knowledge.enabled`. `ask_agent` and `list_knowledge_agents` are offered to the root and to
  ordinary subagents while the session has at least one knowledge agent (a subagent sees them from
  its next turn), so a session without knowledge agents keeps its tool list, and its cached prefix,
  as it was. A call to them from any agent that may not ask (a chunk or `single_call` agent, a
  knowledge agent resumed for more work, a fork) is refused.
- **Frozen prefix.** A subagent notes its system message, the tools it was offered and the reasoning
  effort its request carried (forks use the same effort) at every safe point, in memory; promotion records them with the card. A subagent that finished before a
  daemon restart has none, so it must be resumed once (any follow-up) before it can be kept.
- **Emperor's row** is a catalog row tagged `emperor` or with the table id `emperor` (any case),
  and the first such row that can take the task is used.
- **Escaping** of the answer and question delimiters ignores case.
- **Settings** under `[knowledge]` and `routing.knowledge_model` are read when a session starts, so
  a change applies to new sessions.
- **Writers** are recognised by the `access: write` it was spawned with, whether or not a worktree
  could be made (the kind is kept in the record).
- **Sources** are read from the successful tool calls of the kept conversation when it is
  promoted. A file's size and modified time are those it has at promotion, and a file modified
  after its agent's last turn started is marked stale at once (it may have changed after it was
  read), so an edit between the research and `keep_as_knowledge` gives a stale note, a stale share
  on the card and degraded answers. The mark stays until the agent is refreshed with
  `resume_agent`.
- **Staleness note to the root.** A degraded answer's header advises the asker; when the asker is
  not the root, the user also gets a notice (the root's context is not changed).
- **Cost estimate.** `est_question_cost` counts the uncached share of the prefix from
  `last_cached_ratio` plus a typical question and answer (the cache-lifetime rule is in
  [stage 2 as built](#stage-2-as-built)).
- **Asker and local slots.** An asker on a local model sleeps (`LocalModels::waiting`) while it
  waits, as in `await_tasks`, unless stage 2 lets it keep its slot. A fork uses the knowledge
  agent's holder, and a local model answers one question at a time.
- **No usage-limit waits in a fork.** A fork that hits a provider usage limit fails the question
  rather than waiting for the reset; the asker's budget and limits are the ordinary ones.
- **Not built in stage 1:** local residency (built in stage 2), the Agents panel, the
  `/agents` cards, the root and subagent prompt lines, `brief.md`, Emperor's root prompt and the
  eval case.

## Stage 2 as built

Local residency is built in `imperium-local` and used by the engine through five new defaulted
methods of `LocalModels` (`keep`, `save_kept`, `drop_saved`, `fork_fits`, and the two readings
`swap_cache_bytes` and `kv_bytes_per_token`); no protocol type changed. Tests:
`crates/imperium-local/tests/kept.rs`, the unit tests in `supervisor.rs` and `slot_files.rs`, and the
local-runtime tests at the end of `crates/imperium-engine/tests/knowledge.rs`.

- **`kept` presence.** `keep(model, holder)` returns a presence held from promotion (and taken
  again at the first question after a restart) until the agent is retired. It counts in two places
  only: `save_slots` saves a kept holder's slot on a swap or an idle unload, and `SlotFiles::trim`
  drops the files of holders that cannot come back before it. It never pins a model, is neither
  working nor waiting (`claims`, `critic_may_have_gpu`, `yield_waiters`, the idle unload and the
  swap policy look at those two only), and a model whose only presence is `kept` is swapped or
  unloaded as an idle one.
- **Saving after every fork.** When a fork ends, whatever the outcome (answered, timed out,
  cancelled or failed; a question refused before any call has nothing to save), the engine calls
  `save_kept`: the holder's slot is written to its file with its cells left in place (reported as a
  `save_slot` move with the reason `kept`). It is skipped when another agent has already taken the
  slot, or slots are not saved.
- **Restoring only when needed.** The slot table says whether the slot a lease got still holds its
  holder's own cells (it used the slot last and the slot was not reserved for a restore or erased).
  For a kept holder, `Supervisor::restore` does nothing in that case (no `RestoreSlot` move) and
  otherwise restores from the saved file, which stays after it is read. Other holders keep today's
  behaviour.
- **File lifetime.** A kept holder's file is removed by `drop_saved`, which the engine calls when the
  agent is retired and when it is resumed (its prefix changes), and by trim under its priority.
- **Slot cache share.** Promoting a local agent is refused, with the list oldest asked first, when
  the saved slots of the session's local knowledge agents (each `context_tokens` times the
  model's KV bytes per token) would exceed `knowledge.slot_cache_share` of `swap_cache_mb`. The
  check is skipped when slots are not saved or the model's KV size is unknown.
- **Asker keeps its slot.** An asker on a local model stays awake while the fork runs on a hosted
  model, or on a loaded local model with a slot free for it (beside the asker's own when it is the
  same model: `fork_fits`); otherwise it is marked asleep as in stage 1. The fit is judged when
  the question starts.
- **Cache lifetime.** On a hosted model, `est_question_cost` prices the whole prefix uncached when
  the last question is older than the model's cache lifetime: `cache_lifetime_s` on its row of the
  model table, else 5 minutes. A card never asked has no cached share to lose, so it is priced
  uncached anyway. The estimate is recomputed when `session.agents` or `list_knowledge_agents` reads
  the card, not stored.
- **Replay time.** A replayed journal dates each turn by the time the journal recorded, so a staleness
  mark from before a restart keeps its meaning; a turn without a known time marks nothing.

## Settings

Under `[knowledge]` ([10](10-configuration.md#knowledge-agents)): `enabled` (true), `max_per_session` (4),
`parallel_answers` (2; local models always 1), `answer_max_steps` (6),
`max_questions_per_turn` (20), `answer_reserve` (0.25), `slot_cache_share` (0.5),
`stale_threshold` (0.3), `default_timeout_s` (300; a call may ask for at most 600), and under
`[routing]`, `knowledge_model` (empty: Emperor when present; see K8).

## Settled questions

Decided by the user:

- **Nothing is kept from questions** (K2): no Q&A digest is appended.
- **Own session only** (K7).
- **Forks may use the web** (`web_search`, `web_fetch`) under the normal rules and budget; answers
  that used web material are marked.
- **Explicit promotion**, with a nudge to the root for large research reports (K5).
- **A question's time limit is 300 s by default** (at most 600 s), and it pauses while the session
  is stopped.
- **Knowledge agents should run on Emperor** where it is available (K8).

Decided as defaults, which the user may revisit:

- 4 knowledge agents per session and 6 steps per answer.
- `delegate_chunks` chunk and reduce agents are not promotable.
- Questions are charged to the knowledge agent's record and reported to the asker.
- Branched and rewound sessions do not carry knowledge agents.
- A stale fraction over `stale_threshold` marks answers degraded and advises a refresh.
- Question timeouts pause while the session is stopped.
- Export and sharing treat questions and answers like other transcript content.

## Open questions

- **Q1. Writers as knowledge agents.** A writer that explored and changed a module would be a
  good bank, but its worktree and branch must then outlive merges and cleanup. Allow promotion,
  answering from its worktree read-only, once worktree lifetime is settled?
