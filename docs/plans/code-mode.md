# Plan: programmatic tool calling (code mode)

Revision: 2026-11-26, revision 1 (first draft, not yet critiqued); written against `experimental`
at `1c066512`. Roadmap [item 65](../12-roadmap.md#next-in-order); Imperium-Research roadmap R2.8.
Status: planned.

The model gets one more tool, `run_script`, whose argument is a short Python program. Inside the
program every tool the agent may use is a Python function. Each such call goes through the agent's
real dispatch (the same refusals, universe check, hooks, approvals, permission mode and events as a
direct call). Intermediate results stay in the program's variables. Only what the program prints
(bounded) and a one-line summary of the calls it made come back to the model.

Why, from the guided-harness pilot (private Imperium-Research,
`docs/human-reasoning-models/experiments/guided-harness-pilot.md`; code and per-item records in
Imperium-Models `experiments/guided-harness-pilot/`):

- **A local Qwen3.5-4B computes in its head and aggregates by reading rows.** The stock
  `harness` arm (Imperium, commands denied) scored 5/9 on `hard_compute`. `hard_compute-06`
  stopped at 131.6 kg and never converted the units, and `-07` and `-09` hit the 20-step limit
  "with no working way to calculate". The refined variant, which could compute in a shell, reached
  9/9. Filters are slow and fragile: `hard_filter` took 13 to 21 s per item, against 2 to 6 s for
  most other categories, and `hard_filter-06` counted 0 where 3 companies matched, after 15 calls
  and 42 s. `compare_aggregate-14` hit the step limit. The research roadmap names "aggregation
  done by reading rows instead of computing" as one of the two main small-model failures.
- **The shell is not available where the pilot ran.** In `default` permission mode `run_command`
  asks, and a headless run has nobody to answer, so every command was denied: 220 failed calls in
  the first refinement. Computing inside a sandbox with no ambient capability needs no approval:
  arithmetic, filtering and counting in `run_script` run in `default` mode, and only the tool
  calls the program makes are judged.
- **Tokens.** Every row read into the conversation is resent on every later call. A program that
  reads 40 rows and prints one count puts one line in the context. Anthropic reports large
  savings from the same pattern for MCP tools ("code execution with MCP").

Each stage below is one pull request against `experimental`. Every merge into `experimental`
publishes a build, so the mode ships **off** and is turned on only by
[stage 6](#stage-6-measurement-and-the-default).

## Goals

Each goal is boolean and has a check. "Scripted endpoint" is the fake OpenAI-compatible server of
`crates/imperium-cli/tests/headless_eval.rs`. "Fake runner" is the test double of the engine's
script-runner trait ([C3](#c3-where-the-interpreter-runs-and-how-the-engine-drives-it)); "real
worker" is the shipped interpreter binary, which CI builds.

1. **A script reaches tools only through dispatch.** With the real worker, a program can't read a
   file, read an environment variable, open a socket, start a process or load a module except by
   calling a bound tool. Check: stage 1 worker tests (`open()`, `os.environ`, `socket`,
   `subprocess`, `__import__`, `import os` each fail; `print(open("Cargo.toml").read())` raises).
2. **Every inner call is a real call.** An inner call passes the fork, launcher, named-agent,
   permission-profile and universe refusals, `tool.before` and `tool.after` hooks (including
   rewrite and block), the permission rules and safety floor, auto mode's classifier, approval, and
   plan mode, in the same order as a direct call. It emits `tool_started` and `tool_finished`.
   Check: stage 2 tests, one per refusal kind, each the twin of an existing direct-call test.
3. **Only the bounded output enters the context.** The tool result of `run_script` holds at most
   `harness.script_output_chars` characters of printed output, plus a summary line of at most 300
   characters, plus (on failure) a traceback of at most 2,000 characters. Inner results never
   appear in the conversation unless printed. Check: stage 2 test with an inner `read_file` of
   50 KB that prints `len(text)`; the next request's last message is under 400 characters.
4. **Budgets hold.** The following each end a program with a stated reason and leave the agent
   able to continue: an infinite loop (interpreter time), a 1 GB allocation (memory), more inner
   calls than allowed, a program running past its wall-clock budget, a stopped session, and a
   crashed worker. No worker process stays alive after its program ends. Check: stage 1 and 2
   tests.
5. **Code mode never widens permissions.** Nothing a program can do is unavailable to the same
   agent as a sequence of direct calls. Tools that act on the agent's own conversation or wait on
   people or agents are refused inside programs ([C4](#c4-what-a-program-can-call)). Check: stage 2
   refusal tests; the stage 3 fork, review and named-agent tests.
6. **Off is byte-identical.** With `harness.code_mode` off, the tools, prompts and dispatch are
   byte-identical to today's. Check: the stage 2 snapshot test, against the item 61 off-mode
   snapshot.
7. **Evaluations can see and control it.** A `code_mode` loop flag and variant key turn it on or
   off. The result JSON reports, per program: the root call index, inner calls by name, outcome,
   interpreter time and output size. `--print-prompt` shows `run_script`. Check: stage 5 tests.
8. **Measured before the default changes.** A recorded run of the pilot's dev and hard suites
   through `imperium run --headless --local` in `default` permission mode meets the
   [stage 6](#stage-6-measurement-and-the-default) bar. Check: the stage 6 report (manual).

## Current state (found on `experimental` at `1c066512`)

- **Dispatch.** `Agent::run_tool` (`crates/imperium-engine/src/agent/tools.rs`, about line 703)
  unwraps a `call_tool` call into the effective call (`unwrap_call` in `agent/catalog.rs`), clears
  the per-call flags (`call_started`, `call_args_failed`) and runs `run_effective`. That function
  runs `run_tool_once` and, when a `tool.before` hook asked for a rewrite (`pending_rewrite`),
  runs it again with the rewritten arguments. `run_tool_once` checks in this order: the turn's
  snapshot, `tool_search`, `fork::refusal`, `launcher.restricted`, `named_refusal`,
  `profile_refusal`, then (loading on) universe membership. It then dispatches agent tools by name
  (`AgentTool`), providers (`run_provider`) or built-ins (`run_builtin`: `Invocation::parse`,
  read-only-mount check, `permissions().judge`, fork ask-refusal, `before_tool`, required context,
  `auto_gate`, `approve_with`, execute, `after_tool`). Every path ends in `tool_finished`, which
  emits `SessionEvent::ToolFinished` keyed by `call.id`. The dispatch takes `&mut self` and keeps
  per-call state on the agent (`pending_rewrite`, `ran_rewritten`, `rewritten_from`,
  `last_tool_ok`), so calls of one agent run one at a time.
- **The turn loop** (`agent/turn.rs`, about line 520) runs the completion's calls in order, passes
  each output through `guard_input` (the artefact handle above 75 % of the window, root only), loads
  instruction files for directories a successful call touched (`instructions::touched_dir` on the
  effective call), and re-offers tools after an `activate_plugin` round.
- **Reactive tool loading** (item 61): stages 1, 2a, 2b and 3 are merged (`a06db9bd`, `3d5db869`,
  `5cbd9b5d`, `805a3575`); stage 4 (moving variant tool edits to the engine) and stage 5 (the
  default) are not. `tool_universe()` is the agent's whole permitted list, recomputed at dispatch.
  `CORE_TOOLS` in `catalog.rs` is the fixed offered core. `effective_call()` is the one helper for
  code that reads calls back out of messages.
- **Large-result spill** (R2.5) is on branch `feat/large-result-spill` (4 commits, not merged):
  `spill.rs` saves a result past a sixteenth of the window (at least
  `harness.result_spill_min_chars`, 4,000) to `<journal>.results/` and keeps a preview and the
  path. `EXEMPT_TOOLS` lists tools never spilled. `guard_result` replaces `guard_input` in the
  loop.
- **Approvals** (`approve_with`): pre-approved tools of an unattended session run without asking
  (except floored calls). Otherwise the call waits on the broker, and an unanswered request in a
  headless run denies. Stopping a session (`Session::stop`) halts agents at their next safe point:
  a tool call in flight finishes, a model request is cancelled.
- **Isolation tiers** ([05](../05-tools-and-isolation.md#isolation-tiers)): `isolation.tier`
  confines `run_command` only. The OS tier is bubblewrap on Linux and Seatbelt on macOS; **on
  Windows it is not available** and commands fail rather than run unsandboxed. The container tier
  needs Docker. Tier 0 runs commands with the user's rights. The file tools run in the daemon at
  tier 0, confined by path checks.
- **No embedded interpreter** is a dependency of any crate (no `rquickjs`, `boa`, `mlua`, `rhai`,
  `starlark`, `wasmtime`, `pyo3`).
- **Shipping binaries.** `scripts/ci/build-desktop.sh` builds `imperium` and `imperium-core` per
  target and passes them to Tauri as `externalBin` sidecars. The CLI is packaged separately.
  Runtimes downloaded at run time are pinned in `runtimes/*.env` (llama.cpp, vLLM). The workspace
  `rust-version` is 1.89, CI's test jobs use `stable` and the bundle jobs pin 1.99.1.
- **Harness modes.** `harness.tool_loading` (`auto`/`on`/`off`, default `off`) is independent of
  attendance. Evaluations translate it in `Prepared::new` (`crates/imperium-core/src/eval.rs`):
  `auto` with `--local` becomes `on`. The loop flag is parsed in
  `crates/imperium-core/src/eval/variant.rs`.

## Decisions

### C1. The runtime: Monty, a Python-subset interpreter written in Rust

The program language and interpreter, compared against the same requirements: (a) a 4B model
writes it correctly; (b) the same confinement on Windows, macOS and Linux; (c) no capability
except the bindings; (d) limits on time, memory and calls, and cancellation; (e) the engine can
answer each tool call asynchronously, with `&mut self` dispatch, while the program waits; (f)
startup latency and binary size; (g) supply-chain risk.

| Option | (a) 4B writes it | (b, c) Confinement | (d) Limits | (e) Host calls | (f) Cost | (g) Supply chain | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **Monty** (`pydantic/monty`, Rust; worker processes from `monty-pool`) | Python, the language small models write best; a documented subset (no generators, no `del`, no method decorators, no third-party packages; has `json`, `re`, `csv`, `math`, `statistics`, `collections`, `datetime`, f-strings, comprehensions, `try`) | Language-level: the VM implements no file, network, environment or process operation; same on every OS | Memory (worker allocator), execution time (paused while the host answers), recursion, suspensions | The program **suspends** at each host call and the host resumes it with the result: no callback into the engine, no thread bridging | Under 1 ms per program from a warm pool, 5 ms for a new pool (vendor figures, to be measured in stage 1) | MIT and Python-2.0.1 (bundled CPython stdlib parts); pure Rust, no C; young (first release 2025-12-31, 22 versions, 1.7.0 on 2026-11-24); security advisory programme and bounty | **Chosen** |
| Monty in-process (`monty` crate in the daemon) | As above | Language-level holds, but a stack overflow or allocator abort kills the daemon | Memory limit needs `monty-alloc` as the daemon's global allocator | As above | Lowest | As above | Rejected: one bad program would take every session down |
| CPython script under the command sandbox (`run_command python3 ...`) | Full Python | Windows has no OS tier, so a program would run with the user's rights; tier 0 elsewhere too | OS limits only | Needs an IPC channel from the subprocess back to the daemon for every tool, a new attack surface | 30 to 200 ms per start; needs Python installed (not on stock Windows) | The user's Python | Rejected: (b), (c) and (e) fail |
| QuickJS (`rquickjs`) | JavaScript, written less reliably than Python by small models | Language-level; C engine, a history of memory-safety bugs | Memory and interrupt handler | Async bindings need careful bridging | Small, fast | C code compiled per target | Rejected: (a) and memory safety |
| Boa (`boa_engine`) | JavaScript | Pure Rust, language-level | Loop and recursion limits, no memory limit | Async job queue | Larger, slower | Pure Rust | Rejected: (a), (d) |
| Luau or Lua (`mlua`) | Lua, rarely written well by small models | Luau sandbox mode; C/C++ | Memory and interrupts | Coroutines can suspend | Small | C/C++ | Rejected: (a) |
| Rhai | A niche language a 4B model has barely seen | Pure Rust, sandboxed by design | Operation, depth and size limits | Synchronous callbacks | Small | Pure Rust | Rejected: (a) |
| Starlark (`starlark-rust`) | Python-like, but no `while`, no `try`, no f-strings in the default dialect; models write Python that fails | Hermetic, pure Rust | Few built-in limits | Synchronous callbacks | 1.3 ms | Pure Rust | Rejected: (a), (d) |
| Python in WebAssembly (wasmtime with a CPython or Monty build) | Python | Strong (WASM) | Fuel, memory | Host calls through WASI imports | 16 ms or more per start, tens of MB | wasmtime plus a CPython build | Rejected for now: cost; the first revisit if Monty's language-level sandbox is broken |

Why Monty: it is the only option that meets (a), (b), (c) and (e) together. Its suspend-and-resume
design matches how the engine dispatches: the engine awaits `run_tool` between turns of the
program, exactly as it awaits a model call, so no tool runs on another thread and the agent's
per-call state stays sound. What it costs: a young, fast-moving dependency, a Python subset the
model will sometimes step outside of (errors come back as normal Python tracebacks the model can
fix), and a second shipped binary.

**Run it in worker processes (`monty-pool`), not in the daemon.** The pool's workers are the
`monty subprocess` binary of `monty-runtime`. Imperium ships it as a sidecar named
`imperium-script`, built in CI from an exact pinned version with `--no-default-features` (the
worker alone, no CLI or telemetry). A crashed worker kills only itself, the pool replaces it, and
a `Checkout` dropped mid-program kills its worker. The worker's memory limit is real there
(`monty-alloc` is its global allocator).

**Defence in depth around the worker** (stage 1): the worker starts with an empty environment, an
empty temporary working folder and no inherited handles. On Linux it runs under bubblewrap with
`--unshare-all` (no network) and a read-only, empty root apart from its own binary. On macOS it
runs under a Seatbelt profile that denies network and file writes. On Windows it runs in a Job
Object with a memory limit, kill-on-close and an active-process limit of 1. Whether `monty-pool`
can start a worker through a wrapper command is checked in stage 1 (assumption A1). If it cannot,
the worker binary applies the confinement to itself at start: on Linux a seccomp filter and
`unshare`, on macOS `sandbox_init`, on Windows the job. The language-level sandbox is the
boundary; the OS layer limits damage if it breaks.

**Pinning.** The worker and the `monty-pool` crate are pinned to the same exact version, chosen
at least 14 days after its release, and recorded in `runtimes/monty.env` beside the llama.cpp
pin. The worker is built with `cargo install --locked --version =<pin> monty-runtime`, so it uses
upstream's lockfile, and the CI log records the hash of each built binary. Updates are deliberate
pull requests that rerun the stage 1 escape tests. Monty's MSRV (1.98) is above the workspace's
1.89: the new crate sets `rust-version = "1.98"`, and the workspace value moves to 1.98 (CI builds
with stable and 1.99.1).

Revisit: if a Monty sandbox escape is published and not fixed within a release, switch the worker
to Monty's WebAssembly runtime under wasmtime (same language, stronger boundary). If stage 6 shows
the subset costs accuracy (frequent `NotImplementedError`), weigh full CPython in that same
WebAssembly sandbox.

### C2. The tool: `run_script`

- **Contract.** `run_script({ code: string })`, defined once in `imperium-protocol`'s `tools.rs`
  (D25). Description: "Run a short Python program in a sandbox. Each of your tools is a Python
  function there: call it with keyword arguments named as in its parameters, and it returns the
  tool's text. Only what the program prints comes back to you, so use it to count, filter,
  compute or combine results instead of reading them all. The program has no files, network or
  modules except through your tools; json, re, math, statistics, csv and datetime are built in."
- **Arguments to functions.** Keyword arguments become the JSON object of the call. One
  positional `dict` is accepted as the whole object. Other positional values fill the schema's
  `required` parameters in order (`read_file("a.txt")`). Anything else raises `TypeError` with the
  tool's parameter list. The JSON goes to the tool's own parser, as `call_tool`'s arguments do
  (item 61 R4 step 5), so the program is accepted exactly when a direct call with the same
  arguments would be.
- **Names.** A tool whose name is a Python identifier is a function of that name, including
  `mcp__server__tool` names. Every tool is also reachable as `tool("name", **arguments)`, which is
  how names with `-` or `.` are called. Names are resolved lazily: the program reads an undefined
  name, Monty suspends with a name lookup, and the engine answers with a host function if the name
  is a [callable tool](#c4-what-a-program-can-call). Any other name gets `NameError`. So the
  catalog is never sent to the worker, and a tool that appears mid-session (a plugin activated by
  another agent) is callable at once.
- **Return values and errors.** A tool returns its text as `str`. A failed call (`ok` false in
  `tool_finished`: refused, denied, invalid arguments, tool error) raises `RuntimeError` whose
  message is the tool's own output, capped at 2,000 characters. The program may catch it. Monty
  has no user-defined exception classes, so one built-in type is used for all failures.
- **What the model sees.** The result of `run_script` is:
  1. The printed output (stdout and stderr in order), at most `harness.script_output_chars`
     (default 6,000). Past the cap, the first two-thirds and the last third are kept with a note
     ("output cut to 6,000 characters; print a summary instead").
  2. If the program did not finish: its error, as a Python traceback cut to 2,000 characters, or
     the budget or stop that ended it.
  3. One summary line: "(12 tool calls: read_file ×10, grep ×2; 1 failed: grep, #7)". Failed
     inner calls are named with their index so the model can find them in the traceback.

  The value of the final expression is not shown: Python programs print what they mean to
  return, and a second channel is something the model would have to learn.
- **Its own call.** `run_script` is an agent tool. It runs `tool.before` with
  `{"code": ...}` (a policy hook can block or rewrite a program; a rewrite runs the new program
  through the same checks) and `tool.after` with the result. It needs no approval of its own: the
  program has no capability, and every call it makes is judged. It is allowed in plan mode (its
  inner writes are refused by plan mode as direct ones are).
- **Rejected:** a second, "result" return channel (one rule is easier for a small model); per-tool
  typed Python stubs sent in the prompt (the cost reactive loading removes; `tool_search` already
  returns schemas); JSON-decoded return values (tools return text today, and guessing which
  outputs are JSON would make the same tool behave differently by output); one exception type per
  failure kind (Monty cannot define them, and the model rarely needs to tell them apart).

### C3. Where the interpreter runs, and how the engine drives it

- **Crate.** A new `crates/imperium-script` owns the pool, the worker path, confinement and
  limits behind a trait the engine uses:
  `ScriptRunner::start(code, limits) -> ScriptRun`, where `ScriptRun::next()` yields `Print(text)`,
  `NameLookup(name)`, `Call { name, args, kwargs }`, or `Done(Result<(), ScriptError>)`. Its
  answers are `resume_value`, `resume_name(Option<HostFunction>)` and `resume_error(message)`.
  Dropping a `ScriptRun` kills its worker. `imperium-engine` depends on the trait only, and tests
  use a fake runner that replays scripted events. `imperium-core` builds the Monty
  implementation and passes it in through the session's shared state, as it passes tool
  providers.
- **Pool.** One pool per daemon, created on the first program, `min_processes` 0 until then and 1
  afterwards, `max_processes` 4, idle workers stopped after 5 minutes, and all workers stopped
  when the daemon exits (process cleanup). A program waits for a free worker. The pool is the
  daemon's, not the session's, because workers hold no session state: each program is a fresh
  checkout and is never reused after a limit fires (Monty's guidance).
- **The driver** (`agent/script.rs`, engine). It runs inside `run_tool_once`'s `run_script` arm,
  on the agent's task:
  1. Start the program with the limits (C5).
  2. Loop on `next()`. On `NameLookup`, resolve through C4. On `Call`, check the stop flag and the
     call budget, build an inner `ToolCall` with id `<outer id>:<n>` (1-based) and the JSON
     arguments, run it through `self.run_tool(&inner)` (the whole path of a direct call), record
     the outcome, and resume with the text or the error. On `Print`, append to the bounded buffer.
  3. On `Done`, assemble the result (C2) and return it through `tool_finished` for the outer call.
- **Inner calls are sequential.** The dispatch takes `&mut self` and keeps per-call state on the
  agent, and `run_tool` clears and reads that state per call, so one inner call at a time is
  sound. Async host functions are not offered to the program (`asyncio.gather` over tools
  suspends on each call in turn). Revisit with R2.9 (concurrent read-only calls).
- **`run_tool` re-entry.** The inner call goes through `run_tool`, not `run_tool_once`, so a
  `call_tool`-shaped inner call is unwrapped and a hook rewrite retries the same inner tool. The
  outer call's rewrite state is saved before the program starts and restored after, so an inner
  rewrite cannot leak into the outer `run_script` result (`ran_rewritten` and `rewritten_from`
  are per call). The turn loop reads `ran_rewritten` after `run_tool` returns: for `run_script` it
  must see the outer call's state, which a stage 2 test asserts.

### C4. What a program can call

The **callable set** of an agent is its universe (`tool_universe()`, item 61 R4, whether loading
is on or off) minus the **script-refused** tools:

- `run_script` (no nesting), `tool_search` and `call_tool` (the program calls tools directly; a
  `call_tool("x", ...)` in a program would be a second spelling of `x`);
- the delegation family and everything that waits on another agent or a person: `spawn_agent`,
  `await_tasks`, `steer_agent`, `queue_agent`, `list_agents`, `resume_agent`, `delegate_chunks`,
  `keep_as_knowledge`, `ask_agent`, `list_knowledge_agents`, `ask_user`,
  `request_clarification`, `start_session`;
- tools that change the agent's own conversation or tool list: `update_plan`, `update_state`,
  `load_skill`, `activate_plugin`, `context_load`.

Reasons: a program's results are hidden from the conversation, so anything whose effect is meant
to be seen by the model or the user in the conversation (a plan, a loaded skill, a question)
belongs outside it. A subagent started from a program would outlive the program's budgets.
Activating a plugin would change the offered list mid-turn. Everything else is callable: file
tools (reads and writes), `run_command` (foreground and background) with `command_output` and
`command_stop`, web, browser and media providers, memory tools (writes ask as usual), MCP tools and
activated plugins' tools.

A refused name resolves to a function that raises `RuntimeError("<name> cannot be called from a
program; call it directly")`, so the model learns why. Every other check (fork, launcher,
named-agent list, profile, universe at call time, permission rules, hooks, approvals) happens in
`run_tool` as for a direct call. The resolver's universe check only decides which names exist.

**Approvals inside a program.** An inner call that asks blocks the program like a direct call
blocks the turn. The interpreter's time budget is paused while the host answers (Monty's feed
clock), and the wall-clock budget excludes time spent waiting for the user (C5). The approval
summary is prefixed "From a program: " so the person knows the call came from code. In an
unattended session, pre-approved tools run and a floored call waits, as today. In a headless run
nobody answers, so the call is denied, the program gets `RuntimeError("The user denied this
action.")`, and it can carry on. In auto mode the classifier judges each inner call. Its input
also carries the program's code (cut to 4,000 characters) as data, so it can judge a write or
command against what the program is for.

### C5. Budgets and cancellation

New `[harness]` keys, defaults chosen for a small local model; all are per program:

| Key | Default | Enforced by |
| --- | --- | --- |
| `script_calls` | 40 inner tool calls | Engine counts calls; Monty's `max_suspensions` set to calls plus name lookups as a backstop |
| `script_exec_secs` | 10 s of interpreter time | Monty `max_feed_duration` (paused during host calls) |
| `script_wall_secs` | 300 s, excluding time waiting on approvals | Engine deadline, checked between events and as a timeout around the worker's next event |
| `script_memory_mb` | 256 MB | Monty `max_memory` in the worker, plus the OS limit (job or rlimit) at 2× as backstop |
| `script_output_chars` | 6,000 | Engine print buffer (C2) |
| `script_code_chars` | 20,000 | Engine refuses longer programs before starting a worker (compilation is not charged to the time budget) |
| (fixed) inner result size | 4 MB | Engine raises `RuntimeError` instead of passing a larger text into the program |

- **Stop.** Before each inner call and on each event, the driver checks the session's stop flag.
  If stopped, it drops the run (killing the worker) and returns the output so far with "the
  session was stopped; the program did not finish". This matches `Session::stop`: a tool call in
  flight finishes, nothing new starts.
- **Turn cancellation and errors.** Dropping the outer future drops the `ScriptRun`, which kills
  the worker. A crashed worker returns "the program's interpreter stopped unexpectedly", and the
  pool replaces the worker.
- **Max steps.** A program is one tool call in the turn's step count: 40 inner calls cost the
  turn nothing in `max_steps`. The call budget bounds them instead.

### C6. Results, events, journal, transcripts and compaction

- **Conversation.** Only the outer call (`run_script` with the code) and its assembled result are
  messages. Compaction's transcript renders it as any call: `[called run_script with <code>]`
  followed by the result. Knowledge cards and harness readers see `run_script`, and
  `effective_call` leaves it as it is.
- **Events.** Each inner call emits `tool_started` and `tool_finished` with call id
  `<outer>:<n>`. `ToolStarted` gains an optional `parent_call_id` (the outer call's id),
  additive, protocol minor bump. Approval requests carry the inner tool's name, as they do today.
  The journal records events as it does now; a replayed session rebuilds the nested cards from
  them.
- **Desktop.** The `run_script` card shows the program (collapsed beyond 12 lines), a live list of
  inner calls as child rows (each expandable to its own arguments and output, as tool cards are
  today), the printed output and the summary line. CLI `chat` indents inner calls under the
  program.
- **Instruction files.** An inner `read_file` (or other path tool) that succeeds marks its
  directory as touched, as a direct call does. The driver collects these and the turn loop loads
  the instruction files after the program, so code mode does not skip a folder's `AGENTS.md`.
- **Large results (R2.5).** Inner results are not spilled: they go into the program, not the
  conversation. `run_script` joins `spill::EXEMPT_TOOLS`, since its output is already capped below
  the spill floor (6,000 against at least 8,192 characters for a 32k window; for a smaller window
  the cap shrinks to the spill threshold). If R2.5 is not merged when stage 2 lands, stage 2 adds
  the exemption when it rebases.
- **Context breakdown.** `run_script` results count as `tool_results`, as any tool's do.

### C7. The setting, and which agents get it

`harness.code_mode` = `auto`, `on` or `off`, in `[harness]`, independent of attendance (the
program asks nobody unless a tool asks). `auto` means on when the agent's own model runs on this
machine (`ResolvedModel.local`), for the root and subagents, and follows the model at turn
boundaries as `tool_loading` does. The default is `off` until stage 6, then `auto`.

- **Offered list.** In code mode `run_script` is added to the offered list. With reactive loading
  on, it is a core tool (after `run_command`, so the fixed list stays fixed for the session).
  With loading off, it follows `run_command` in the full list.
- **Prompt.** One sentence in its own swappable region, beside item 61's `loader_text`: "For
  counting, filtering, arithmetic or combining many results, write a run_script program that calls
  your tools and prints only the answer." It is swapped with the mode when the model changes.
- **Hosted models** get it only with `on`. Hosted frontier models have native code execution and
  cheap cached prefixes, and the measured failure is a 4B local model's.

### C8. Sessions, subagents, named agents, forks, reviews, background commands

- **Subagents** get `run_script` under the same rule for their own model. Their callable set is
  their own universe.
- **Named agents.** A file's `tools` list must name `run_script` for the agent to have it. Its
  inner calls are limited to the names in the list by `named_refusal`, as direct calls are.
- **Question forks.** `run_script` is added to `FORK_TOOLS`: its inner calls go through
  `fork::refusal`, so a fork's program can read, search and use the web, and nothing else. The
  fork's frozen tool list keeps `run_script` only if the source agent was offered it.
- **Initiative reviews.** `run_script` is not added to `REVIEW_TOOLS`, so the launcher refuses it
  during a review. Reviews are short and read-only, and a smaller surface is safer than exempting
  the outer call as item 61 did for `call_tool`.
- **Background commands.** `run_command(..., background=True)` is allowed and returns the
  command's id. A process outlives the program as it outlives a direct call, R2.14's completion
  notice arrives at the next steering point, and `command_output` and `command_stop` are callable.
  A foreground command's time counts against `script_wall_secs`.
- **Resume.** A program in flight when the daemon stops is lost, like any tool call in flight. On
  resume, the call has no result and gets the existing "did not finish" handling.
- **The MCP bridge** (`imperium mcp-serve --tools`) does not offer `run_script`: the client has its
  own code execution, and a bridged program would ask approvals across two harnesses.

### C9. Security

| Threat | Mitigation | Residual risk |
| --- | --- | --- |
| Sandbox escape from the interpreter (VM bug) | Monty's language-level sandbox; the worker process, not the daemon; empty environment and working folder; OS confinement per platform (C1); pinned version, escape tests on every update | A memory-safety bug in Monty plus a confinement gap. Weakest on Windows (job object only, no file or network confinement), where the language level is the only boundary against file or network access |
| Capability through the bindings | Every inner call is `run_tool`: same refusals, rules, hooks, approvals as direct calls; script-refused set (C4) | A program chains read-then-send in one model step (read a secret, `web_fetch` a URL with it in the query). Today the model can do the same in two steps; code mode removes the pause between them, not a check. Auto mode sees the code; the desktop shows each inner call live |
| Prompt injection from tool results into the program | Results are values in variables; they never become code unless the program evaluates them. `eval` and `exec` are made unavailable in programs (A2), so text from a tool cannot run as code. The printed output enters the context as a tool result with the existing untrusted-data framing | The model reads injected text that the program printed, as with any tool result |
| Resource exhaustion | C5 budgets; memory limit in the worker plus an OS limit; pool size cap (4 workers); code length cap; output cap | Many sessions at once queue for workers (bounded wait inside the wall budget) |
| Secrets | Worker environment cleared, so no `GH_TOKEN`, API keys or daemon token; no file access except through `read_file`, whose path checks hide the daemon's files as today; the worker runs outside the OS tier's hidden paths list only because it has no file access at all | None beyond the tools' own rules |
| Network | None inside the interpreter; Linux and macOS confinement also deny it at the OS level; web tools go through their own rules | Windows has no OS-level network block for the worker (A3) |
| Denial through approvals | A program that asks 40 times is bounded by `script_calls`, and a denial is an ordinary error the program sees | The user sees up to 40 prompts. Approve-for-session works as today |

### C10. Evaluations

- **Loop flag and variant key** `code_mode` (`on`, `off`, `auto`), translated in `Prepared::new`
  like `tool_loading` (`auto` with `--local` becomes `on`). The budget keys are flags too
  (`script_calls`, `script_exec_secs`, `script_output_chars`).
- **Result fields** (additive, CLI-only types): `code_mode { mode, programs: [{ call, inner:
  [{ name, ok }], outcome: "done" | "error" | "budget" | "stopped" | "crashed", exec_ms, wall_ms,
  output_chars, error_kind }] }`, where `call` is the 1-based root call index. `--print-prompt`
  shows `run_script` in the tools and the code-mode sentence.
- **Variant tool edits** (item 61 stage 4 moves them to the engine): a renamed tool is called by
  its new name inside programs (the resolver maps the variant name back to the engine name before
  C4), and a dropped tool is not callable. Programs in recorded scripts are part of the outer call,
  so recordings stay keyed as today.
- **`prompt_hash`** changes when code mode is on (one more tool and the sentence); it is
  unchanged with `--flag code_mode=off`.

## Scope and non-goals

In scope: C1 to C10, the worker sidecar and its CI build, the setting and flags, protocol field,
desktop and CLI rendering, tests and docs, and the measurement that gates the default.

Not in scope (recorded only): concurrent inner calls (R2.9); programs that persist state between
calls (a REPL per agent; each program starts empty); file mounts into the interpreter (Monty's
`monty-fs`); hosted models by default; the headless default permission mode (item 61 S4);
retraining Emperor or Consul to write programs.

## Assumptions

Each is checked in stage 1 and its outcome recorded in this plan before stage 2 starts.

- **A1.** `monty-pool`'s subprocess config can start the worker through a wrapper (program plus
  arguments) or pre-spawn hook, so confinement can wrap it. If not, the worker confines itself
  (C1).
- **A2.** `eval` and `exec` can be removed from a program's builtins (a Monty option or a name
  policy). If not, the engine refuses a program whose source uses either name outside a string
  literal, before it starts (a token scan with Monty's parser, or a conservative regex), and the
  refusal says why.
- **A3.** A Windows Job Object memory limit and active-process limit work on the worker without
  breaking the pool's pipes. No file or network confinement is planned on Windows beyond the
  language level (AppContainer is item-level work elsewhere in 05).
- **A4.** Startup from a warm pool is under 20 ms and the worker binary under 15 MB on each
  target. If either fails by more than 2×, stage 1 records it and the setting description says so.
- **A5.** Qwen3.5-4B writes short Python programs that stay inside Monty's subset often enough.
  Stage 6 measures syntax, `NotImplementedError` and `NameError` rates.
- **A6.** The guided-harness pilot's failures on aggregation are reachable by code: the knowledge
  base is files the program can `read_file` or `grep`, and the counts follow from them. True for
  the dev and hard suites (gold answers are computed from the structured world).

## Relevant paths

`crates/imperium-script/` (new), `crates/imperium-engine/src/agent/{script,tools,turn,catalog,prompts,fork}.rs`,
`crates/imperium-engine/src/{agent.rs,session.rs,spill.rs}`, `crates/imperium-engine/src/agent/auto.rs`,
`crates/imperium-protocol/src/{tools,session,lib}.rs`, `crates/imperium-protocol/schemas/`,
`crates/imperium-config/src/{settings,registry}.rs`, `crates/imperium-core/src/{eval.rs,eval/variant.rs,state/create.rs}`,
`crates/imperium-core/src/` (pool construction), `crates/imperium-cli/src/{eval.rs,eval/,chat.rs}`,
`crates/imperium-cli/tests/headless_eval.rs`, `apps/desktop/src/sessions/`, `apps/desktop/src/panels/SessionPanel.tsx`,
`scripts/ci/build-desktop.sh`, `.github/workflows/{ci,build}.yml`, `runtimes/monty.env` (new).

## Related docs

[03 agent loop](../03-agent-engine.md#agent-loop),
[03 harness aids](../03-agent-engine.md#harness-aids-for-small-models),
[03 evals](../03-agent-engine.md#running-evals-through-the-harness-implemented),
[05 built-in tools](../05-tools-and-isolation.md#built-in-tools),
[05 isolation tiers](../05-tools-and-isolation.md#isolation-tiers),
[05 permissions](../05-tools-and-isolation.md#permissions),
[10 configuration](../10-configuration.md), [11 CI/CD](../11-ci-cd.md),
[13 decisions](../13-decisions.md), [reactive tools plan](reactive-tools.md).

## Stages

Rust is built and tested by CI only; every acceptance check is a CI job unless marked manual.
Stages 1 to 5 are in order and ship with `harness.code_mode = off`, so each experimental build
behaves as today unless a user or an evaluation turns it on. Stage 5 needs item 61 stage 4 merged
(variant tool edits in the engine). Stage 6 follows the build that contains stage 5.

## Stage 1: The interpreter worker and `imperium-script`

- Objective: C1 and C3's crate, with the worker built and shipped on every target; A1 to A4
  settled and recorded here.
- Changes: `runtimes/monty.env` (pinned version); `crates/imperium-script` with the
  `ScriptRunner` trait, the Monty pool implementation (limits, confinement wrapper or
  self-confinement, worker path discovery beside the daemon binary, idle shutdown, stop on drop),
  and a fake runner for engine tests; `scripts/ci/build-worker.sh` (`cargo install --locked
  --version =<pin> --no-default-features monty-runtime` per target into a staging folder, the macOS
  universal build with `lipo`, the binary renamed `imperium-script[.exe]` and its SHA-256 logged);
  `build-desktop.sh` adds it to `externalBin`; the CLI package and the server image (item 63, if
  merged) carry it; a CI job builds the worker once per OS and exports `IMPERIUM_SCRIPT_WORKER` for
  the tests; workspace `rust-version` 1.98.
- Tests (`crates/imperium-script/tests/`, real worker, all three OSes): a program prints;
  suspends at a host call and resumes with the value; a name lookup resolves to a host function
  and an unknown name raises `NameError`; `open()`, `import os`, `os.environ`, `socket`,
  `subprocess`, `__import__` and (per A2) `eval`/`exec` fail; `while True: pass` ends on the time
  budget; `x = [0] * 10**9` ends on the memory budget; more suspensions than allowed abort with an
  uncatchable error; dropping a run mid-program leaves no worker process (checked by PID); a
  killed worker is reported as crashed and the next program runs; the pool stops its workers on
  shutdown; the worker's environment is empty. A measurement test logs warm and cold start and
  binary size (A4).
- Docs: 05 gains *Programs (`run_script`)*, with the interpreter and confinement subsection
  (implemented part only); 11 lists the worker build; 13 gains the next free decision number (C1).
- Depends on: none. Size: about 600 lines plus CI.
- Owned paths: `crates/imperium-script/**`, `Cargo.toml`, `Cargo.lock`, `runtimes/monty.env`,
  `scripts/ci/build-worker.sh`, `scripts/ci/build-desktop.sh`, `.github/workflows/{ci,build}.yml`,
  `docs/05-tools-and-isolation.md`, `docs/11-ci-cd.md`, `docs/13-decisions.md`, this plan.
- Risks: a target where the pinned version does not build (Windows arm64 is the likeliest); then
  code mode is unavailable on that target and the setting says so, rather than blocking the stage.

## Stage 2: The tool, the driver and dispatch

- Objective: C2 to C5 for the root and plain subagents, behind `harness.code_mode` (default
  `off`, hidden from the settings UI until stage 3).
- Changes:
  - Protocol (next free minor at merge): `run_script()` `ToolDefinition` in `tools.rs` (exported
    to `schemas/tools/`); `ToolStarted.parent_call_id`; the 02 version paragraph.
  - Config: `HarnessSettings.code_mode: HarnessMode` (default `Off`) and the C5 budget keys.
  - Engine: `agent/script.rs` (the driver, resolver, callable set, script-refused set, argument
    mapping, output assembly, summary line, stop and budget checks); the `run_script` arm in
    `run_tool_once` with `tool.before`/`tool.after` on the code; inner call ids and
    `parent_call_id`; saving and restoring the outer rewrite state; touched directories returned
    to the turn loop; `run_script` in the offered list per C7 (core when reactive); the code-mode
    sentence as its own prompt region; the "From a program: " approval prefix; auto mode's input
    carries the code; `run_script` in `spill::EXEMPT_TOOLS` (if R2.5 is merged).
- Tests (engine, fake runner unless marked):
  - Off mode: the tool list and prompts equal the item 61 off-mode snapshot (goal 6).
  - Dispatch twins (goal 2), each a copy of an existing direct-call test through a program: a
    profile refusal; a named-agent refusal; a universe refusal (reactive on); a permission-rule
    refusal; plan mode refusing an inner `write_file`; a `tool.before` hook that blocks an inner
    call (the program sees `RuntimeError`) and one that rewrites it (the same tool runs with the
    new arguments, and the outer `run_script` result carries no rewrite note); approval asked for
    an inner `run_command` with the prefixed summary, then allowed, then denied; auto mode's
    classifier input contains the code; pre-approved tools in an unattended session run without
    asking.
  - Each script-refused tool raises with the "call it directly" message and never reaches
    dispatch; an unknown name raises `NameError`; `tool("mcp-style.name", ...)` calls a provider
    tool with a dotted name.
  - Arguments: keyword, single dict, positional mapped to `required`, and a wrong type raising
    `TypeError` with the parameter list; arguments a direct call's tolerant parser accepts are
    accepted.
  - Output: a 50 KB inner read with `print(len(text))` gives a result under 400 characters (goal
    3); output past the cap keeps the head and tail with the note; the summary line counts calls
    by name and names failed ones; a traceback is cut to 2,000 characters.
  - Budgets: call budget, wall budget (fake runner with a slow tool), wall time excluding an
    approval wait, code length refused before start, inner result over 4 MB raises; a session
    stop between inner calls ends the program with the output so far.
  - Events: inner calls emit `tool_started`/`tool_finished` with `<outer>:<n>` ids and
    `parent_call_id`; the outer call's `tool_finished` comes last.
  - Instruction files: an inner `read_file` in a folder with `AGENTS.md` loads it after the
    program.
  - Real worker (CI job with `IMPERIUM_SCRIPT_WORKER`): a program that greps a fixture, filters
    rows and prints a count gives the right number, with the inner calls in events.
- Docs: 03 gains *Programs (code mode)* (C2 to C7 as built); 05 *Programs* gains the binding and
  dispatch rules; 02 protocol version paragraph.
- Depends on: stage 1. Size: about 1,000 lines, so two pull requests: 2a (protocol, config,
  driver, dispatch and their tests, with the fake runner) and 2b (wiring the real runner, offered
  list, prompt region, approval prefix, auto mode, spill exemption, instruction files, and the
  real-worker test).
- Owned paths: `crates/imperium-protocol/**`, `crates/imperium-config/src/{settings,registry}.rs`,
  `crates/imperium-engine/src/agent/{script,tools,turn,catalog,prompts,auto}.rs`,
  `crates/imperium-engine/src/{agent.rs,session.rs,spill.rs}`, `crates/imperium-core/src/`
  (passing the runner into sessions), `docs/02-architecture.md`, `docs/03-agent-engine.md`,
  `docs/05-tools-and-isolation.md`.
- Risks: a path where an inner call skips a check (each check has a twin test, and the inner call
  goes through `run_tool` itself, not a copy); per-call agent state leaking between the outer and
  inner calls (the rewrite-state test).

## Stage 3: Sessions, subagents, forks, reviews and the interface

- Objective: C6 and C8, and the setting made visible.
- Changes: subagents and named agents per C8; `run_script` in `FORK_TOOLS`; nothing for reviews
  beyond a test; the desktop's nested program card (code, live inner rows, output, summary) and
  CLI indentation; journal replay rebuilds nested cards from `parent_call_id`; the registry entry
  `harness.code_mode` ("Let agents write programs that call tools") and the 10 rows for it and the
  budgets.
- Tests: a subagent with a local model gets `run_script` and its program cannot call a tool its
  profile forbids; a named agent without `run_script` in its list is not offered it, and one with
  it cannot call an unlisted tool from a program; a question fork's program reads a file and is
  refused `write_file`; an initiative review refuses `run_script` with the review's message
  (`crates/imperium-core/tests/`, the initiative tests' daemon harness); a background command
  started from a program is listed by `command_output` after the program ends and its completion
  notice arrives; a resumed session renders the nested cards; compaction's transcript shows
  `[called run_script with ...]` and the result; desktop unit tests for the nested card and the
  live update.
- Docs: 07 desktop (the program card), 10 configuration, 03 updates.
- Depends on: stage 2. Size: about 500 lines.
- Owned paths: `crates/imperium-engine/src/agent/{delegation,fork,compaction}.rs`,
  `crates/imperium-engine/tests/`, `crates/imperium-core/tests/`, `crates/imperium-config/src/registry.rs`,
  `apps/desktop/src/**` (sessions and the tool card), `crates/imperium-cli/src/chat.rs`,
  `docs/03-agent-engine.md`, `docs/07-desktop-ui.md`, `docs/10-configuration.md`.

## Stage 4: Security review pass

- Objective: an independent review of stages 1 to 3 against C9 before evaluations run programs
  at scale.
- Work: a security reviewer (not the implementer) reviews the worker confinement on each OS, the
  dispatch twins, the resolver, the budgets and the approval prefix, and tries an escape suite.
  Findings are fixed in this stage's pull request.
- Changes: fixes; `crates/imperium-script/tests/escape.rs` with the reviewer's escape attempts
  as regression tests (introspection through `__class__`/`__subclasses__`, `type()` tricks,
  `locals()`, huge string formatting, deep nesting in source, recursion, unicode name tricks,
  catching the abort exception, a host function that is not callable).
- Depends on: stage 3. Size: depends on findings; the escape suite is about 200 lines.
- Owned paths: `crates/imperium-script/**`, files named by findings.

## Stage 5: Evaluation surface

- Objective: C10.
- Changes: the `code_mode` loop flag and budget flags in `variant.rs`, `Prepared::new` and
  `state/create.rs`; `EvalResult.code_mode`; `--print-prompt` showing `run_script`; the resolver's
  variant-name mapping (on top of item 61 stage 4's engine-side edits).
- Tests (`headless_eval.rs`, scripted endpoint, real worker in CI):
  - `a_program_counts_rows_and_only_the_count_enters_the_context`: `--local --flag code_mode=on`;
    a workspace with a 60-line CSV; call 1 answers `run_script` with a program that reads the file,
    filters rows and prints a count; call 2 answers text. Asserts: request 2's last message holds
    the count and the summary line and is under 400 characters; `code_mode.programs` has one entry
    with `inner = [read_file]` and `outcome = done`; events carry the inner call with
    `parent_call_id`.
  - `programs_compute_in_default_permission_mode`: the same run with `--permission-mode default`
    computes the count without any approval request.
  - `an_inner_command_is_denied_headless`: a program calls `run_command` in `default` mode; the
    inner call is denied, the program catches `RuntimeError` and prints a fallback.
  - `a_dropped_tool_is_not_callable_from_a_program` and
    `a_renamed_tool_is_called_by_its_new_name_in_a_program`.
  - `code_mode_off_changes_nothing`: with the flag off, request 1 equals the off snapshot and the
    prompt hash equals the run without the flag.
- Docs: 03 evals (flags, fields, hashes).
- Depends on: stage 4, and item 61 stage 4. Size: about 400 lines.
- Owned paths: `crates/imperium-core/src/{eval.rs,eval/**,state/create.rs}`,
  `crates/imperium-protocol/src/methods/eval.rs`, `crates/imperium-cli/src/{eval.rs,eval/**}`,
  `crates/imperium-cli/tests/headless_eval.rs`, `crates/imperium-engine/src/agent/script.rs`,
  `docs/03-agent-engine.md`.

## Stage 6: Measurement and the default

- Objective: goal 8, then the default.
- Work (manual, on the experimental build containing stage 5; one model at a time, on an idle
  machine): run the pilot's suites (Imperium-Models `experiments/guided-harness-pilot/`, dev 135
  items and hard 72) through `imperium run --headless --local` with Qwen3.5-4B in **`default`
  permission mode** (the product default, where the shell is denied), as four arms:
  `code_mode` off and on, each with `tool_loading` at its released default and at `on`. Add a
  `harness_code` arm to `imperium_backend.py`/`arms.py` that passes `--flag code_mode=...`; the
  task prompts are unchanged (no "compute with the shell" policy). Record: accuracy (strict
  scorer), prompt and cached tokens per item, model calls per item, programs per item, inner calls
  per program, program outcomes (done, Python error by type, budget, crash), `NotImplementedError`
  and `NameError` rates (A5), and wall time. Report in Imperium-Research beside the pilot.
- Acceptance for `auto`. The stock `harness` arm is near the ceiling on most of these categories
  (14/15 `compare_aggregate`, 14/15 `compute`, 9/9 `hard_filter`, 5/9 `hard_compute`: 42 of 48),
  so the bar is mostly about not losing accuracy and about cost:
  - the four categories `compare_aggregate`, `compute`, `hard_filter` and `hard_compute` together
    score at least 45 of 48 with code mode on (the shell-enabled refined arm's level, without the
    shell), and `hard_compute` at least 8 of 9;
  - neither suite loses more than 2 items overall, and no other category more than 1;
  - in those four categories, model calls per item fall by at least 25 % and prompt tokens per
    item by at least 20 %; across each whole suite, prompt tokens per item do not rise by more
    than 10 %;
  - no step-limit stop in those four categories;
  - at most 20 % of programs end in a Python error, and none in a crash.
- Then, in one small pull request: the default becomes `auto` (settings default, registry text,
  10, 03, roadmap item 65 marked implemented with the measured numbers, this plan's status). If it
  fails, the default stays `off` and 13 records why. If programs fail mainly on the subset, the next
  step is a prompt line listing what is unsupported; if the model rarely writes programs, the next
  step is the sentence's wording; neither changes the runtime choice.
- Depends on: stage 5 released.

## Rollout

- Stages 1 to 5 ship `harness.code_mode = off`. Stage 1 adds a binary to every bundle, unused
  until the setting or `--flag code_mode` turns the mode on.
- Stage 6's pull request sets `auto` (on for local models, Emperor and Consul included; off for
  hosted ones). `off` restores today's behaviour; `on` enables hosted models too.

## Release path

As in the [reactive tools plan](reactive-tools.md#release-path): `experimental` is the default
branch, `CI gate` is the required check, and every merge to `experimental` publishes
`v0.2.<run>-experimental` through `.github/workflows/experimental.yml` with no version bump or
changelog. Stage 1 also needs the bundle jobs (`build.yml`) to pass on all five targets, since it
adds a sidecar; root checks the `Experimental` run for that merge and that the release assets carry
`imperium-script` on each target. Stage 2 bumps the protocol minor (the next free one at merge).

## Risks across stages

- **A young dependency.** Monty is 11 months old with 22 releases. Pinning, a 14-day minimum age,
  the escape suite on every update, the worker process boundary and OS confinement contain the
  risk. The WebAssembly runtime is the fallback.
- **Windows confinement.** Only the language-level sandbox stops file and network access there.
  This is the same boundary Monty's own Python and JavaScript users rely on, plus a process
  boundary and a job object; the residual risk is stated in 05.
- **A check skipped for inner calls.** Inner calls go through `run_tool` itself, and every
  direct-call refusal has a program twin test.
- **The small model writes programs badly.** Stage 6 measures it before any default changes.
  Python errors come back as tracebacks the model can fix in its next call, at the cost of a step.
- **Hidden work.** Results the model never sees can't be cited. The desktop shows every inner call,
  the summary line names them, and the model can print what it needs.
- **Two budgets for the same turn.** A program's wall budget (300 s) sits inside the turn. A
  program near its limits can make a turn much longer than one tool call; the summary line and
  events make it visible.
- **Protocol minor collisions.** Concurrent pull requests take the next free minor at merge.

## Open questions

For the user:

1. **Hosted models.** `auto` is local-only (C7). Should hosted models get `run_script` by default
   too? Anthropic's reported savings are for hosted models with many MCP tools.
2. **Shipping Monty.** Is a third-party interpreter worker, 11 months old and pinned, acceptable in
   every bundle, given the Windows confinement limit (C9)? The alternative that avoids it is the
   WebAssembly runtime, at about 16 ms or more per program and a larger bundle.
3. **Memory writes from programs.** C4 allows `memory_remember`, `memory_edit` and
   `memory_forget` inside programs, gated by their usual approval. Should memory changes stay
   direct-only, so each is a visible step in the conversation?
4. **A harder aggregation suite.** The pilot suites leave little headroom (42 of 48 stock in the
   target categories), so stage 6 can show that code mode costs less but hardly that it is more
   accurate. Should stage 6 also author a small aggregation suite (larger tables, counts over 50
   or more rows, grouped sums) in Imperium-Models before the default changes?

Settled inside this plan (recorded for review): the runtime (C1), local-only `auto` (C7), reviews
refuse `run_script` (C8), and the budget defaults (C5, tunable by flags in stage 6).
