# Plan: reactive tool loading and the knowledge-source rule

Revision: 2026-11-25, revision 4 (plan critic round 1: findings 1 to 10; delta review: N1 to N6;
final review: D1, D2 and notes;
the root's answers to the first open questions recorded as [settled decisions](#settled-decisions));
written against `experimental` at `a85f1310`. Roadmap [item 61](../12-roadmap.md#next-in-order).
Status: stages 1 to 4 implemented; stage 5 (the measurement and the default) is open.

Two changes to the agent loop, both from the guided-harness pilot (private Imperium-Research,
`docs/human-reasoning-models/experiments/guided-harness-pilot.md`; code and per-item records in
Imperium-Models `experiments/guided-harness-pilot/`):

1. **Reactive tool loading.** Today every model call carries every tool schema the agent may use.
   For a local Qwen3.5-4B root in a headless run that is 21 tools, 14,261 bytes of tool JSON
   ([measured](#measured-baseline)), and a floor of about 4,500 prompt tokens on every call (mean
   21,926 prompt tokens per dev item over 3.62 calls, 15,357 of them cached). The pilot's refined
   variant kept only `read_file`, `grep`, `list_dir` and `run_command` **and** cut the system
   prompt to one line; together those cut the per-call floor to about 1,400 tokens and the prompt
   tokens per item by 70 % (dev) and 42 % (held-out hard suite). The variant's parts were not
   isolated, so the saving is not the tool cut's alone. This plan offers a small, fixed core and
   reaches every other tool on demand, so no capability is lost.
2. **Knowledge-source rule.** The stock loop answered "Unknown" on 10 of 15 real-world questions
   when the workspace had no such facts: it grepped the project for the author of *Pride and
   Prejudice*, found nothing and abstained. One task-prompt sentence ("real-world facts,
   arithmetic and logic may come from your own knowledge; only facts about the workspace must
   come from the files") gave the best held-out result (70/72 against 65/72 stock). This plan puts
   a general form of that sentence in the base prompts.

Each stage below is one pull request against `experimental`, with the docs it changes. Every merge
into `experimental` publishes a build, so the mode ships **off** and is turned on only by
[stage 5](#stage-5-measurement-and-the-default).

## Settled decisions

Recorded from the root's answers (2026-11-25):

- **S1.** `harness.tool_loading = auto` means on for agents whose own model runs on this machine
  and off for hosted models; it becomes the default only after stage 5 (until then the default
  is `off`).
- **S2.** `auto` applies to the house models (Emperor, Consul) like any local model.
- **S3.** The knowledge-source rule goes in the base prompts for every model, hosted included.
- **S4.** The headless default permission mode is out of scope.

## Goals

Each goal is boolean and has a check. "Scripted endpoint" is the fake OpenAI-compatible server of
`crates/imperium-cli/tests/headless_eval.rs` (`endpoint`, `endpoint_with`), which answers each
request from a script and records every request body.

1. **The base prompts carry the knowledge-source rule.** The root and subagent prompts each
   contain the rule sentence exactly once, and `emperor-root.md` carries its counterpart. Check:
   engine and protocol prompt tests ([stage 1](#stage-1-knowledge-source-rule)).
2. **Fewer schemas on every call.** With loading on, a headless `--local` root's request offers
   exactly the core set, `tool_search` and `call_tool`, and its `tools` JSON is at most 5,700
   bytes: 40 % of the 14,261 bytes the same request carries with loading off (the 3,997 bytes of
   the seven core tools leave 1,700 bytes for the two loader tools). Re-measured at stage 4 from
   the tool definitions on this branch (the compact JSON of the `tools` array, brackets and commas
   included): 14,375 bytes off after R2.6 (14,283 and 14,261 before it; the 14,261 above is the sum of the 21 tools) and **5,328 bytes
   on**, 37.1 % of it: the seven core tools 4,089 bytes (3,997 before R2.6), `tool_search` 807 bytes with the names
   of the 14 deferred built-in tools in its description, `call_tool` 422 bytes. The bound stays
   at 5,700 bytes, which leaves 372 bytes of headroom. Check: the eval-facing test
   of [stage 4](#stage-4-evaluation-surface-and-the-eval-facing-test).
3. **Every tool stays reachable.** A tool outside the core is found with `tool_search` (which
   returns its schema) and run with `call_tool`, in the same turn, with the same result as a
   direct call. Check: stage 2 engine tests; stage 4 scripted-endpoint test.
4. **The tool list does not change within a session.** With loading on, every model call of an
   agent sends a byte-identical `tools` array, through plugin activations, MCP servers connecting
   or changing their tools, compaction, initiative reviews and daemon restarts. The only
   exceptions are (a) a model switch that changes the mode (`auto` with a local and a hosted
   model) and (b) a permission change that removes a core tool (the shell lock removing
   `run_command`); each has a test. Check: stage 2 and 3 tests comparing consecutive requests;
   stage 4 asserts every request body of a run carries the same `tools`.
5. **Loading never widens permissions.** `call_tool` and `tool_search` reach only the agent's
   universe (defined in [R4](#r4-the-universe-and-dispatch-order)), and every inner call passes the
   same refusals, hooks and approvals as a direct call, in the same order. A variant's dropped
   tool cannot be found, run or called directly, and each such refusal is reported in the result
   JSON's warnings; a renamed tool cannot be used to bypass a refusal keyed on the engine's name.
   A `tool.before` hook sees, rewrites and blocks the inner tool of a `call_tool` exactly as it
   would a direct call. On mode refuses calls that off mode runs today because the engine does not
   check what was offered (listed in R4); off mode dispatches exactly as today. Check: stage 2, 3
   and 4 permission tests, including a renamed `run_command` under a shell-locked profile, hook
   rewrite and block through `call_tool`, and the off-mode dispatch regression test.
6. **Off is byte-identical.** With loading off, the tools sent are byte-identical to today's, in
   today's order. Check: stage 2 snapshot test against the list the pre-change code built.
7. **Evaluations can see and control it.** `--print-prompt` shows the deferred tools; the result
   JSON reports the mode, the core and deferred names, which tools were searched and called on
   which call, tool bytes per root call, and a catalog hash; a variant can drop, rename and
   redescribe deferred tools, set the core, and turn loading on or off. Check: stage 4 tests.
8. **Measured before the default changes.** A recorded run of the pilot's dev and hard suites
   through `imperium run --headless --local` shows loading-on accuracy within two items of
   loading-off on each suite and fewer prompt tokens per item; only then does the default become
   `auto`. Check: the [stage 5](#stage-5-measurement-and-the-default) report (manual).

## Current state (found on `experimental` at `a85f1310`)

- **Tool list.** `Agent::tool_specs` (`crates/imperium-engine/src/agent/tools.rs`) builds the
  whole list each turn: the nine `imperium_tools::specs()` file and command tools (shell tools
  removed when the profile locks the shell), MCP providers' tools, activated plugins' tools,
  `update_plan`, `load_skill` and `activate_plugin` (when there is something to load),
  `context_load` (when a context index exists), `start_session`, the builtin providers (web,
  browser, media, the daemon's `search_sessions`, `list_models` and the rest), memory tools, the
  delegation family, `keep_as_knowledge`, `delegate_chunks`, `ask_user`, the harness tools
  `request_clarification` and `update_state`, and the knowledge-agent questions. A named agent's
  file narrows it (`narrow_tools`). Breakdown groups are contiguous ranges (`ToolsMcp`,
  `ToolsPlugins`). The turn loop builds the list once per turn and again after an
  `activate_plugin` round, keeps it in `last_tools`, and a knowledge agent's `Frozen` request
  copies it.
- **Nothing checks that a called tool was offered.** `run_tool_once` refuses by fork
  (`fork::refusal`, `FORK_TOOLS`), launcher (`SessionLauncher::restricted`), named-agent list and
  permission profile, then dispatches by name. Several arms are gated by conditions other than
  being offered: `ask_agent` runs whenever knowledge agents are on, even where `can_ask` is false
  and it was not offered; a known provider or built-in tool runs whatever was offered.
- **Plugins are the only lazy tools.** `activate_plugin` (item 23,
  [06](../06-extensibility.md#lazy-skills-and-plugins-implemented)) starts a plugin's servers and
  its tools join the list **in the middle** (after the MCP tools, before `update_plan`).
  Activations are journalled as `extension_loaded` and replayed on resume (`replay_extensions`).
- **Initiative reviews** (`crates/imperium-core/src/initiative.rs`): for the review turn the
  launcher refuses every tool outside `REVIEW_TOOLS` (`review_restriction`, about line 1533),
  which includes the launcher's own `suggest_work` and `work_digest` and, in `act` mode,
  `start_session`.
- **Harness aids** (`[harness]`, `HarnessMode` `auto`/`on`/`off`, `auto` = the agent's model is
  local) are root-only and need an attended session (`Agent::harness_on`). In a headless run the
  eval model is built with `local: false` (`Prepared::new` in `crates/imperium-core/src/eval.rs`),
  and `--local` instead turns the harness flag's `auto` into `on` before the session starts.
- **Evaluations.** The `Recorder` (`crates/imperium-core/src/eval/record.rs`) wraps the root's
  model client: it applies the variant's prompt edits and tool edits (`edit_tools`: drop, rename,
  description) to each request after the engine built it, renames past calls in the history
  (`rename_calls`), maps renamed calls in replies back, and records the first and last request
  and the ledger. A dropped tool is only hidden: the engine runs it if called. `prompt_hash` is
  SHA-256 over the first root request's system prompt template and tools;
  `session.prompt_preview` (`--print-prompt`) returns that request. Subagent recordings are keyed
  by `task_id` (`t1`, `t2`, counted per spawn), brief and role, never by tool names. Context
  breakdowns count the tools as the engine built them, before the variant's edits.
- **Prompts.** `root_prompt` (`agent/prompts.rs`) says: "For facts that can change, search with
  the tools you have (web_search, search_sessions, memory_search, list_models) instead of
  relying on what you learned in training." `subagent_prompt`: "Look up facts that can change
  rather than relying on what you learned in training." Both carry `PROVERB` ("... say when you
  don't know, and never invent facts ..."). Nothing says settled knowledge may be used. A test
  (`the_root_prompt_mentions_knowledge_agents_only_while_they_are_on`) asserts that the root
  prompt without knowledge agents has no "knowledge" outside the proverb, and that the prompt
  with them starts with the prompt without them.
- **No roadmap item** covered either change; item 23 is the precedent, item 58 (hot-swappable
  harnesses) is the next harness work.

### Where tools sit in a prompt

- **Qwen3.5** (`F:/AI-Models/Qwen/Qwen3.5-4B/chat_template.jinja`): with tools, the system turn is
  `# Tools ... <tools>`, one JSON line per tool, `</tools>`, then a fixed calling-format trailer,
  then **the system prompt**, then the conversation. A change anywhere in the tool list therefore
  re-prefills everything after the change: the later tools, the trailer, the whole system prompt
  and the whole conversation.
- **Hosted APIs** (Anthropic, OpenAI) put tools first in the cached prefix too: a tool-list change
  invalidates the system prompt and messages.
- **llama.cpp's tool-call grammar** (the pinned llama.cpp in the emperor repository,
  `common/parsers/qwen3-coder.cpp`, chosen for Qwen3.5 by `common/chat.cpp`): with tools and
  `tool_choice` auto, the server builds a **lazy grammar** from the request's `tools`, triggered
  by `<tool_call>`, whose function name is a choice of the declared names' literals and whose
  parameters follow each declared schema. A tool that is not in the request's list cannot be
  called once the grammar triggers.

### Measured baseline

`imperium.exe run --headless --json --print-prompt --local --thinking off` with the installed
experimental build (0.2.0, prompt hash `sha256:b5d9ed5b...0d08`, the pilot's stock hash): 21
tools, 14,261 bytes of compact tool JSON; system prompt 2,360 characters. Per tool (bytes):
`spawn_agent` 3,018, `update_plan` 1,170, `keep_as_knowledge` 1,026, `grep` 1,002, `glob` 795,
`delegate_chunks` 792, `resume_agent` 771, `ask_user` 732, `run_command` 578, `edit_file` 474,
`read_file` 469, `steer_agent` 414, `web_fetch` 405, `queue_agent` 399, `command_output` 365,
`write_file` 359, `list_agents` 331, `web_search` 329, `list_dir` 320, `await_tasks` 299,
`command_stop` 213. The seven core tools of [R3](#r3-the-core-set) are 3,997 bytes (28 %); the
pilot's four are 2,369 (17 %). An interactive root has more (daemon, browser, media, MCP tools),
so it saves more.

## Decisions

### R1. A fixed tool list with a search tool and a call proxy

With loading on, an agent's tool list is fixed for the session: the core tools, then
`tool_search`, then `call_tool`. `tool_search` finds tools and returns their full schemas as
tool-result text; `call_tool(name, arguments)` runs any tool of the agent's universe through
exactly the checks a direct call gets ([R4](#r4-the-universe-and-dispatch-order)).

Compared against the same requirements (fewer schema tokens, every capability kept, cache cost,
works with llama.cpp's grammar, reliable for a 4B model, permissions unchanged):

| Design | Cache cost per load (Qwen3.5 and hosted) | Grammar | Argument checking | Verdict |
| --- | --- | --- | --- | --- |
| Append the loaded schema to `tools` (revision 1) | Re-prefill after the changed position: the trailer, the whole system prompt and the whole conversation (hosted: the system prompt and conversation) | Works for loaded tools; a direct call to an unloaded tool cannot be emitted, so revision 1's implicit load was unreachable locally | Per-tool grammar and schema | Rejected |
| `call_tool` proxy, fixed list | None: only the new result tokens, as for any tool result | Works: `call_tool` is declared; its `arguments` is constrained to a JSON object | The grammar guarantees valid JSON only; the tool's own parser accepts or rejects the arguments exactly as for a direct call, and a rejection carries the schema | **Chosen** |
| Send everything (today) | None | Works | Per-tool | Kept as `off` |
| Static smaller set (the pilot variant) | None | Works | Per-tool | Rejected for the product (loses tools); possible as a variant (`tools.core`) |
| Per-message relevance selection | A re-prefill on most turns | Works | Per-tool | Rejected |

Why the proxy: with tools rendered before the system prompt, appending is the most expensive
option (each load repeats the whole conversation's prefill), and the grammar makes direct calls to
unlisted tools impossible, so a call proxy is the only design in which the list is constant and
every tool is callable. What it costs: the inner arguments lose grammar-level schema enforcement
(the tool's own parser judges them as for a direct call, and a rejection carries the schema, while the proxy's own `name`
and `arguments` stay grammar-checked), a searched tool's schema sits in the conversation (where
compaction can remove it, see [R6](#r6-sessions-compaction-subagents-named-agents-forks-and-reviews)),
and the model writes one extra level of nesting. Revisit (switch to append, or offer both) if
stage 5 shows `call_tool` argument errors or misses that cost more accuracy than loading off.

### R2. The setting

`harness.tool_loading` = `auto`, `on` or `off`, in `[harness]`. `auto` means on when the agent's
**own** model runs on this machine (`ResolvedModel.local`), for the root and subagents alike,
because the cost is per call. It needs no attended session: it changes no message and asks
nobody. It follows the model at turn boundaries as the other aids do: a switch to a hosted model
offers the whole list (off), a switch back offers the fixed reactive list again (goal 4's
exception a). An agent whose deferred catalog would hold fewer than three tools gets everything,
since a loader would cost more than it saves. The default is `off` until stage 5, then `auto`
([S1](#settled-decisions)).

In evaluations the eval model is built with `local: false`, so `Prepared::new` translates the new
loop flag `tool_loading` (default `auto`) as it does `harness`: `auto` with `--local` becomes
`on`, otherwise `off`. The engine's own `auto` check never sees the eval model as local.

Alternatives: on everywhere (rejected by S1: hosted caches are cheap, the evidence is a local 4B
model); a threshold mode (on when the deferred schemas exceed a share of the window, as Claude
Code does for MCP tools; first revisit, once hosted sessions with many MCP tools are measured).

### R3. The core set

Core, always offered when the agent may have them, in today's relative order:

- `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `run_command`;
- the loop's own protocol tools when they exist for the agent: `context_load` (a context
  manifest exists; `required-before-action` blocks work until it is called), `load_skill` and
  `activate_plugin` (the system prompt's index points at them), `request_clarification` and
  `update_state` (their harness aids are on and the harness paragraph tells the model to call
  them);
- `tool_search` and `call_tool`, last.

Everything else in the agent's universe is deferred: `command_output`, `command_stop`,
`update_plan`, `ask_user`, the delegation family, `delegate_chunks`, `keep_as_knowledge`,
`list_knowledge_agents`, `ask_agent`, `start_session`, memory tools, every builtin provider's
tools, every MCP server's tools, and activated plugins' tools. A plugin's activation never changes
the tool list in on mode: its tools become findable with `tool_search` and callable with
`call_tool` from the next step (off mode is unchanged).

Reasons: the pilot shows the four read and command tools enough for question answering; coding
work edits and finds files on most turns, so deferring `write_file`, `edit_file` and `glob` (1,628
bytes) would put a search on nearly every coding session. Rejected: the pilot's four (cheapest,
available as `tools.core`); adding `update_plan` and `ask_user` (the loop needs neither: there is
no finish tool, a turn ends on a reply without calls, and a plan is optional). Revisit from stage
5's record of the tools reached through `call_tool` most often.

### R4. The universe and dispatch order

The **universe** of an agent is the list `tool_specs` builds today for that agent with loading
off, after every filter (permission profile and shell lock, MCP and plugin allowlists,
named-agent narrowing, `can_delegate`, `can_ask` and fork state, harness gating), plus activated
plugins' tools, minus a variant's dropped tools (root only). The **offered list** is derived from
it once, at session start, and stays fixed (goal 4); the **universe** is recomputed **at dispatch
time** for each call, so a plugin another agent of the session activated mid-turn is reachable at
once and nothing is refused against a stale universe. Core and deferred tools are both in it.

Every call, whether direct, through `call_tool`, or a direct call to a deferred name (possible on
endpoints that do not constrain names, and in recorded scripts), is dispatched in this order:

1. **Effective call.** `run_tool` (not `run_tool_once`) turns a `call_tool` call into an
   **effective call**: a `ToolCall` with the inner name and arguments and the outer call's `id`.
   Everything after this step, including the hook-rewrite retry in `run_tool` (which today clones
   the call and re-runs `run_tool_once` with the rewritten arguments), works on the effective call,
   so a `tool.before` hook sees, rewrites or blocks the inner tool with inner-shaped arguments, and
   a rewrite stays the same inner tool. The turn loop also uses the effective call: `activates`
   (an `activate_plugin` round) and `instructions::touched_dir(..., name, ...)` key on the inner
   name, so `call_tool(read_file)` loads the directory's instruction files as a direct
   `read_file` does. `call_tool` whose inner name is `call_tool` or `tool_search` is refused
   ("call it directly"). `tool_search` and `call_tool` themselves are not subject to the
   launcher's or a fork's refusal; their inner calls are.
2. Map a variant's renamed name back to the engine's name (root only; [R7](#r7-evaluations)), so
   every later check sees engine names.
3. Fork refusal (`fork::refusal` on the inner name), launcher refusal (`restricted` on the inner
   name), named-agent refusal, permission-profile refusal, in today's order.
4. Universe membership, **on mode only**: a name outside the universe as computed now is refused
   ("<name> is not one of your tools"). In off mode this step is skipped and dispatch is exactly
   today's.
5. Today's dispatch: hooks, approvals, events (with the inner tool's name and arguments), and the
   tool's own argument parsing (`Invocation::parse` in `run_builtin`, each agent tool's `parse`,
   each provider's own handling), so `call_tool` accepts exactly the arguments a direct call
   accepts. There is no separate schema validation. The rule: **any failed result** of a call that
   came through `call_tool`, or of a direct call to a deferred name, gets the tool's schema
   appended, as advice. It is keyed on failure, not on a parse error, because MCP servers and
   providers report argument errors and other failures alike.

The journal and the transcript keep the call as the model made it (the outer `call_tool`), so the
history sent back to the model shows the calling form its grammar allows; `tool_started` and
`tool_finished` carry the effective call, so the desktop's tool cards, approvals and hooks show
the real tool. Code that reads tool calls back out of messages goes through one helper,
`effective_call(&ToolCall)` ([R9](#r9-code-that-reads-calls-back-from-messages)).

On mode refuses calls that off mode runs today, because step 4 checks the universe and today's
dispatch does not check what was offered: `ask_agent` and `list_knowledge_agents` where `can_ask`
is false or no knowledge agent exists yet, harness tools while their aid is off for the current
model (after a switch), a tool hidden by a named agent's list that today's arms would still run,
and a variant's dropped tool. Stage 2 lists each with a test, and an off-mode regression test
shows each still runs when loading is off (for example `ask_agent` with `can_ask` false).

### R5. `tool_search` and `call_tool` contracts

- `tool_search({ query?: string, names?: [string] })`. `names` (exact names, or
  `mcp__<server>__*`) returns those tools' full schemas; `query` matches words against names and
  descriptions and returns up to five full schemas, then up to ten more as `name: first sentence`
  lines; neither returns the catalog as `name: first sentence` lines. A result holds at most
  8,000 bytes of full schemas: schemas past the cap become `name: first sentence` lines in
  `more` ("ask for these by name"). The cap never hides a schema on its own: the first requested
  or best-matching schema is always returned in full whatever its size, and a request by exact
  `names` returns every named schema in full. The result is JSON (`{"tools": [{"name", "description",
  "parameters"}], "more": [...], "unknown": [...]}`) so the evaluation boundary can rewrite names
  ([R7](#r7-evaluations)). An unknown name gets up to three close names. It is read-only, needs no
  approval, and is allowed in plan mode and in a question fork.
- Schemas and descriptions from MCP servers and plugins are outside text, as they are today in
  the tool block. A malicious description can push its tool up a `query` ranking (ranking
  poisoning); that adds no capability, since every result tool is already in the universe and
  every call to it passes R4. Search results are tool results, so the existing untrusted-data
  framing of tool output applies to them.
- Its description is fixed when the session starts: one sentence, then the names of the deferred
  **built-in** tools of the agent's universe at that moment, then "and the tools of connected MCP
  servers and activated plugins: search for them". Tools that appear later (a plugin activated, an
  MCP server connecting or changing its list) are found by `query` or `names`, never by rewriting
  the description.
- `call_tool({ name: string, arguments: object })` runs one tool (R4). Its result is the inner
  tool's result unchanged.
- **`activate_plugin` in on mode.** Whether it is offered is decided once, at session start (it
  is offered when the agent may activate any plugin then), and it stays offered for the whole
  session; once every plugin is active it answers "already active" with the plugin's tools. Today
  it disappears from the list after the last activation, which would change the list.
- **The loader sentence.** In on mode the system prompt carries one sentence: "Tools that are not
  in your tool list are used in two steps: tool_search finds them and shows their parameters, and
  call_tool runs one by name with those arguments." It is its own swappable region of the system
  prompt (a `loader_text` beside `harness_text`, placed with the same `swap_harness_text` and
  `shift_spans` mechanics, after the base part), independent of attendance and of
  `can_delegate`, so unattended roots and subagents get it too; it is swapped when the mode
  changes with the model. The base prompt's mentions of `spawn_agent`, `update_plan` and the rest
  stay; they now name tools reached through `call_tool`.

### R6. Sessions, compaction, subagents, named agents, forks and reviews

- **Loaded set.** The engine keeps, per agent, the names whose schemas the model has seen in a
  `tool_search` result or has called through `call_tool`. It drives nothing but compaction and
  telemetry: `call_tool` does not require a prior search (the tool's own parsing, R4 step 5, catches
  a guess). Each addition emits `tools_loaded { names, via }` (`via`: `search` or `call`),
  journalled.
- **Resume, and a switch back to local.** The loaded set is rebuilt from the journal's
  `tools_loaded` events, every tool called (directly or through `call_tool`) in the restored
  history, and activated plugins' tools once their plugin is active again. A replayed name that
  no longer exists, is no longer in the universe, or belongs to a plugin not (yet) re-activated is
  dropped silently; it comes back if it is searched again or its plugin is activated. The tool
  list itself needs no restoring: it is fixed (goal 4).
- **Compaction** removes old `tool_search` results with the rest of the summarised turns. Schema
  text can come from MCP servers and plugins, so it is never put back at user-message trust (not
  in the state block, and not as descriptions in the summary, which `compact.rs` inserts as a
  user-role message). After a compaction the engine appends to the summary a list of the loaded
  tools still in the universe: MCP and plugin tools by **name only**, built-in tools by name with
  their description cut to its first sentence (at most 120 characters; built-in text is
  Imperium's own), inside a delimited `<loaded-tools>` block
  marked as reference data ("tools you used before; search again for their parameters"), at most
  1,500 bytes, most recently used first. The model gets full schemas again only through a new
  `tool_search`, whose result has tool-result trust.
- **Subagents** get the fixed reactive list of their own universe; their loaded set starts with
  the parent's loaded tools that are in their universe, which only affects what a later
  compaction lists.
- **Named agents** (`spawn_agent` with `agent`, `--agent` roots): a file's `tools` list is
  resolved against the parent's universe, not its offered list (today `named_agent(def, &parent)`
  takes the offered names, which would narrow a named agent to the parent's core), and every
  listed tool is offered directly from the start, with no loader: the list is that agent's whole
  universe. `tools: []` still means none.
- **Question forks** keep the frozen list byte for byte (it already holds `tool_search` and
  `call_tool` when the source agent was reactive). `tool_search` runs in a fork (read-only);
  `call_tool`'s inner name goes through `fork::refusal`, so a fork reaches the same tools it may
  call directly today and nothing else.
- **Initiative reviews.** `tool_search` and `call_tool` are exempt from `restricted` at the outer
  level; the inner name is checked by `restricted` (R4 step 3), so during a review
  `call_tool(suggest_work, ...)` runs and `call_tool(write_file, ...)` is refused with the
  review's message. The list does not change for the review. `REVIEW_TOOLS` is unchanged.
- **The MCP bridge** (`imperium mcp-serve --tools`, `bridge::tools()`) is a separate list and is
  untouched.

### R7. Evaluations

- **Variant tool edits move to the engine's model boundary.** Today they are applied in the
  `Recorder` to the finished request, which cannot reach `tool_search` results, `call_tool`'s
  inner names or deferred tools. The engine takes the variant's `ToolEdits` through
  `SessionOptions` and applies them to the **root** agent only (as today; subagents are
  untouched): drops remove tools from the universe (R4); renames and descriptions are applied
  where the engine builds a request (tool specs, `tool_search` result payloads, past calls and
  `call_tool` inner names in the history sent) and reversed where a completion arrives (tool call
  names and `call_tool` inner names), before anything is stored, so the engine's messages, the
  journal and events keep engine names. `run_tool_once` also maps an edited name back as its very
  first step (R4 step 2), so no edited name reaches a check keyed on engine names (`SHELL_TOOLS`,
  `FORK_TOOLS`, the named list). The `Recorder` keeps the prompt edits, the recording and the
  ledger. Name validation (warnings) checks the universe, so the pilot's variant, which drops 17
  tools, raises no warnings for deferred ones.
- **Behaviour change.** A dropped tool is refused when called (today it is hidden but runs). The
  result JSON lists each refusal in `warnings` ("dropped tool <name> was called and refused"),
  and goal 5's checks cover it.
- **Accounting change.** Context breakdowns and token estimates now count the tools as sent after
  the variant's edits, not as the engine built them before; variants that redescribe tools show
  their real size.
- **New variant keys:** `tools.core` (names forced into the core; the rest of the universe is
  deferred) and the flag `tool_loading` (`on`, `off`, `auto`; R2).
- **Result fields** (additive; CLI-only types): `tool_loading { mode, core, deferred, searched:
  [{ names, call }], called: [{ name, call }], catalog_hash, tool_bytes_by_call }`, where `call` is
  the 1-based root call index and `tool_bytes_by_call` the compact JSON size of the `tools` sent on
  each root call. `--print-prompt` adds `deferred_tools` (full schemas, as `tool_search` would
  return them) and `tool_loading.mode`.
- **Hashes.** `prompt_hash` keeps its definition (system prompt template and tools of the first
  root request), so it changes when loading is on; with `--flag tool_loading=off` it differs from
  runs made before this plan only by stage 1's prompt change. `catalog_hash` is SHA-256 over the
  deferred tools' full schemas in catalog order.
- **Recorded subagent replays stay valid.** They are keyed by `task_id`, brief and role, and task
  ids count spawns, not tool calls. Root scripts that call `spawn_agent` directly still work (R4
  treats a direct call to a deferred name like `call_tool`). An unreached recording shows in
  `replay.unused`, as today.

### R8. Telemetry and breakdowns

- `tools_loaded` is the record outside evaluations (desktop transcript notice "Looked up tools:
  ...", CLI `chat` line).
- Breakdown groups change from contiguous ranges to one category per offered tool (a
  `Vec<Category>` beside the specs, kept in step by `narrow_tools`), so no category depends on
  where tools sit. In on mode `tool_search` and `call_tool` count as `tools_builtin`; the schemas
  inside a `tool_search` result are labelled per tool with the tool's source category
  (`tools_builtin`, `tools_mcp`, `tools_plugins`) as spans of that message, the way `load_skill`
  results count as `skills_loaded`; the names-only `<loaded-tools>` list after a compaction counts
  under `summary`. No new category key.

### R9. Code that reads calls back from messages

Several places read tool calls back out of the conversation by name. With `call_tool` the stored
call is the outer one, so each of them goes through one helper, `effective_call(&ToolCall) ->
Cow<ToolCall>` (the inner name and arguments of a `call_tool`, otherwise the call itself), rather
than storing the inner call in messages (which would show the model a direct call its grammar
forbids). Audited on `a85f1310`:

| Place | What it keys on | With the helper |
| --- | --- | --- |
| `compact.rs` (about line 219) | Transcript lines `[called <name> with <args>]` for the summariser | The inner name and arguments |
| `compact.rs` (about line 306) | Call ids to tool names, to label tool results when cutting | The inner name |
| `knowledge.rs` (about line 282) | Calls and whether they worked, for a knowledge card | The inner name and arguments |
| `breakdown.rs` (about line 283) | `load_skill` call ids, to count their results as `skills_loaded` | The inner name (`load_skill` is core, but a call through `call_tool` still counts) |
| `harness.rs` (about line 238) | Whether a result came from `update_state` | The inner name |
| `harness.rs` (about line 363) | `[called <name> ...]` lines in the de-reference step's history | The inner name and arguments |
| `turn.rs` (`activates`, `touched_dir`) | `activate_plugin` rounds and directories touched | The effective call (R4 step 1) |
| Recorder and transcript export | Eval `last_reply`, exported transcripts | Unchanged: they show what the model sent |

Stage 2 adds a test per row that the `call_tool` form gives the same result as the direct form,
and a source-scan test that no other `function.name` comparison remains in the engine outside the
helper's callers.

### K1. The knowledge-source rule: wording and place

Wording, inserted in `root_prompt` right after the "For facts that can change, search ..."
sentence, and in `subagent_prompt` right after "Look up facts that can change rather than relying
on what you learned in training.":

> Settled real-world facts, arithmetic and logic may come from what you already know; facts about
> this project, its files and its environment must come from the tools.

`emperor-root.md` step 3 ("Fill gaps yourself first") gains: "Settled real-world facts,
arithmetic and logic may come from what you already know; facts about the project, its files and
its environment come from the tools." This keeps the vendored prompt in step with the engine; it
does not retrain Emperor, which learns the rule only from a later training round that uses the
updated prompt.

Why this wording: it is the pilot's tested sentence with three changes, and the three sentences
now read as one rule (changeable facts: search; settled facts: own knowledge; project facts:
tools). "Settled" keeps it from contradicting the changeable-facts sentence (library versions and
APIs change and must still be looked up). "What you already know" replaces "your own knowledge" so
the root prompt without knowledge agents still has no "knowledge" outside the proverb (the
existing test keeps its meaning). "The tools" replaces "the files", because project facts also
come from `git`, commands and tool output. It names no evaluation, knowledge base or answer
format, and holds for coding work (what a status code means, a complexity bound, a date
calculation). The changed wording is untested; stage 5 checks it.

Where: the base prompts, every model, root and subagents (S3). Rejected: a harness aid for local
models (the aids are off in unattended and headless sessions, exactly where the failure was
measured); a task-prompt or eval-only note (changes nothing in the product); the pilot's refined
classification policy (its binary "general or fictional" step lost 2 of 9 held-out mixed
questions). Alternative K2, for stage 5 only: K1 plus "so do not search the project for them, and
their absence from the files is no reason to say you don't know."

Cost: about 30 tokens in each base prompt. It changes the stock prompt hash.

## Scope and non-goals

In scope: the decisions above, their settings, protocol event, evaluation surface, tests and docs,
and the measurement that gates the default.

Not in scope (recorded only): the headless default permission mode (S4: in `default` mode nobody
answers `run_command`'s approval, so every command is denied); the pilot's harness variants v3 to
v5; appending schemas to the list (R1's rejected design, the first fallback); a threshold mode for
hosted models; retraining Emperor or Consul.

## Assumptions

- Qwen3.5-4B calls a proxy tool with nested JSON arguments reliably enough. Untested; stage 5
  records `call_tool` argument errors and turns that end without a needed tool.
- A tool result holding a few schemas (about 300 to 3,000 bytes each) is read as well as the same
  schemas in the tool block. Stage 5 compares accuracy.
- The llama.cpp build Imperium downloads behaves like the pinned one read for this plan (lazy
  grammar over declared names). With the chosen design this matters only for R4's direct calls to
  deferred names, a fallback for unconstrained endpoints.

## Relevant paths

`crates/imperium-engine/src/agent/{tools,specs,turn,prompts,delegation,compaction,fork}.rs`,
`crates/imperium-engine/src/agent.rs`, `crates/imperium-engine/src/session.rs`,
`crates/imperium-engine/src/launcher.rs`, `crates/imperium-tools/src/invocation.rs`,
`crates/imperium-protocol/src/{tools,session,lib}.rs`,
`crates/imperium-protocol/src/methods/eval.rs`, `crates/imperium-protocol/schemas/`,
`crates/imperium-config/src/{settings,registry}.rs`, `crates/imperium-core/src/eval.rs`,
`crates/imperium-core/src/eval/`, `crates/imperium-core/src/state/create.rs`,
`crates/imperium-core/src/initiative.rs`, `crates/imperium-cli/src/{eval.rs,eval/,chat.rs}`,
`crates/imperium-cli/tests/headless_eval.rs`, `apps/desktop/src/sessions/sessions.ts`.

## Related docs

[03 agent loop](../03-agent-engine.md#agent-loop),
[03 harness aids](../03-agent-engine.md#harness-aids-for-small-models),
[03 context management](../03-agent-engine.md#context-management),
[03 evals](../03-agent-engine.md#running-evals-through-the-harness-implemented),
[03 response style](../03-agent-engine.md#response-style),
[05 built-in tools](../05-tools-and-isolation.md#built-in-tools),
[06 lazy skills and plugins](../06-extensibility.md#lazy-skills-and-plugins-implemented),
[10 configuration](../10-configuration.md), [02 protocol](../02-architecture.md#protocol),
[11 CI/CD](../11-ci-cd.md), [13 decisions](../13-decisions.md).

## Stages

Rust is built and tested by CI only; every acceptance check is a CI job unless marked manual.
Stage 1 is independent. Stages 2 to 4 are in order and ship with the default `off`, so each
experimental build they publish behaves as today unless a user or an evaluation turns loading on.
Stage 5 follows the build that contains stage 4.

## Stage 1: Knowledge-source rule

- Objective: K1 in both base prompts and in `emperor-root.md`.
- Changes: a `KNOWLEDGE_SOURCE` constant in `agent/prompts.rs`, used by `root_prompt` and
  `subagent_prompt`; the step 3 line in `crates/imperium-protocol/schemas/prompts/emperor-root.md`.
- Tests: `the_prompts_carry_the_knowledge_source_rule` (each prompt contains it once; it contains
  no "knowledge"; the knowledge-agents test passes unchanged); the protocol test
  `prompts_are_present_and_consistent_with_the_tools` pins "Settled real-world facts, arithmetic
  and logic" in `emperor-root.md`; the headless `print_prompt` test asserts the sentence.
- Docs: 03 *Response style* gains the rule; the eval docs note that the stock prompt hash changed.
- Depends on: none. Size: under 100 lines.
- Owned paths: `agent/prompts.rs`, `agent.rs` (tests), `emperor-root.md`,
  `crates/imperium-protocol/src/tools.rs` (test), `headless_eval.rs`, `docs/03-agent-engine.md`.
- Risks: hosted models answer settled facts without a search (the intent).

## Stage 2: Protocol, setting, universe and the proxy

- Objective: R1 to R5 and R8 for the root and plain subagents, behind `harness.tool_loading`
  (default `off`).
- Changes:
  - Protocol 0.59 (next free minor at merge): `tool_search()` and `call_tool()`
    `ToolDefinition`s in `tools.rs` (in `all()`, exported to `schemas/tools/` by the export
    test); `SessionEvent::ToolsLoaded { names, via }` with `ToolLoadVia` `search`, `call`; the 02
    version paragraph.
  - Config: `HarnessSettings.tool_loading: HarnessMode`, default `Off`. It stays **hidden** while
    only 2a is merged: 2a adds the field (readable by evaluations through `--flag tool_loading`)
    but no registry entry, settings UI or 10 row, because without 2b's `effective_call` helper the
    name-keyed code would misread `call_tool` calls (03 says the engine reads it from `[harness]`
    if set). 2a ships without the registry entry; 2b adds the registry entry
    `harness.tool_loading` ("Reach tools on demand") and the 10 table row.
  - Engine: `tool_universe()` (today's list and filters, unchanged, computed at dispatch time)
    and `offered_tools()` (the universe when off; core, `tool_search`, `call_tool` when on, fixed
    at session start); a new `agent/catalog.rs` with the core rule, the fixed `tool_search`
    description, search and its byte cap, the schema advice on a parse failure, and the loaded
    set; the effective call in `run_tool` and the turn loop, and the R4 order in
    `run_tool_once`; `effective_call` and its callers (R9); the loader sentence as its own
    swappable region; breakdown groups as one category per tool; `activate_plugin` offered for
    the whole session in on mode. Desktop `sessions.ts` and CLI `chat.rs` render `tools_loaded`.
- Tests (engine): off-mode snapshot equal to the pre-change list (root, subagent, shell-locked
  profile); the on-mode list is core, `tool_search`, `call_tool`, and byte-identical over calls
  across a plugin activation and an MCP server's list change; `tool_search` by name, by query,
  unknown with close names, and a newly activated plugin's tool found by query; `call_tool` runs a
  deferred tool with the same events and hooks as a direct call; arguments a direct call accepts
  (including what the tolerant parsers coerce) are accepted through `call_tool`, and a parse
  failure returns the tool's own error with the schema appended; a `tool.before` hook rewrites the
  inner tool's arguments through `call_tool` (the rewrite retry runs the same inner tool) and a
  hook that blocks the inner tool blocks the call; `call_tool(read_file)` loads the touched
  directory's instruction files; `call_tool(call_tool)` and `call_tool(tool_search)` are refused; a
  direct call to a deferred name runs as `call_tool` would; each R4 refusal for an inner name
  (profile, named list, outside the universe) and each on-mode-only refusal listed in R4; the
  **off-mode dispatch regression**: with loading off, `ask_agent` with `can_ask` false and the
  other on-mode-only refusals still run as today, and no universe check happens; a plugin
  activated by another agent of the session mid-turn is reachable through `call_tool` on the next
  call (the universe is computed at dispatch); `activate_plugin` stays offered through the last
  activation and answers "already active", with the list byte-identical before and after; a
  shell lock mid-session removes `run_command` from the list (goal 4 exception b) and
  `call_tool(run_command)` is refused; a model switch local to hosted to local (exception a)
  swaps the list and the loader sentence; the loader sentence is in the system prompt of an
  unattended root and of a subagent in on mode; fewer than three deferred tools means no loader;
  a `tool_search` result over 8,000 bytes of schemas spills into `more` lines, while a single
  MCP-style schema larger than the cap is still returned in full (first match, and by exact
  name); a failed result of an MCP-style tool called through `call_tool` carries the schema;
  `tool_search` results keep their tool-result framing when the compaction summariser renders
  them; a compacted session's `<loaded-tools>` block holds no MCP or plugin description text
  (a fixture plugin tool with a marker in its description; the marker is absent); the breakdown
  labels a `tool_search` result's schemas per source category; one test per R9 row that the
  `call_tool` form reads back like the direct form, and the source-scan test of R9.
- Stage 2b, as built: `effective_call` and the R9 rows with their tests and the source scan, the
  `loader_text` region, one breakdown category per tool with `tool_search` schemas labelled by
  source, the `harness.tool_loading` registry entry and 10 row, and schema advice at most once
  per tool and agent. The names-only `<loaded-tools>` block after a compaction is stage 3's
  change (R6, stage 3 "Changes"), so its two tests (no MCP or plugin description in the block;
  the 1,500-byte cap) move to stage 3 with it; stage 2b tests that a `tool_search` result keeps
  its tool-result framing in the summariser's transcript.
- Depends on: none. Size: about 1,000 lines, so it is two pull requests: 2a, the protocol and
  config part plus the engine's universe, offered list, `tool_search`, `call_tool` and the
  dispatch order; 2b, the R9 helper and its callers, the loader sentence and the breakdown.
- Owned paths: `crates/imperium-protocol/**`, `crates/imperium-config/src/{settings,registry}.rs`,
  `crates/imperium-engine/src/agent/{tools,turn,catalog,prompts}.rs`, `agent.rs`,
  `crates/imperium-engine/src/{compact,knowledge,breakdown,harness,instructions}.rs`,
  `apps/desktop/src/sessions/sessions.ts`, `crates/imperium-cli/src/chat.rs`,
  `docs/02-architecture.md`, `docs/10-configuration.md`.
- Risks: a `call_tool` path that skips a check (the R4 tests mirror every direct-call refusal
  test); `max_steps` stops one step earlier on a turn that searches (a search is a model call).

## Stage 3: Sessions, compaction, subagents, named agents, forks and reviews

- Objective: R6.
- Changes: rebuild the loaded set on resume and on a switch back to local (journal events, history
  calls, re-activated plugins; stale names dropped); the names-only `<loaded-tools>` list after
  compaction; subagents' starting loaded set; `named_agent` and `into_named_root` resolve against
  the universe and offer the whole list; forks keep the frozen list with `call_tool`'s inner names
  checked by `fork::refusal`; `tool_search` and `call_tool` exempt from the launcher's outer
  refusal.
- Tests: a resumed session sends the same tool list and its loaded set holds the journal's names
  and the tools called in history; replayed names that no longer exist, are no longer permitted,
  or precede their plugin's re-activation are dropped without an error, and the plugin's names
  return after it is activated again; a compaction appends the names-only `<loaded-tools>` list to the summary within its 1,500-byte cap and puts no schema text in a user message;
  a subagent inherits a parent's loaded `web_fetch` but not a tool its profile forbids; a named
  agent whose file lists `update_plan` is offered it directly and one with `tools: []` gets none;
  a question fork sends the frozen list byte for byte, runs `tool_search`, and is refused
  `call_tool(write_file)`; **an initiative review with a local model**
  (`crates/imperium-core/tests/`, the initiative tests' daemon harness with a local-marked model):
  the review turn's list equals the session's list, `call_tool(suggest_work)` and
  `call_tool(work_digest)` run, and `call_tool(write_file)` is refused with the review's message.
- Depends on: stage 2. Size: about 500 lines.
- Owned paths: `crates/imperium-engine/src/{session.rs,agent/delegation.rs,agent/compaction.rs,agent/fork.rs,agent/prompts.rs}`,
  `crates/imperium-engine/tests/`, `crates/imperium-core/tests/` (the review test).
- Risks: journals written before this plan have no `tools_loaded`; their loaded set comes from
  history calls only (correct).

## Stage 4: Evaluation surface and the eval-facing test

- Objective: R7, and the `tool_loading` translation in `Prepared::new` (R2).
- Changes: `SessionOptions` carries the variant's `ToolEdits` and core override; the engine's root
  model boundary applies and reverses them; `Recorder` loses `edit_tools` and `rename_calls`;
  validation checks names against the universe; `EvalVariant.tools.core`; the `tool_loading` loop
  flag in `LoopFlags`, `Prepared::new` and `state/create.rs`; `EvalResult.tool_loading`, the
  dropped-tool warnings and `PromptPreview.deferred_tools`.
- Tests (`crates/imperium-cli/tests/headless_eval.rs`, scripted endpoint):
  - `reactive_loading_sends_fewer_schemas_and_reaches_tools_on_demand`: two runs of one task with
    `--local`, one with `--flag tool_loading=off`. Script for the on run: call 1 answers
    `tool_search { names: ["update_plan"] }`, call 2 answers `call_tool { name: "update_plan",
    arguments: {...} }`, call 3 answers text. Asserts: request 1 offers exactly the seven core
    tools, `tool_search` and `call_tool`; its `tools` JSON is at most 5,700 bytes; all three
    requests carry byte-identical `tools`; request 2's last message holds `update_plan`'s schema;
    the result's `plan` is set; `tool_loading.searched` is `[{ names: ["update_plan"], call: 1 }]`
    and `called` is `[{ name: "update_plan", call: 2 }]`; the off run's request 1 equals the
    off-mode snapshot (14,261 bytes on the measured build); the two prompt hashes differ;
    `catalog_hash` is set.
  - `a_direct_call_to_a_deferred_tool_runs_like_call_tool`.
  - `a_dropped_tool_cannot_be_found_or_run`: `tool_search` reports it unknown, `call_tool` and a
    direct call are refused, the refusal is in `warnings`, and dropping a deferred tool raises no
    edit warning.
  - `a_renamed_tool_is_searched_and_called_by_its_new_name_and_stored_by_the_engine_name`.
  - `a_renamed_run_command_is_refused_under_a_shell_lock`: a variant renames `run_command` to
    `sh` and the profile locks the shell; `sh` and `call_tool(sh)` are refused.
  - `variant_tool_edits_reach_the_root_only`: a subagent's request carries the unedited tools.
  - `tools_core_sets_the_core`: `tools.core = ["read_file", "grep", "list_dir", "run_command"]`
    gives those plus the two loader tools.
  - The existing tests pass unchanged (default `off`).
- Docs: 03 evals (keys, flag, fields, hashes, the accounting change), a new 03 section *Reactive
  tool loading* (R1 to R6 as built) linked from *Agent loop* step 1 and *Harness aids*; 05
  built-in tools lists `tool_search` and `call_tool`; 06 says how plugin activation and loading
  combine; 13 gains D28 (R1, R2) with its revisit conditions.
- As built (differences from the list above):
  - The engine takes the variant's `ToolEdits` (and a `ToolWatch` the evaluation implements) in
    `SessionOptions`; drops leave the root's universe, `tools.core` replaces the default core, and
    renames and descriptions are applied in `agent/edits.rs` and `tool_edits.rs` where the request
    is built (specs, `tool_search` description and results, past calls, `call_tool` inner names,
    `tool_search` `names`) and reversed where a reply arrives. A `tool_search` `query` is ranked
    over the shown names and descriptions. Renaming or dropping `tool_search` or `call_tool` is
    refused by variant validation; a subagent's named-agent list resolves against the universe
    before the root's drops.
  - `Recorder` keeps the prompt edits, the recording and the ledger, and counts the bytes of each
    root request's `tools`; `ToolWatch` reports the catalog, what a search showed for the first
    time, which deferred tools were called, and each dropped-tool refusal, all with the names the
    model saw. `EvalResult.tool_loading` also has `tool_list_changes`, and `PromptPreview` has
    `deferred_tools` and `tool_loading`.
  - The translation of the `tool_loading` flag in `Prepared::new` and `state/create.rs` landed in
    stage 2; the flag's default stays `off` here (the plan's "default `auto`" waits for stage 5),
    so the loading-on runs of the tests pass `--local --flag tool_loading=auto` or
    `--flag tool_loading=on`.
  - The shell-lock test is an engine test (`reactive_tools.rs`), where the profile can be set; the
    rest are in `headless_eval.rs`.
- Depends on: stage 3. Size: about 650 lines.
- Owned paths: `crates/imperium-core/src/{eval.rs,eval/**,state/create.rs}`,
  `crates/imperium-protocol/src/methods/eval.rs`, `crates/imperium-cli/src/{eval.rs,eval/**}`,
  `crates/imperium-cli/tests/headless_eval.rs`, `crates/imperium-engine/src/{session.rs,agent/turn.rs,agent/tools.rs}`,
  `docs/03-agent-engine.md`, `docs/05-tools-and-isolation.md`, `docs/06-extensibility.md`,
  `docs/13-decisions.md`.
- Risks: the model-boundary translation must cover every place a name appears (the rename tests
  cover specs, history calls, `call_tool` inner names and `tool_search` payloads).

## Stage 5: Measurement and the default

- Objective: goal 8, then the default.
- Work (manual, on the experimental build containing stage 4; one model at a time, on an idle
  machine): run the pilot's `harness` arm on the dev (135) and hard (72) suites with `--local`,
  `--flag tool_loading=on` and `=off`, both with K1, and once with K2 on the dev suite; a house
  model as well once one is available (S2). Record accuracy (strict scorer), prompt and cached
  tokens per item, model calls, the tools searched and called, `call_tool` argument errors, direct
  calls to deferred names, and **misses**: items where the model answered or stopped without a
  tool it needed (judged from the trace, for example an "Unknown" with no search). Also record,
  per session, the bytes of `tool_search` results against the schema bytes saved per call times
  the calls made, to find the break-even point from real traces. Report in Imperium-Research
  beside the pilot.
- Acceptance: loading on within two items of off on each suite with fewer prompt tokens per item;
  K1 leaves at most two of 15 general-knowledge dev items answered "Unknown".
- Then, in one small pull request: the default becomes `auto` (settings default, registry text,
  10, 03, roadmap item 61 marked implemented with the measured numbers, this plan's status). If
  loading fails the bar, the default stays `off` and 13 records why, with the append design or a
  core change as the next step; if K1 fails and K2 or the pilot's exact sentence passes, the
  wording changes instead.
- Depends on: stage 4 released.

## Rollout

- Stage 1 changes every model's base prompts on merge; no flag.
- Stages 2 to 4 ship `harness.tool_loading = off`: each experimental build behaves as today unless
  the setting or `--flag tool_loading` turns it on.
- Stage 5's pull request sets `auto` (on for local models, Emperor and Consul included; off for
  hosted ones). `off` restores today's behaviour; `on` enables it for hosted models.

## Release path

Found on `origin/experimental`:

- `experimental` is the default branch; pull requests target it. `ci.yml` runs on every pull
  request and on pushes to `main` and `experimental`; its `CI gate` job is the required check.
- **Every push to `experimental` (a merged pull request) publishes a release** through
  `.github/workflows/experimental.yml`: `prepare` (`scripts/ci/experimental-prepare.sh`) refuses
  any ref but `experimental` and computes `X.Y.<run number>-experimental` from
  `apps/desktop/src-tauri/tauri.conf.json` (now `0.2.0`, so `0.2.<run>-experimental`; the current
  one is `v0.2.230-experimental`); `build` calls `build.yml` with `channel: experimental`;
  `publish` (`scripts/ci/publish-experimental.sh`) writes the updater manifest and `SHA256SUMS`,
  runs `gh release create v<version> --prerelease --latest=false` at the merged SHA, and
  `prune-releases.sh` deletes the previous experimental release and tag. A manual
  `workflow_dispatch` on `experimental` does the same.
- No version bump, tag or changelog is needed: the run number is the patch, the workflow makes the
  tag, the notes are fixed text, and there is no `CHANGELOG`. Installs on the experimental channel
  update automatically.
- `PROTOCOL_VERSION` is separate: stage 2 bumps the minor (0.59, or the next free one at merge).
- A main release is a separate path (a pull request from `experimental` into `main`, then a
  `vX.Y.Z` tag and `release.yml`), not needed here.
- Root releases by merging each stage's pull request once `CI gate` passes, then checking that the
  `Experimental` run for that push succeeded (`gh run list --workflow experimental.yml`) and that
  `v0.2.<run>-experimental` exists.

## Risks across stages

- **Proxy reliability on a 4B model.** Nested arguments and the two-step pattern are untested;
  stage 5 measures them, and the default stays `off` if they cost accuracy.
- **A bypass through the proxy.** Every direct-call refusal has a `call_tool` twin test; the
  dispatch order is one function.
- **Context growth from searched schemas** (they sit in the conversation). They are counted in the
  breakdown, capped at 8,000 bytes per search, not carried past a compaction, and usually smaller than the schemas they
  replace on every call.
- **Silent narrowing.** Named agents, subagents or variants that resolve against the offered list
  instead of the universe would lose tools; stage 3 and 4 tests guard each path.
- **House models' distribution shift.** Emperor and Consul see the reactive list under `auto` (S2)
  though trained on the full one; stage 5 includes a house model when one is available.
- **Protocol minor collisions.** Concurrent pull requests take the next free minor at merge.

## Open questions

None for the user; the first round's are [settled](#settled-decisions). Stage 5 decides the
default from measurement.
