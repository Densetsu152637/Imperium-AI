# Plan: reactive tool loading and the knowledge-source rule

Revision: 2026-11-25, revision 1 (first draft, not yet reviewed by a plan critic); written against
`experimental` at `a85f1310`. Roadmap [item 61](../12-roadmap.md#next-in-order). Status: planned.

Two changes to the agent loop, both measured in the guided-harness pilot (private
Imperium-Research, `docs/human-reasoning-models/experiments/guided-harness-pilot.md`; code and
per-item records in Imperium-Models `experiments/guided-harness-pilot/`):

1. **Reactive tool loading.** Today every agent call carries every tool schema the agent may use.
   For a local Qwen3.5-4B root in a headless run that is 21 tools and a floor of about 4,500
   prompt tokens on every call (mean 21,926 prompt tokens per dev item over 3.62 calls, 15,357 of
   them cached). A variant that kept only `read_file`, `grep`, `list_dir` and `run_command` (and
   shortened the prompt) cut the per-call floor to about 1,400 tokens and the prompt tokens per
   item by 70 % (dev) and 42 % (held-out hard suite), with no accuracy loss from the tool cut. This
   plan offers a small core set and loads the rest on demand, so the agent keeps every capability.
2. **Knowledge-source rule.** The stock loop answered "Unknown" on 10 of 15 real-world questions
   when the workspace had no such facts: it grepped the project for the author of *Pride and
   Prejudice*, found nothing and abstained. One task-prompt sentence ("real-world facts,
   arithmetic and logic may come from your own knowledge; only facts about the workspace must
   come from the files") gave the best held-out result (70/72 against 65/72 stock). This plan puts
   a general form of that sentence in the base prompts.

Each stage below is one pull request against `experimental`, with the docs it changes.

## Goals

Each goal is boolean and has a check. "Scripted endpoint" is the fake OpenAI-compatible server of
`crates/imperium-cli/tests/headless_eval.rs` (`endpoint`, `endpoint_with`), which answers each
request from a script and records every request body.

1. **The base prompts carry the knowledge-source rule.** The root and subagent prompts each
   contain the rule sentence exactly once, and `emperor-root.md` carries its counterpart. Check:
   engine and protocol prompt tests ([stage 1](#stage-1-knowledge-source-rule)).
2. **Fewer schemas on the first call.** With reactive loading on, the root's first request offers
   only the core set and `tool_search`, and its `tools` JSON is at most 40 % of the bytes of the
   same request with loading off. Check: the eval-facing test of
   [stage 4](#stage-4-evaluation-surface-and-the-eval-facing-test).
3. **Loading works.** A tool loaded with `tool_search` is offered from the next model call of the
   same turn, runs, and stays offered for the rest of the session, across compaction and a daemon
   restart. Check: stage 2 and 3 engine tests; stage 4 scripted-endpoint test.
4. **The tool list only grows at its end.** Between two calls of a session with loading on, the
   earlier call's tool list is a prefix of the later one's, except at a model switch or a
   permission change. Check: stage 2 prefix test; stage 4 test comparing consecutive request
   bodies.
5. **Loading never widens permissions.** No tool reaches an agent through `tool_search` or an
   implicit load that the same agent could not be offered with loading off (permission profile,
   shell lock, MCP and plugin allowlists, named-agent lists, launcher restrictions, harness
   gating, variant drops). Check: stage 2 and 3 permission tests.
6. **Off is byte-identical.** With loading off (the default for hosted models), the tools sent
   are byte-identical to today's, in today's order. Check: stage 2 snapshot test against the
   tool list built by the code before this plan.
7. **Evaluations can see and control it.** `--print-prompt` shows the deferred tools; the
   result JSON reports the mode, the core and deferred names, what was loaded on which call, the
   tool bytes per root call and a catalog hash; a variant can drop, rename and redescribe
   deferred tools, set the core list, and turn loading on or off. Check: stage 4 tests.
8. **Measured on the pilot suites before the default holds.** A recorded run of the dev and hard
   suites through `imperium run --headless --local` shows loading-on accuracy within two items
   of loading-off on each suite and fewer prompt tokens per item. Check: the
   [stage 5](#stage-5-measurement-and-rollout) report (manual, not CI; it gates keeping the
   default).

## Current state (found on `experimental` at `a85f1310`)

- **Tool list.** `Agent::tool_specs` (`crates/imperium-engine/src/agent/tools.rs`) builds the
  whole list on every turn: the nine `imperium_tools::specs()` file and command tools (shell
  tools removed when the profile locks the shell), MCP providers' tools, activated plugins'
  tools, `update_plan`, `load_skill` and `activate_plugin` (when there is something to load),
  `context_load` (when a workflow context index exists), `start_session`, the builtin providers
  (web, browser, media, the daemon's `search_sessions`, `list_models` and the rest), memory
  tools, the delegation family, `keep_as_knowledge`, `delegate_chunks`, `ask_user`, the harness
  tools `request_clarification` and `update_state`, and the knowledge-agent questions. A named
  agent's file then narrows the list (`narrow_tools`). The turn loop (`agent/turn.rs`) builds it
  once per turn and again only after an `activate_plugin` call (`activates`), and keeps it in
  `last_tools`, which a knowledge agent's frozen request (`Frozen`) copies.
- **Nothing checks that a called tool was offered.** `run_tool_once` refuses by fork, launcher,
  named-agent list and permission profile, then dispatches by name; a known tool the model calls
  without having been offered it runs.
- **Plugins are the only lazy tools.** `activate_plugin` (roadmap item 23,
  [06](../06-extensibility.md#lazy-skills-and-plugins-implemented)) starts a plugin's servers
  and its tools join the list from the next call, inserted **in the middle** of the list (after
  the MCP tools, before `update_plan`), so every tool after them changes position. Activations
  are journalled as `extension_loaded` events and replayed on resume (`replay_extensions` in
  `session.rs`).
- **Where tools sit in a prompt.** OpenAI-compatible endpoints and llama.cpp's `--jinja` chat
  templates render the tool list inside the system turn, before every message (Qwen's template
  writes the system content, then a `# Tools` block, then the conversation). Hosted APIs cache
  tools before the system prompt and messages. So **any change to the tool list invalidates the
  cached conversation after it**: appending keeps the system prompt and the earlier tools cached,
  and costs one prefill of the conversation so far.
- **Loop protocol.** There is no finish tool: a turn ends when the model replies without tool
  calls. `update_plan` is optional (a headless run records the plan if there is one). The loop
  itself needs `context_load` under `required-before-action` (`missing_context` refuses writes
  and commands until required references are loaded), and the root prompt's harness paragraph
  tells the model to call `request_clarification` and `update_state` when those aids are on. The
  skills and plugin index in the system prompt points at `load_skill` and `activate_plugin`.
- **Harness aids** (`[harness]`, `HarnessMode` `auto`/`on`/`off`, `auto` = the agent's model is
  local) are root-only and need an attended session (`Agent::harness_on`). In a headless run
  `--local` turns `auto` into `on` (`Prepared::new` in `crates/imperium-core/src/eval.rs`) and
  `--flag harness=` sets all four aids.
- **Evaluations.** The `Recorder` (`crates/imperium-core/src/eval/record.rs`) wraps the root's
  model client: it applies the variant's prompt edits and its tool edits (`edit_tools`: drop,
  rename, description) to each request **after** the engine built it, renames past calls in the
  history, maps renamed calls back, and records the first and last request and the ledger
  (`EvalLedger`: calls, prompt, completion and cached tokens, cost). `prompt_hash` is SHA-256
  over the first root request's system prompt template and tools (`variant.rs`);
  `session.prompt_preview` (`--print-prompt`) returns that first request. A dropped tool is only
  hidden: the engine still runs it if called. Subagent recordings are keyed by `task_id` (`t1`,
  `t2`, counted per spawn), brief and role, never by tool names.
- **Prompts.** `root_prompt` (`agent/prompts.rs`) says: "For facts that can change, search with
  the tools you have (web_search, search_sessions, memory_search, list_models) instead of
  relying on what you learned in training." `subagent_prompt` says: "Look up facts that can
  change rather than relying on what you learned in training." Both carry `PROVERB` ("... say
  when you don't know, and never invent facts ..."). Nothing says that settled knowledge may be
  used, which is the gap the pilot found. A test (`the_root_prompt_mentions_knowledge_agents_only_while_they_are_on`)
  asserts that the root prompt without knowledge agents has no other "knowledge" in it, and that
  the prompt with knowledge agents starts with the one without them.
- **No roadmap item** covered either change before this plan; item 23 (lazy skills and plugins)
  is the precedent, item 58 (hot-swappable harnesses) is the next harness work, and the planned
  task-scoped context paragraph is related context-saving work.

## Decisions

### R1. Load by a search tool, add the schemas to the tool list, append only

`tool_search` is a core tool. Its description lists the names of the deferred tools (the
catalog); a call loads named tools or returns one-line descriptions. Loaded tools are appended to
the end of the tool list from the next model call, in load order, and stay for the session.

Alternatives considered against the same requirements (fewer schema tokens, every capability
kept, cache-friendly, reliable for a 4B model, permissions unchanged):

- **Keep sending everything** (today). Simple, cache-stable, but pays the full schema cost on
  every call and leaves less of a 16K window for the work. Rejected; remains the `off` mode.
- **A static smaller set** (the pilot variant). Lowest cost, but drops capabilities a coding
  session needs. Rejected for the product; remains possible as a variant (`tools.core` and
  `tools.drop`).
- **A proxy tool** (`call_tool(name, arguments)`, schemas only in tool results). The tool list
  never changes, so the cache is never broken. Rejected for now: the model writes nested JSON
  arguments without a declared schema, llama.cpp's tool grammar and hosted validation cannot
  check them, permission summaries, hooks and the desktop's tool cards would all have to unwrap
  it, and small models are worse at it. Revisit if stage 5 shows the re-prefill after a load costs
  more than the schemas saved.
- **Per-message relevance selection** (a classifier or embeddings choose tools each turn).
  Rejected: the list changes unpredictably, breaking the cache on most turns, it needs another
  model, and a wrong pick still needs a way to load.
- **The catalog in the system prompt** (like the skills index) instead of `tool_search`'s
  description. Equivalent in tokens; rejected so that the system prompt is the same in both modes
  and everything about tools stays in the tools block. Revisit if models ignore the description.

Cache consequence (R1a): every load costs one prefill of the conversation after the tools block
(an estimate for the pilot's items: 2,000 to 6,000 tokens, under a second on the pilot's RTX
5080 for a 4B model; stage 5 measures it). To keep loads rare: a call may load several tools,
related tools load as a bundle (R4), a direct call to a deferred tool loads it too (R5), and
nothing is unloaded within a session. `tool_search`'s description is fixed for the session: it
lists the whole catalog, loaded or not, and is rebuilt only when the catalog itself changes (an
MCP server's tool list changes, a plugin is activated, the permission profile changes), so a load
changes only the tail of the list.

### R2. The setting and where it applies

`harness.tool_loading` = `auto` (default), `on` or `off`, in `[harness]` with the other aids.
`auto` means on when **the agent's own model** runs on this machine (`ResolvedModel.local`), for
the root and for subagents alike, because the cost is per call. Unlike the other aids it does
**not** need an attended session: it changes no message and asks nobody, and headless and
background sessions are where small local models spend most calls. It follows the model at turn
boundaries as the other aids do: an escalation to a hosted model offers the whole list (`off`
behaviour) and a return to a local one offers core plus what was loaded.

Alternatives: on everywhere (rejected for now: hosted providers cache cheaply, a load breaks their
cache too, and the measured evidence is a local 4B model only; `on` is available, and the revisit
condition is a hosted run or a session with many MCP tools showing a token or cost win); a
threshold mode (on when the deferred schemas exceed a share of the window, as Claude Code does
for MCP tools; rejected as a second rule to explain before it is measured, recorded as the first
revisit). An agent whose catalog would hold fewer than three tools gets everything (no
`tool_search`), since loading would save nothing.

### R3. The core set

Core (always offered when the agent may have them, in today's relative order, `tool_search`
last):

- `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`, `run_command`.
- `tool_search`.
- The loop's own protocol tools, when they exist for the agent: `context_load` (a context
  manifest exists; `required-before-action` blocks work until it is called), `load_skill` and
  `activate_plugin` (the system prompt's index points at them), `request_clarification` and
  `update_state` (when their harness aids are on, because the harness paragraph tells the model to
  call them).

Deferred: `command_output`, `command_stop`, `update_plan`, `ask_user`, the delegation family
(`spawn_agent`, `await_tasks`, `steer_agent`, `queue_agent`, `list_agents`, `resume_agent`),
`delegate_chunks`, `keep_as_knowledge`, `list_knowledge_agents`, `ask_agent`, `start_session`, the
memory tools, every builtin provider's tools (web, browser, media, the daemon's tools), every MCP
server's tools, and activated plugins' tools (appended as loaded when the plugin is activated).

Evidence and reasons: the pilot shows the four read and command tools sufficient for
question-answering; coding work also edits and finds files on most turns, so deferring
`write_file`, `edit_file` and `glob` would add a load to nearly every coding session for about
300 tokens saved. Rejected: the pilot's four alone (cheapest, but a load on most coding turns;
available as `tools.core` in a variant); core plus `update_plan` and `ask_user` (the prompt
mentions both, but neither is needed by the loop, and R5 covers a direct call). Revisit the list
from stage 5's record of which tools are loaded most.

### R4. `tool_search` contract

Arguments, both optional: `load` (a list of tool names, or `mcp__<server>__*` for all of one
server's tools) and `query` (words matched case-insensitively against names and descriptions).
With `load` it answers per name: loaded (offered from the next step), already loaded, not allowed
for this agent, or unknown with up to three close names. With `query` it returns up to ten
`name: first sentence of the description` lines (at most 120 characters each); with neither, the
whole catalog that way. Its description: "Tools not offered yet, loaded on request: <names>. Call
with load: [names] to offer them from your next step for the rest of the session, or query: words
to see what they do." When the catalog has more than 40 names, an MCP server's tools are listed as
`mcp__<server>__* (<n> tools)`.

Bundles load together: the delegation family; `ask_agent` with `list_knowledge_agents`;
`memory_search` with `memory_read`; `command_output` with `command_stop`. A `run_command` call
with `background: true` also loads `command_output` and `command_stop`, since its result names
them.

It is an agent tool like `load_skill` (no approval, no hooks: it runs nothing and reads no file),
allowed in plan mode, refused in a knowledge agent's question fork (whose frozen list must not
change), and it never loads a tool outside the agent's catalog.

### R5. A call to a deferred tool loads it and runs

When the model calls a catalog tool that is not loaded (the base prompt names `spawn_agent`,
`update_plan` and others, and some endpoints do not constrain names), the engine loads it (and
its bundle) and runs the call through every usual check, prefixing the result with "(<name> was
not loaded; it is loaded now.)". Rejected: refusing with "load it first" (costs the model a step
and a re-prefill, and breaks root scripts recorded before this plan); running without loading
(the next call would again lack the schema). A name outside the catalog is refused as today.

### R6. Persistence, compaction, subagents, named agents and forks

- **Journal.** A load emits `tools_loaded { names, via }` (`via`: `search`, `implicit`,
  `plugin`), journalled; a resumed session replays it per agent, as `replay_extensions` does for
  skills and plugins, so the list after a restart is the list before it.
- **Compaction** keeps the loaded set (a compaction rewrites the conversation, so the cache is
  rebuilt anyway; unloading tools the summary no longer mentions is a revisit, not this plan).
- **Subagents** start with their own core plus the parent's loaded tools that are in the
  subagent's catalog, so a brief that needs `web_fetch` does not cost the subagent a load.
- **Named agents** (`spawn_agent` with `agent`, and `--agent` roots): a file's `tools` list is
  resolved against the parent's whole catalog (core, loaded and deferred), not only what is
  offered now, and every listed tool is offered from the start (the list is the agent's core;
  `tools: []` still means none). Today `named_agent(def, &parent)` takes the parent's offered
  names, which under loading would silently narrow a named agent to the parent's core.
- **Knowledge agents' question forks** keep the frozen list unchanged (as today).
- **The MCP bridge** (`imperium mcp-serve --tools`, `bridge::tools()`) is a separate list and is
  untouched.

### R7. Evaluations

- **Variant tool edits move into the engine.** Today they are applied to the request after the
  engine built it, which cannot reach deferred tools, `tool_search`'s catalog or its `load`
  arguments. The engine takes the variant's `ToolEdits` through `SessionOptions` and applies
  them to the agent's whole catalog before the core/deferred split: a dropped tool is neither
  offered, nor listed, nor loadable, nor run (a call to it is refused; today it is only hidden),
  a renamed tool is offered, listed and loaded under its new name and mapped back where calls
  are dispatched, and a description edit shows in both the schema and the catalog line. The
  `Recorder` keeps the prompt edits and the recording. Validation warns about a name that is in
  neither the offered list nor the catalog (so the pilot's variant, which drops 17 tools, raises
  no warnings for deferred ones). Rejected: keeping the edits in the `Recorder` and rewriting
  `tool_search` results and arguments there (couples the recorder to the tool's text format).
- **New variant keys:** `tools.core` (names forced into the core; the rest of the catalog is
  deferred) and the flag `tool_loading` (`on`, `off`, `auto`; `auto` follows `--local`, without
  needing `attended=true`). `--flag tool_loading=off` reproduces today's tool list.
- **Result fields** (additive, CLI-only types): `tool_loading { mode, core, deferred, loaded:
  [{ name, call, via }], catalog_hash, tool_bytes_by_call, tool_list_changes }`. `call` is the
  1-based index of the root call that first offered the tool; `tool_bytes_by_call` is the JSON byte size of the
  `tools` sent on each root call (deterministic, no tokenizer); `tool_list_changes` counts root
  calls whose tool list differs from the previous call's (each is a cache rebuild).
- **Hashes.** `prompt_hash` keeps its definition (the first root request's system prompt template
  and tools as sent), so it changes when loading is on; with `--flag tool_loading=off` it differs
  from runs made before this plan only by stage 1's prompt change.
  `catalog_hash` is SHA-256 over the deferred tools' full schemas in catalog order, so two runs
  whose first requests match but whose deferred schemas differ are told apart.
  `--print-prompt` adds `deferred_tools` (the full schemas) and `tool_loading.mode`.
- **Recorded subagent replays stay valid.** Recordings are keyed by `task_id`, brief and role;
  task ids count spawns, not tool calls, so a `tool_search` call shifts nothing. Root scripts that
  call `spawn_agent` directly keep working through R5. A recording that a run never reaches
  because the model did not delegate shows in `replay.unused`, as today.

### R8. Telemetry outside evaluations

The `tools_loaded` event is the record (desktop transcript notice "Loaded tools: ...", CLI
`chat` line). Context breakdowns already count tool definitions per call by source
(`tools_builtin`, `tools_mcp`, `tools_plugins`); loaded tools count under their source and
`tool_search` under `tools_builtin`, so a breakdown shows the saving with no new category.

### K1. The knowledge-source rule: wording and place

Wording, inserted in `root_prompt` right after the "For facts that can change, search ..."
sentence, and in `subagent_prompt` right after "Look up facts that can change rather than relying
on what you learned in training.":

> Settled real-world facts, arithmetic and logic may come from what you already know; facts about
> this project, its files and its environment must come from the tools.

`emperor-root.md` step 3 ("Fill gaps yourself first") gains: "Settled real-world facts,
arithmetic and logic may come from what you already know; facts about the project, its files and
its environment come from the tools." so Emperor is trained on what the engine asks for.

Why this wording: it is the pilot's tested sentence with three changes, each for a reason, and
the three sentences now read as one rule (changeable facts: search; settled facts: own
knowledge; project facts: tools). "Settled" keeps it from contradicting the changeable-facts
sentence (library versions and APIs change and must still be looked up). "What you already know"
replaces "your own knowledge" so the root prompt without knowledge agents still has no
"knowledge" outside the proverb (the existing test keeps meaning something). "The tools"
replaces "the files", because project facts also come from `git`, commands and tool output. It
names no evaluation, knowledge base or answer format, and holds for coding work (what a status
code means, a complexity bound, a date calculation).

Where: the base prompts, for every model, root and subagents. Rejected: a harness aid for local
models (the aids are off in unattended and headless sessions, exactly where the failure was
measured, and the rule is right for hosted models too, which also waste searches on settled
facts); a task-prompt or eval-only note (changes nothing in the product); the pilot's refined
classification policy (its binary "general or fictional" step lost 2 of 9 held-out mixed
questions, abstaining on the Moon landing). Recorded alternative wording K2, for stage 5 only:
K1 plus "so do not search the project for them, and their absence from the files is no reason to
say you don't know." (targets the observed failure directly; untested).

Cost: about 30 tokens in each base prompt. It changes the stock prompt hash.

## Scope and non-goals

In scope: the two decisions above, their settings, protocol event, evaluation surface, tests and
docs, and the measurement that gates the default.

Not in scope (open decisions, recorded only):

- **The headless default permission mode.** `imperium run --headless` defaults to `default`,
  in which nobody answers `run_command`'s approval, so every command is denied (the pilot's
  stock arms computed in the model's head). Whether headless should default to another mode is
  the user's decision; this plan changes nothing there.
- **Harness variants v3 to v5** of the pilot (the classification policy and its successors).
- Unloading tools at compaction; a threshold mode for hosted models; the proxy-tool design.
- Retraining Emperor or Consul on the new tool surface.

## Assumptions

- llama.cpp `--jinja` with Qwen3.5 renders tools in the system turn as described above, so an
  appended tool keeps the system prompt and earlier tools in the slot's cache. Stage 5 checks the
  `cached_tokens` of the call after a load.
- A small model calls a tool listed in another tool's description reliably enough; R5 is the
  fallback when it calls the tool directly instead. Stage 5 measures how often each happens.
- The per-call schema saving is about 3,000 tokens for a headless local root (estimate: the
  pilot's per-call floor fell from about 4,500 to about 1,400 tokens when 17 tools were dropped
  and the prompt shortened; the stage 4 test measures the bytes exactly). An interactive root has
  more tools (daemon, browser, media, MCP), so it saves more.

## Relevant paths

`crates/imperium-engine/src/agent/tools.rs`, `agent/specs.rs`, `agent/turn.rs`,
`agent/prompts.rs`, `agent/delegation.rs`, `agent/compaction.rs`, `agent/fork.rs`, `agent.rs`,
`session.rs`, `crates/imperium-tools/src/invocation.rs`, `crates/imperium-protocol/src/tools.rs`,
`crates/imperium-protocol/src/session.rs`, `crates/imperium-protocol/src/methods/eval.rs`,
`crates/imperium-protocol/src/lib.rs`, `crates/imperium-protocol/schemas/`,
`crates/imperium-config/src/settings.rs`, `crates/imperium-config/src/registry.rs`,
`crates/imperium-core/src/eval.rs`, `crates/imperium-core/src/eval/`,
`crates/imperium-core/src/state/create.rs`, `crates/imperium-cli/src/eval.rs`,
`crates/imperium-cli/src/eval/`, `crates/imperium-cli/src/chat.rs`,
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

Rust is built and tested by CI only (the project builds nothing locally); every acceptance check
below is a CI job unless marked manual. Stage 1 is independent and ships first. Stages 2 to 4 are
in order; stage 5 follows stage 4's release.

## Stage 1: Knowledge-source rule

- Objective: K1 in both base prompts and in `emperor-root.md`.
- Changes: `KNOWLEDGE_SOURCE` constant in `agent/prompts.rs`, used by `root_prompt` (after the
  changeable-facts sentence) and `subagent_prompt` (after its look-up sentence); the step 3 line in
  `crates/imperium-protocol/schemas/prompts/emperor-root.md`.
- Tests: `the_prompts_carry_the_knowledge_source_rule` (both prompts contain it once, it contains
  no "knowledge", the knowledge-agents test still passes unchanged); the protocol test
  `prompts_are_present_and_consistent_with_the_tools` pins "Settled real-world facts, arithmetic
  and logic" in `emperor-root.md`; the headless `print_prompt` test asserts the sentence is in the
  printed prompt.
- Docs: 03 *Response style* bullet list gains the rule; the headless eval docs note that the stock
  prompt hash changed.
- Depends on: none. Size: under 100 lines.
- Owned paths: `agent/prompts.rs`, `agent.rs` (tests), `emperor-root.md`,
  `crates/imperium-protocol/src/tools.rs` (test), `headless_eval.rs`, `docs/03-agent-engine.md`.
- Risks: a hosted model's behaviour changes slightly (it may answer settled facts without a
  search, which is the intent).

## Stage 2: Protocol, setting and the engine's catalog

- Objective: the catalog, the loaded set, `tool_search`, append-only offering and implicit loads
  for the root and plain subagents, behind `harness.tool_loading`.
- Changes:
  - Protocol 0.59 (the next free minor at merge): `tool_search()` `ToolDefinition` in
    `tools.rs` (in `all()`, exported to `schemas/tools/tool_search.json` and `index.json` by the
    export test); `SessionEvent::ToolsLoaded { names: Vec<String>, via: ToolLoadVia }` with
    `ToolLoadVia` `search`, `implicit`, `plugin`; the 02 version paragraph.
  - Config: `HarnessSettings.tool_loading: HarnessMode` (default `Auto`), registry entry
    `harness.tool_loading` ("Load tools on demand"), 10 table row.
  - Engine: split `tool_specs` into `tool_universe()` (today's list with today's filters and
    named-agent narrowing, unchanged) and `offered_tools()` (the universe when loading is off;
    core in universe order, then `tool_search`, then loaded tools in load order when on). A new
    module `agent/catalog.rs` holds the core rule (R3), bundles (R4), the `tool_search`
    implementation and description, and the per-agent loaded list (`Vec<String>`, in `Shared`
    keyed by agent id, as `loaded_skills` is). `turn.rs` rebuilds the offered list after any
    round that loaded tools (generalising `activates`) and before `compact_near_limit` reads
    `tool_texts`. `run_tool_once` gains the R5 check: a catalog tool that is not loaded is loaded
    and then runs; a name outside the universe is refused as now. In reactive mode
    `activate_plugin` appends the plugin's tools as loaded (`via: plugin`) instead of inserting
    them mid-list; in `off` mode it is unchanged.
  - Desktop: `sessions.ts` renders `tools_loaded` as a notice; CLI `chat.rs` prints a line.
- Tests (engine unit and `imperium-engine/tests`): off-mode snapshot (the list equals the list the
  pre-change `tool_specs` built, for the root, a subagent and a shell-locked profile); on-mode
  first list is core plus `tool_search`; `tool_search` loads a tool and its bundle, reports
  already-loaded, unknown (with close names) and not-allowed names, and answers `query`; the
  next call's list has the previous list as its prefix; R5 implicit load runs the call and
  appends; a shell-locked profile has no `run_command` in core or catalog and a load of it is
  refused; a tool hidden by an MCP or plugin allowlist is not in the catalog; `auto` follows the
  agent's model (a switch to a hosted model offers the whole list, back to local offers core plus
  loaded); fewer than three catalog tools means no `tool_search`; plugin activation in reactive
  mode appends at the tail.
- Depends on: none (can be reviewed beside stage 1). Size: about 700 lines; split the protocol
  and config part into its own pull request if it grows past 800.
- Owned paths: `crates/imperium-protocol/**`, `crates/imperium-config/src/{settings,registry}.rs`,
  `crates/imperium-engine/src/agent/{tools,turn,catalog}.rs`, `agent.rs`, `apps/desktop/src/sessions/sessions.ts`,
  `crates/imperium-cli/src/chat.rs`, `docs/02-architecture.md`, `docs/10-configuration.md`.
- Risks: an ordering bug that reorders core tools between turns (the prefix test guards it);
  `max_steps` stops one step earlier on a turn that loads (a load is a model call; documented).

## Stage 3: Persistence, compaction, subagents, named agents and forks

- Objective: R6.
- Changes: `replay_extensions` (or a sibling) replays `tools_loaded` per agent on resume;
  compaction leaves the loaded list alone; `spawn_agent` gives a subagent the parent's loaded
  tools that are in its catalog; `named_agent` resolves a file's list against the parent's
  universe and offers the whole list eagerly; `into_named_root` likewise; forks and the `Frozen`
  request are unchanged; `tool_search` is refused in a fork.
- Tests: a session that loaded `web_fetch`, restarted from its journal, offers it on the first
  call after the restart, in the same position; a compaction keeps the loaded tools; a subagent
  inherits a parent's loaded `web_fetch` but not a tool its profile forbids; a named agent whose
  file lists `update_plan` gets it from the start although the parent never loaded it, and one
  with `tools: []` gets none; a knowledge question fork sends the frozen list byte for byte.
- Depends on: stage 2. Size: about 400 lines.
- Owned paths: `crates/imperium-engine/src/{session.rs,agent/delegation.rs,agent/compaction.rs,agent/fork.rs}`,
  `agent.rs`, `crates/imperium-engine/tests/`.
- Risks: journals written before this plan have no `tools_loaded` (they resume with core only,
  which is correct).

## Stage 4: Evaluation surface and the eval-facing test

- Objective: R7.
- Changes: `SessionOptions` carries the variant's `ToolEdits` and core override into the engine,
  applied in `tool_universe()` with a renamed-name map used in `run_tool_once`; `Recorder` drops
  `edit_tools` and `rename_calls` (prompt edits and recording stay); `variant::validate` and its
  warnings check names against the universe; `EvalVariant.tools.core`; the `tool_loading` loop
  flag in `LoopFlags` and `state/create.rs`; `EvalResult.tool_loading` and `PromptPreview`
  `deferred_tools`; `--print-prompt` prints them.
- Tests (`crates/imperium-cli/tests/headless_eval.rs`, scripted endpoint):
  - `reactive_loading_sends_fewer_schemas_first_and_loads_on_demand`: two runs of the same task
    with `--local`, one with `--flag tool_loading=off`. Script for the on run: call 1 answers
    `tool_search { load: ["update_plan"] }`, call 2 answers an `update_plan` call, call 3 answers
    text. Asserts: request 1 offers exactly core plus `tool_search` and not `update_plan`; the
    JSON bytes of request 1's `tools` are at most 40 % of the off run's request 1; request 2's
    tool list is request 1's plus `update_plan` at the end; the result's `plan` is set (the tool
    ran); `tool_loading.loaded` is `[{ name: "update_plan", call: 2, via: "search" }]`;
    `tool_list_changes` is 1; the two prompt hashes differ and `catalog_hash` is present.
  - `a_direct_call_to_a_deferred_tool_loads_it_and_runs`: call 1 answers `update_plan` with no
    load; the plan is recorded, `via` is `implicit`, request 2 offers it.
  - `a_dropped_tool_cannot_be_loaded_or_run`: a variant dropping `update_plan`; `tool_search`
    reports it unknown and a direct call is refused; no warning is raised for dropping a
    deferred tool.
  - `a_renamed_deferred_tool_is_listed_loaded_and_run_by_its_new_name`.
  - `tools_core_sets_the_core`: `tools.core = ["read_file", "grep", "list_dir", "run_command"]`
    gives exactly those plus `tool_search` on request 1.
  - The existing tests pass unchanged (they run without `--local`, so loading is off).
- Docs: 03 evals (variant keys, flag, result fields, hashes), and a new 03 section *Reactive tool
  loading* (R1 to R6 as built), linked from *Agent loop* step 1 and *Harness aids*; 05 built-in
  tools lists `tool_search`; 06 lazy skills and plugins says how activation and loading combine;
  13 decisions gains D28 (R1, R2) with its revisit conditions.
- Depends on: stage 3. Size: about 600 lines.
- Owned paths: `crates/imperium-core/src/{eval.rs,eval/**,state/create.rs}`,
  `crates/imperium-protocol/src/methods/eval.rs`, `crates/imperium-cli/src/{eval.rs,eval/**}`,
  `crates/imperium-cli/tests/headless_eval.rs`, `crates/imperium-engine/src/{session.rs,agent/tools.rs}`,
  `docs/03-agent-engine.md`, `docs/05-tools-and-isolation.md`, `docs/06-extensibility.md`,
  `docs/13-decisions.md`.
- Risks: moving the edits changes what a dropped tool does when called (refused instead of run);
  stated in the docs and the release notes.

## Stage 5: Measurement and rollout

- Objective: goal 8, and the decision to keep `auto` as the default.
- Work (manual, after stage 4's experimental release; one model at a time, on an idle machine):
  re-run the pilot's `harness` arm on the dev (135) and hard (72) suites with the released
  `imperium.exe` under `--local`, `--flag tool_loading=on` and `=off`, both with K1 (and once with
  K2 on the dev suite). Record accuracy (strict scorer), prompt and cached tokens per item, model
  calls, `tool_list_changes`, which tools were loaded and how (`search` or `implicit`), and the
  cached tokens of the call after each load. Report in Imperium-Research beside the pilot.
- Acceptance: loading on is within two items of loading off on each suite and uses fewer prompt
  tokens per item; K1 leaves at most two of 15 general-knowledge dev items answered "Unknown".
  If loading fails the bar, set the default to `off` in a follow-up and record why in 13; if K1
  fails and K2 or the pilot's exact sentence passes, switch the wording.
- Docs: 12 roadmap item 61 marked implemented with the measured numbers; this plan's status.
- Depends on: stage 4 released.

## Rollout

- Stage 1 changes every model's base prompts once merged; no flag.
- Loading ships as `harness.tool_loading = auto`: on for agents whose model is local, off for
  hosted ones, so hosted sessions see exactly today's tool list. `off` restores today's behaviour
  everywhere; `on` turns it on for hosted models too.
- Each merge into `experimental` publishes an experimental build (see
  [release path](#release-path)); stage 5 runs against the build that contains stage 4.

## Release path

Found on `origin/experimental`:

- `experimental` is the default branch; pull requests target it. `ci.yml` runs on every pull
  request and push to `main` and `experimental`; its `CI gate` job is the required check.
- **Every push to `experimental` (a merged pull request) publishes a release** through
  `.github/workflows/experimental.yml`: `prepare` (`scripts/ci/experimental-prepare.sh`) refuses
  any ref but `experimental` and computes the version `X.Y.<run number>-experimental` from
  `apps/desktop/src-tauri/tauri.conf.json` (now `0.2.0`, so `0.2.<run>-experimental`, for example
  the current `v0.2.230-experimental`); `build` calls `build.yml` with `channel: experimental`;
  `publish` (`scripts/ci/publish-experimental.sh`) writes the updater manifest and
  `SHA256SUMS`, runs `gh release create v<version> --prerelease --latest=false` at the merged
  SHA, and `prune-releases.sh` deletes the previous experimental release and tag. A manual
  `workflow_dispatch` on `experimental` does the same.
- No version bump, tag or changelog is needed for an experimental release: the run number is the
  patch, the tag is made by the workflow, the release notes are a fixed text, and there is no
  `CHANGELOG` file. Installs on the experimental update channel update to it automatically.
- `PROTOCOL_VERSION` is separate: stage 2 bumps the minor (0.59, or the next free one at merge).
- A main release is a separate path (a pull request from `experimental` into `main`, then a
  `vX.Y.Z` tag, `release.yml`), not needed here.
- So root releases by merging each stage's pull request into `experimental` once `CI gate`
  passes, then checking that the `Experimental` run for that push succeeded
  (`gh run list --workflow experimental.yml`) and that `v0.2.<run>-experimental` exists.

## Risks across stages

- **Cache churn.** Each load re-prefills the conversation. Bundles, multi-name loads, R5 and no
  unloading keep loads rare; stage 5 measures the cost; the proxy design is the fallback.
- **Small-model tool discovery.** A 4B model may not think of `tool_search`. R5 catches direct
  calls; the catalog names are in the description it reads every call; stage 5 records misses.
- **Silent narrowing.** Named agents, subagents or variants that resolve tools against the offered
  list instead of the universe would lose tools; stage 3 and 4 tests guard each path.
- **Distribution shift for house models.** Emperor and Consul run locally, so `auto` changes the
  tools they see from their training-time surface (see open questions).
- **Protocol minor collisions.** Concurrent pull requests take the next free minor at merge.

## Open questions

1. **Default scope.** Recommended: `auto` (local models only), as planned. Alternative: on
   everywhere. The user's call, since it changes hosted sessions' prompts.
2. **House models.** Should `auto` apply to Emperor and Consul now, or should they keep the
   whole list until an evaluation through the harness shows no loss for them? Recommended: apply
   it (they are local), and include a house model in stage 5 once one is available.
3. **Core set.** Recommended: the seven file and command tools plus `tool_search` and the loop's
   protocol tools (R3). Alternative: the pilot's four.
4. **Knowledge-source rule everywhere.** Recommended: in the base prompts for every model (K1),
   which changes hosted prompts and every stock prompt hash. Alternative: local models only.
5. **Headless permission mode** (out of scope): should `imperium run --headless` default to a mode
   in which `run_command` works, given that today every command is denied in the stock arms?
