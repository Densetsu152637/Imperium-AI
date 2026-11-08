# 03 — Agent engine

## Concepts

| Term        | Meaning                                                                       |
| ----------- | ----------------------------------------------------------------------------- |
| Project     | The environment (folder, container, or SSH host) plus its config; see [14](14-projects-and-automation.md) |
| Session     | A conversation inside a project, with its own root agent                      |
| Project agent | An optional persistent agent per project that dispatches work into new sessions |
| Agent       | A loop of model calls and tool calls with its own context, model, and budget  |
| Root agent  | The agent the user talks to; manages delegation                               |
| Subagent    | An agent spawned by another agent for a bounded task                          |
| Task        | The unit of delegated work: brief, requirements, budget, result contract      |
| Event       | Anything that happened, published on the session event bus                   |

## Agent loop

1. Assemble context: policy layers ([08](08-agent-policy.md)), session memory, task brief,
   tool schemas (filtered by permissions), and conversation.
2. Call the model (streaming). Record tokens, latency, throughput, and cost as they arrive.
3. Execute tool calls. Independent calls run concurrently; each passes through hooks and
   permission checks ([06](06-extensibility.md#hooks)).
4. Append results; repeat until the model ends its turn, the budget is exhausted, or the task
   is cancelled.

**Implemented (phase 1):** a single agent per session with steps 2–4: streaming model calls
with the built-in tools, sequential tool execution with permission prompts, and a stop after
`agents.max_steps` model calls per turn. The system prompt is a fixed instruction naming the
project folder; policy layers, memory, hooks, and concurrent tool calls come later. Each turn
emits `turn_started`, `text_delta`, `permission_requested`, `tool_started`, `tool_finished`,
`model_call_completed` after every model call (tokens, duration, time to first token, tokens
per second, cost) followed by that call's `context_breakdown` ([context
management](#context-management)), and `turn_completed` (token usage, cost, model calls) or
`turn_failed`.

**Model clients (implemented):** the agent calls its model through a `ChatClient`
(`imperium-models`), built for the turn from the session's `ChatClients` by the endpoint's `kind`
([configuration](10-configuration.md)): it streams a completion with its usage and may offer the
model's `Tokenizer` (token counts, chat-template rendering, and the context window), which
[context breakdowns](#context-management) use. The built-in kinds are `openai` (any
OpenAI-compatible endpoint; one on this machine may be llama.cpp, so its tokenizer is tried) and
`llamacpp` (always asked for its tokenizer; local llama.cpp models use it). A client for another
API is registered under a new kind without changing the agent.

**Tool providers (implemented):** tools beyond the agent's own (files, commands, planning,
delegation, memory, skills, workflow context) come from `ToolProvider`s (`imperium-api`) that the
daemon gives each session: web and browser (`imperium-web`), media (`imperium-media`), and MCP
servers (`imperium-mcp`). A provider lists its tools, may add system-prompt instructions (the web
citation rules) and start-up work (MCP servers connect before the first turn, failures becoming
notices), says how each call is checked (its summary, whether to ask, whether it waits for
required context), and runs it. The agent runs `tool.before` hooks, the required-context rule,
the approval, and `tool.after` hooks around every call, as for its own tools. MCP providers'
tools are offered right after the file and command tools and the others' after the agent's
planning, skill, context, and session tools, so the order the model sees is the same as before.

Tool calling works with native function-calling models and, as a fallback, a constrained
JSON/grammar format (llama.cpp grammars, vLLM guided decoding) for models without native
support.

## Planning

**Implemented (phase 3):** the `update_plan` tool (a goal and steps with `pending`,
`in_progress`, `done`, `blocked`, or `skipped`) and the `plan_updated` event, shown as a live
checklist in the desktop app and the CLI. Not yet: the automatic plan phase, approval modes,
progress checks, and re-planning described below.

Planning is built into the runtime (`imperium-engine`), not left to prompts, so every agent —
whatever model it runs on — works toward an explicit, tracked goal.

- **Plan object.** Each agent can hold a plan: a goal, acceptance criteria, and ordered steps
  (`pending`, `in_progress`, `done`, `blocked`, `skipped`), each optionally linked to the tasks
  delegated for it. Plans are stored in the session journal and streamed to clients
  (`plan.updated`), where the UI shows them as a live checklist.
- **Automatic planning.** When a request is non-trivial (a classifier pass, configurable
  thresholds, or the policy pack's rules), the engine runs a **plan phase** first: the agent
  may only read and search, then proposes a plan. Small requests skip straight to execution.
- **Plan approval modes:** `auto` (execute immediately), `review` (user approves or edits the
  plan; default for root agents), or `policy` (the policy pack decides by scope or risk).
- **Plans drive delegation.** Independent steps can be turned into subagent tasks in one
  action; the parent's plan advances as completion events arrive, without extra model calls.
- **Staying on course.** The engine injects a compact plan summary into each turn, flags turns
  that make no progress against the current step, and prompts a re-plan when steps fail, the
  budget burns faster than progress, or the user changes the goal. Re-plans keep a diff of what
  changed.
- **Verification step.** Plans end with a check against the acceptance criteria; the agent
  reports what is met, what is not, and what was not verified.
- Tools: `plan.propose`, `plan.update`, `plan.advance`, `plan.complete`. Project agents
  ([14](14-projects-and-automation.md)) use the same plans for long-running goals.

## Reactive delegation

**Implemented (phase 3):** the root agent has `spawn_agent` (a self-contained brief, an optional
minimum tier, or a named catalog model) and `await_tasks` (some or all task ids). Each subagent
is its own task running in parallel; `await_tasks` waits on them with no model calls and returns
their reports (up to 8,000 characters each). Subagents cannot delegate further (depth 1) and
share the session's permission approvals. Events carry an `agent_id` (`root`, or the task id),
and the session emits `task_spawned` and `task_finished` (status, report, tokens, cost).
Not yet: `send_to_task`, budgets per task, deeper trees, and cancellation.

Delegation must not burn tokens while subagents work.

- `spawn_agent` (a built-in tool) creates a **Task** and returns immediately with a task ID.
- The parent continues if it has independent work; otherwise it calls `await_tasks` and its
  loop **suspends**: no model calls are made, and its context is not resent.
- When a subagent finishes, fails, or asks a question, the engine posts a **completion event**
  to the parent. Only then does the parent resume, receiving a compact result (the subagent's
  final report, capped in size, plus artefact references), never the subagent's transcript.
- Progress (`task.progress`) is streamed to the UI and telemetry, not to the parent model.
- A parent can `send_to_task` to steer a running child; the child sees it at its next turn.
- Questions a child cannot resolve escalate up the tree; the root escalates to the user.

```text
root ──spawn A──spawn B──await──────────────(sleeping, 0 tokens)──resume(A,B results)──▶
        A ───────────────────────────▶ done ┘
        B ──────────────────▶ done ─────────┘
```

## Task contract

```jsonc
{
  "brief": "Summarise failing tests in crates/core and propose fixes",
  "requirements": { "tools": ["fs.read", "exec"], "min_context": 32768,
                    "capabilities": ["tool_calling"], "quality_tier": "standard" },
  "budget": { "max_tokens": 200000, "max_cost_usd": 0.50, "max_wall_s": 900 },
  "environment": "inherit",            // or a narrower folder / container / host
  "result": { "format": "markdown", "max_chars": 4000 }
}
```

The router ([04](04-models-and-routing.md#routing)) resolves `requirements` to a concrete
model; the parent model never needs to name one, though it may express a preference.

## Budgets and limits

- Budgets are hierarchical: a child's budget is carved from its parent's.
- Limits: max depth (default 3), max concurrent agents per session (default 8, bounded by
  available runtimes), max total cost per session.
- Hitting a budget ends the child with a partial result and a `budget_exhausted` status.

## Context management

**Context breakdown (implemented):** after every model call, the agent that made it emits
`context_breakdown`: what that call's request held, by category, against the model's context
window, like Claude Code's `/context`. Each agent (the root and every subagent) reports its own
calls, under its agent id.

The agent records what each part of a message is as it builds it, so the request divides into
labelled segments without guessing: the system prompt's parts (base instruction, skills
listing, memory instructions, web citation rules, project instructions), the tool definitions
as the JSON actually sent (built-in and MCP tools separately), and, for each user message, its
workflow context manifest, recalled memories, and the user's own text. Assistant messages
count their text plus each tool call's name and arguments; tool results count their content.
Conversations restored after a daemon restart recognise the manifest and memories by the tags
they are wrapped in.

| Category key | Label |
| --- | --- |
| `system_base`, `system_skills`, `system_memory`, `system_citations`, `system_project` | The system prompt's parts |
| `tools_builtin`, `tools_mcp` | Tool definitions |
| `summary` | A [compacted](#switching-models-and-compaction) conversation's summary of its older turns |
| `manifest`, `memories` | Workflow context manifest and recalled memories, from user-message preambles |
| `user`, `assistant`, `tool_results` | The conversation |
| `overhead` | Chat template and formatting: roles, call ids, separators, template markup |

Empty categories are left out. Counting uses the best method available, named in `method`:

- **`exact`**: when the model's client offers a tokenizer: local llama.cpp models, `llamacpp`
  endpoints, and any `openai` endpoint on this machine that answers llama.cpp's
  `POST /tokenize`. Each segment is counted with the model's own tokenizer, without
  BOS and with special tokens parsed as the chat template's output is. Counts are cached by
  content hash per model, so unchanged messages are not tokenized again on later calls. Tool
  definitions count as the chat template renders them, not as the JSON sent: templates add
  their own tool instructions and re-serialise the JSON (Qwen3's spends about 2.6 times the
  sent JSON's tokens on one tool). Each tool set is measured once with `POST /apply-template`,
  as the difference it makes to a minimal conversation. The
  overhead is the provider's reported `prompt_tokens` minus the segments; it absorbs token
  merges across part boundaries, and is negative when the template drops text the request
  carried (such as earlier reasoning). The context window is the server's per-request context
  from `GET /props` (`default_generation_settings.n_ctx`, or the older shapes).
- **`calibrated`**: every other endpoint, or when the tokenizer fails or takes more than five
  seconds. Each segment is estimated with a cheap heuristic (words, punctuation runs, line
  breaks) and the formatting with a per-message allowance; the estimates are then scaled, with
  largest-remainder rounding, so they add up to the reported `prompt_tokens` exactly. The
  context window comes from the model table's `context_window`, else it is unknown; it is never
  invented.
- **`estimated`**: the provider reported no usage. Segments use the tokenizer when available,
  else the heuristic, and the overhead is the formatting allowance.

Whenever usage is reported, the categories (overhead included) add up to `prompt_tokens`. The
event also carries `cached_tokens`. Breakdowns are computed after the call completes, so they
never delay streaming, and they never fail a turn.

`imperium chat` prints the root agent's latest breakdown with `/context` (a used/free bar and a
table with each category's share of the window); `--context` adds a one-line summary after each
turn. The desktop shows it in the session's *Context and outputs* section
([07](07-desktop-ui.md#environments-and-sessions-implemented)).

- Planned: per-agent context windows track usage and, near the limit during a turn, the engine
  compacts automatically with the [same compaction](#switching-models-and-compaction) a model
  switch uses (today it runs on a switch or when asked).
- Large tool outputs are stored as artefacts; the model receives a truncated view plus a
  handle it can page through.

## Switching models and compaction

**Implemented:** a session's root agent can switch to another model or endpoint mid-session
and keep its conversation (`Session::set_model`; `session.control` with `set_model` and a model
table id or `<endpoint>:<model>` spec, or none for `models.default`). The daemon resolves the
model as `session.create` does, so an unknown endpoint or a missing key is refused with
`-32004` before anything changes.

- **When it applies:** between turns only. The switch is handled by the session's task when no
  turn is running and takes effect from the next turn; while a turn runs (including one paused
  for a usage limit or waiting on a permission) it is refused with `-32006` ("a turn is
  running"), never applied mid-stream. Messages sent meanwhile [queue](#queueing-and-steering) as usual. Running
  subagents keep their own models; subagents spawned later fall back to the new model.
- **Measuring:** the conversation the target would read (system prompt, every message, and
  the tool definitions) is counted with the target's client, by the [context
  breakdown](#context-management) machinery: exactly with its tokenizer when it has one (a
  local llama.cpp model is loaded for this, as its next turn would), else with the heuristic
  scaled by how far it was off on the latest call that reported its prompt size
  (`calibrated`), else the heuristic alone (`estimated`). The chat template's formatting is
  estimated. The window is the server's (`/props`), else the model table's `context_window`;
  when neither is known the conversation is not checked and the switch goes ahead.
- **Budget:** the window less 15% for the reply (no per-model output limit is configured, so
  a fixed share stands in for one). A conversation within it switches unchanged.
- **Compaction:** otherwise older messages are replaced by a summary. The system prompt is
  always kept; the most recent messages are kept verbatim, as many as fit in 60% of the budget
  (so the conversation can go on) after the system prompt, tools, and room for the summary (a
  third of what is left, 32 to 2,048 tokens). The kept part starts at a user turn when one
  fits, else at an assistant message within the last turn; it never starts at a tool result,
  so every kept tool call keeps its result. The **current** model writes the summary, since it
  has been reading the conversation and its window holds it; if it cannot be reached (an
  endpoint that is down, a local model that will not load), the target model writes it. The
  summarisation prompt asks it to keep the user's goals and constraints, decisions and their
  reasons, file paths, commands, identifiers, and versions, what the important tool results
  showed, and open tasks and next steps, folding in any earlier summary (tool results over
  2,000 characters are cut in what it reads). The summary goes before the first kept user
  message, tagged `<conversation-summary>` (so roles still alternate, and breakdowns count it as
  `summary`), or in a user message of its own when the kept part starts mid-turn. The result
  is measured again: a summary too long is shortened, and if the kept messages are still too
  many, fewer are kept and the summary rewritten, up to six attempts.
- **Refusal:** when the system prompt and tools alone (plus a minimal summary) exceed the
  budget, or compaction cannot get under it, the switch is refused with `-32007` and the
  session keeps its model and conversation untouched. A summary no model could write is
  refused with `-32004`.
- **Events and results:** compaction emits `context_compacted` (the target, tokens before and
  after, messages summarised, budget, window, counting method, and the summarising model), then
  the switch emits `model_changed` (`from`, `to`, `compacted`); the session summary's `model`
  follows. `session.control` returns `model` and `context` (`tokens`, `context_window`,
  `budget`, `method`, and `compaction` with the token counts and summarised messages).
- **Persistence:** `model_changed` is journalled, and a resumed session starts with the model
  the journal last switched to (the daemon resolves its spec again); the compacted
  conversation is saved to `<id>.messages.json` straight away.
- **Compact now:** `session.control` with `compact` runs the same compaction for the
  session's own model, keeping the last two turns verbatim (fewer if they do not fit), and
  does nothing when there is nothing older. It needs a known context window.

`imperium chat` switches with `/model <id>` and compacts with `/compact`, printing what
happened (`Switched to x; compacted 30.1k -> 9.2k tokens (12 messages summarised by y).`). The
desktop has a model picker in the session header ([07](07-desktop-ui.md#environments-and-sessions-implemented)).

## Queueing and steering

**Implemented:** messages sent while an agent is busy are never rejected or interleaved; they
wait, or join the running turn when the user asks.

- **Busy:** the root agent is busy from `turn_started` until `turn_completed` or `turn_failed`,
  including while it is paused for a usage limit or waiting for a permission answer. A
  subagent is busy until it reports.
- **Queue (the default):** `session.send` to an idle root starts a turn at once, as before.
  While it is busy the message is queued: `message_queued` (`id`, `text`, `steer`) and then
  `queue_changed` (the whole queue) are emitted, and when the turn ends the queue is delivered
  in order, one new turn per message (a failed turn does not drop it). Every message gets an
  id (`q1`, `q2`, ... unique in the session) that `session.send` returns.
- **Steer:** `session.send` with `mode: "steer"` puts the message into the **running** turn.
  The agent takes it at the next safe point: after the current model response and all of its
  tool calls have their results, before the next model call. It is added as a user message
  wrapped in `<steering>` (with a line saying it arrived while the agent was working and
  nothing was cancelled), the agent emits `steered` (`id`, `text`), and the turn goes on. A
  steering message that arrives while the model writes its final answer keeps the turn going
  for one more call instead of letting it end. Steering never lands between a tool call and
  its result. With no turn running, steering is an ordinary send.
- **Editing the queue:** `session.queue` (a method of its own, since it answers with the
  queue) with `action` `list`, `edit` (`id`, `text`), or `remove` (`id`), returning the queue
  after the action. A message already delivered (or a steering message already taken) can no
  longer be changed (`-32602`). Steering messages wait in the same queue until taken, so they
  can be edited or removed too; one still waiting when the turn ends runs as the next turn.
- **Subagents:** `session.send` and `session.queue` take an optional `agent_id` (a task id
  such as `t1`) to reach a running subagent; its events carry that agent id. A queued message
  runs as a further turn of the subagent before it reports, and its report (what
  `await_tasks` returns) is the last turn's reply with every turn's usage. A subagent that
  has reported takes no more messages (`-32602`).
- **Root over subagents:** the root agent has `steer_agent` and `queue_agent`
  (`agent_id`, `text`) beside `spawn_agent` and `await_tasks`, for subagents it started and
  has not collected. Like the other delegation tools they run without a permission prompt and
  show as tool calls in the transcript.
- **Persistence:** queue events are journalled. A resumed session restores the root agent's
  queue from the latest `queue_changed` and delivers it (after reporting a turn the restart cut
  off as failed); ids continue from the highest journalled one. Subagents are not resumed, so
  neither are their queues.

`imperium chat` reads input while a turn runs: a line typed then is queued, `/steer <text>`
steers, and `/queue` lists the queue. The desktop composer stays enabled while the agent works
([07](07-desktop-ui.md#environments-and-sessions-implemented)).

## Persistence

**Session status (implemented):** every session has an explicit status, sent as
`status_changed` events and in `session.list`: `idle`, `working` (a turn is running),
`needs_attention` (a permission request is waiting, or the last turn failed), `paused`
(waiting for a usage limit to reset), `watching` (idle while following a pull request's checks,
with the pull request as detail), and `merged` (that pull request merged). Auto-fix sessions
watch their pull request after pushing; any session can be set to watch one with
`session.control` (`watch`, `unwatch`), and the daemon's watcher marks it merged or ends the
watch when the pull request closes.

**Usage limits (implemented):** when a model call is refused for a usage or rate limit (HTTP
429, 503, or 529), the turn waits instead of failing, following the session's resume policy:
`wait_for_reset` (the provider's `Retry-After` or rate-limit reset header, else retry every
interval; the default), `at_time` (wait for a local time such as `03:00`, then retry every
interval), `retry_every` (every N minutes), or `stop` (fail the turn at once), giving up after
`max_wait_hours`. While waiting the session is `paused` and reports `paused` (with the resume
time) and `resumed` events. The default comes from `[resume]`; each session can override it,
and be resumed early, with `session.control` (`set_resume_policy`, `resume_now`) — the
desktop's session header has these as *Usage limits* and *Resume now*. A session's override is
not yet kept across daemon restarts.

Every event is appended to the session journal, `<data>/sessions/<session-id>.jsonl` (one JSON
object per line with an `at_ms` timestamp; implemented). A running session also keeps its events in memory
so clients that attach later replay them. Sessions continue after a daemon restart
(implemented): each records `<id>.meta.json` (project and model) and, after every root turn,
`<id>.messages.json` (the root's conversation); on start the daemon re-opens sessions whose
journal changed within `agents.resume_days`, replays the journal so clients see the transcript,
restores the conversation under the current instructions, and reports a turn the restart cut
off as failed. Subagents that were running are not resumed, and project-agent sessions start
afresh. Every client is told about new sessions (`session.created`), whoever started them.
Planned: transcripts, task trees, and artefacts browsable and exportable.
