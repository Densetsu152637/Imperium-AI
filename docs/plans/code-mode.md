# Plan: programmatic tool calling (code mode)

Revision: 2026-11-26, revision 5 (final review: D1, and the label and call-state refinements; delta review of revision 3: N1 to N7 and the F12 follow-up; plan critic round 1: findings F1 to F12, all accepted, with the root's decisions on F3 and F4; the root's answers recorded as [settled decisions](#settled-decisions)); written against `experimental`
at `1c066512`. Roadmap [item 65](../12-roadmap.md#next-in-order); Imperium-Research roadmap R2.8.
Status: stage 1a built (pull request #303), stages 1b to 6 planned.

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

## Settled decisions

Recorded from the root's answers (2026-11-26):

- **S1.** `harness.code_mode = auto` means on for agents whose own model runs on this machine and
  off for hosted models ([C7](#c7-the-setting-and-which-agents-get-it)). Hosted models get
  `run_script` only with `on`.
- **S2.** Shipping the Monty worker is accepted on two conditions. `harness.code_mode` stays `off`
  by default until the [stage 4](#stage-4-security-review-pass) independent security review has
  passed, so stage 6 cannot change the default before then. The Windows confinement gap is
  mitigated where it can be and the rest is recorded as an accepted risk with its rationale
  ([C9, Windows](#windows-confinement-mitigation-and-accepted-risk)).
- **S3.** Memory writes (`memory_remember`, `memory_edit`, `memory_forget`) are refused inside
  programs ([C4](#c4-what-a-program-can-call)). `memory_search` and `memory_read` stay callable.
- **S4.** Stage 6 adds a harder aggregation suite, generated with gold answers computed by script,
  so accuracy gains can be measured ([stage 6](#stage-6-measurement-and-the-default)).

## Goals

Each goal is boolean and has a check. "Scripted endpoint" is the fake OpenAI-compatible server of
`crates/imperium-cli/tests/headless_eval.rs`. "Fake runner" is the test double of the engine's
script-runner trait ([C3](#c3-where-the-interpreter-runs-and-how-the-engine-drives-it)); "real
worker" is the shipped interpreter binary, which CI builds.

1. **A script reaches tools only through dispatch.** With the real worker, a program can't read a
   file, read an environment variable, open a socket, start a process or load a module except by
   calling a bound tool. If the OS confinement of [C1](#c1-the-runtime-monty-a-python-subset-interpreter-written-in-rust)
   cannot be applied, no program runs at all. Check: stage 1 worker tests (`open()`,
   `os.environ`, `os.getenv`, `socket`, `subprocess`, `__import__`, `import os` each fail;
   `print(open("Cargo.toml").read())` raises), the confinement tests, and the fail-closed test.
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
   people or agents are refused inside programs ([C4](#c4-what-a-program-can-call)), and the
   refusal is enforced on every call the worker reports, not only when a name is looked up (the
   worker is untrusted). Check: stage 2 refusal tests, including a fake runner that sends a call
   for `spawn_agent` it never looked up; the stage 3 fork, review and named-agent tests.
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
| **Monty** (`pydantic/monty`, Rust; in an Imperium-owned worker process) | Python, the language small models write best; a documented subset (no generators, no `del`, no method decorators, no third-party packages; has `json`, `re`, `csv`, `math`, `statistics`, `collections`, `datetime`, f-strings, comprehensions, `try`) | Language-level: the VM implements no file, network, environment or process operation; same on every OS | Memory (worker allocator), execution time (paused while the host answers), recursion, suspensions | The program **suspends** at each host call and the host resumes it with the result: no callback into the engine, no thread bridging | Vendor figures: under 1 ms from a warm pooled worker, 5 ms for a new pool; Imperium starts a fresh confined worker per program, so its cold start is measured in stage 1 (A4) | MIT and Python-2.0.1 (bundled CPython stdlib parts); pure Rust, no C; young (first release 2025-12-31, 22 versions, 1.7.0 on 2026-11-24); security advisory programme and bounty | **Chosen** |
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

**Run it in an Imperium-owned worker process, one per program, not in the daemon** (root
decision on F4). The worker is a small binary crate in Imperium's own workspace,
`crates/imperium-script-worker`, so its dependencies sit under Imperium's `Cargo.lock` and are
built and checked with everything else. It depends on `monty` and `monty-proto` (feature
`worker`, whose `Worker` state machine is the interpreter loop upstream's own `monty subprocess`
wraps in about 190 lines) and on `monty-alloc` as its global allocator, so the memory limit is
real. It reads framed `monty-proto` requests on stdin and writes events on stdout. Stage 1
measures the binary with and without `monty-type-checking`, which the `worker` feature pulls in;
if the type checker adds more than 5 MB or 20 MB of resident memory and cannot be left out
through a feature, the worker drives `monty::MontyRun` directly instead of `monty_proto::worker`,
keeping the same framing (F12). It ships as the sidecar `imperium-script`.

The daemon side (`crates/imperium-script`) drives the protocol directly with `monty-proto`'s frame
types. It does not use `monty-pool`: the pool's spawn and reuse rules cannot express the Windows
launch below or one worker per program, and driving the protocol ourselves keeps every event in
one place ([C3](#c3-where-the-interpreter-runs-and-how-the-engine-drives-it)).

**One fresh worker per program** (F5): a worker serves exactly one program and is killed when it
ends, whatever the outcome, so nothing a program did can reach the next one. To hide start-up
cost, the daemon may keep **one** confined worker spawned ahead and idle (never fed), which the
next program takes; it is still used once. Stage 1 measures cold start (A4).

**OS confinement, applied before the worker reads any program** (root decision on F3, F4, F10):

- **Linux.** The worker confines itself at start: `PR_SET_NO_NEW_PRIVS`, then a seccomp-bpf
  allowlist built with `seccompiler` (N3). The filter first checks the architecture
  (`AUDIT_ARCH_X86_64` or `AUDIT_ARCH_AARCH64`, anything else kills the process) and rejects x32
  system calls on x86-64. It allows reads and writes on its two pipes, memory management,
  futexes, clocks, `getrandom`, signal handling, `exit` and `exit_group`; `kill` and `tgkill` only
  with the worker's own process id; `clone` only with thread flags, and `clone3` returns `ENOSYS`
  so the runtime falls back to `clone`. Everything else fails, including `open`, `openat`,
  `socket`, `connect`, `execve`, a process-creating `clone` and `ptrace`. The filter is installed
  before any thread is started; if a thread exists by then, it is installed with
  `SECCOMP_FILTER_FLAG_TSYNC`, and the worker fails closed if that flag is refused. Landlock (no
  file access at all) is applied as well where the kernel has it. Memory is capped with `RLIMIT_DATA` (not `RLIMIT_AS`, which large virtual
  reservations trip) at twice `script_memory_mb`, and with the cgroup v2 memory controller when
  the daemon runs in a delegated cgroup (stage 1b; not part of 1a). Namespaces are not required, so the worker does not
  depend on unprivileged user namespaces (restricted by AppArmor on Ubuntu 24.04).
- **macOS.** The worker calls `sandbox_init` with a profile that starts from `(deny default)`
  and allows only what the worker needs (N3): `file-read*` of its own binary and the system
  libraries and dyld shared cache it loads, `sysctl-read` of the few keys the Rust runtime asks
  for, and nothing else. Network, process creation, file writes, other file reads, `mach-lookup`
  (so no pasteboard, no launch services, no other system service), IPC and IOKit are all denied
  by the default. It sets `RLIMIT_CORE` to 0 (mandatory). **macOS has no OS memory backstop**:
  Darwin does not enforce `RLIMIT_DATA` and the CI runner refuses to set it, so the setting is
  best-effort and reported as unavailable. Memory is bounded by Monty's `max_memory` and the
  `monty-alloc` hard limit, which hold while no escape has happened. Residual: a native escape
  on macOS can exhaust host memory. Stage 1b may add an optional daemon-side RSS watchdog that
  kills a worker above a ceiling.
- **Windows.** The daemon launches the worker **suspended**, with a restricted token
  (`CreateRestrictedToken`: privileged groups disabled, all privileges removed) at **Low**
  integrity, assigns it to a Job Object (memory limit at twice `script_memory_mb`,
  active-process limit 1, kill-on-job-close, UI restrictions), and only then resumes it. Its
  environment is empty and its working folder an empty low-integrity folder. It inherits exactly
  its two pipe handles: the launch uses `STARTUPINFOEX` with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
  naming only them (N6), so no other inheritable handle the daemon holds leaks into it.
  So that a Low process cannot read what the daemon and its sessions keep, the daemon sets one
  **inheritable `NO_READ_UP` mandatory label at Medium integrity** (object inherit and container
  inherit) at start on the **data folder**, the **config folder**, and each environment's
  **sessions folder and scratch folder** (N2). Inheritance covers every file and folder in them,
  including temporary files, renamed files and files created later: the endpoint file, the secret
  store, journals and their `.results/` folders, `history.json`, logs, memory, attachments, and
  the config files that hold MCP servers' environment variables and headers in plain text. The
  label is applied when a new environment is added. The daemon reads each folder's own label
  first and sets the inheritable label (and re-propagates it to existing children) only when it
  is missing or different, so a normal start costs a few reads. The worker's Low working folder
  is created outside the labelled folders (under the user's Low temporary folder) and carries an
  explicit Low label. If a file or folder in the labelled folders cannot be labelled (one the user
  does not own), code mode fails closed and the setting names the path. The daemon's named pipe
  keeps the default Medium label (`NO_WRITE_UP`), so a Low process cannot connect.
- **Linux and macOS: inherited descriptors.** The daemon holds descriptors that are not
  close-on-exec, and a child inherits them; an already-open descriptor stays usable after
  Landlock or `sandbox_init`, which only stop new opens. So before any confinement stage the
  worker closes every descriptor from 3 up (`close_range(3, ~0, 0)` on Linux, falling back to a
  loop up to `RLIMIT_NOFILE`; a loop up to `getdtablesize()` on macOS) and exits when that fails
  (F1). The daemon starts the worker in an empty folder of its own (mode 0700, removed with the
  run). The unsafe calls live in one opt-in module (`confine/fds.rs`).
- Everywhere: empty environment, empty working folder, and nothing inherited beyond the two
  pipes and stderr (Windows: the handle list above; Linux and macOS: the descriptor sweep above).

**Fail closed.** If any part of a platform's confinement cannot be applied (seccomp unavailable,
`sandbox_init` fails, the token, label or job call fails), the worker exits before reading a
program, or the daemon refuses to start it. Code mode is then **unavailable**: `run_script` is not
offered, and the setting shows why ("code mode needs seccomp, which this kernel does not offer").
Nothing runs with less confinement than this plan states.

**Pinning and supply chain** (F9). `monty`, `monty-proto`, `monty-types` and `monty-alloc` are
exact pins (`=x.y.z`) in the worker crate, all at one Monty release, recorded in
`runtimes/monty.env`. Cargo verifies every crates.io checksum against Imperium's `Cargo.lock`. A
release is adopted at least 14 days after publication, except a **security fix**, which is
adopted as soon as the stage 4 escape suite passes on it. Updates are deliberate pull requests
that rerun the escape suite. Imperium has no `cargo-deny` or `cargo-audit` today: stage 1 adds a
`deny.toml` (advisories, licences including Python-2.0 for Monty's bundled stdlib, and sources
limited to crates.io) to CI, and a weekly scheduled workflow that runs the advisory check and
lists new GitHub security advisories for `pydantic/monty`, opening an issue when one appears.
Monty's MSRV (1.98) is above the workspace's 1.89: the workspace `rust-version` moves to 1.98
(CI builds with stable and 1.99.1).

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
  `{"code": ...}` and `tool.after` with the result. A hook on `run_script` gates the program **as
  a whole** (block it, or rewrite the code, which then runs through the same checks); it does not
  see or govern the calls the program makes. Those reach hooks one by one as their own tools, with
  `parent_call_id` and `"from_program": true` added to the hook payload (F11), so a policy hook
  can treat calls from programs differently. It needs no approval of its own: the
  program has no capability, and every call it makes is judged. It is allowed in plan mode (its
  inner writes are refused by plan mode as direct ones are).
- **Rejected:** a second, "result" return channel (one rule is easier for a small model); per-tool
  typed Python stubs sent in the prompt (the cost reactive loading removes; `tool_search` already
  returns schemas); JSON-decoded return values (tools return text today, and guessing which
  outputs are JSON would make the same tool behave differently by output); one exception type per
  failure kind (Monty cannot define them, and the model rarely needs to tell them apart).

### C3. Where the interpreter runs, and how the engine drives it

- **Crates.** `crates/imperium-script` (daemon side) owns worker launch and confinement on
  Windows, the protocol driver, limits and the event allowlist, behind a trait the engine uses:
  `ScriptRunner::start(code, limits) -> ScriptRun`, where `ScriptRun::next()` yields `Print(text)`,
  `NameLookup(name)`, `Call { function, args, kwargs }` or `Done(Result<(), ScriptError>)`. Its
  answers are `resume_value`, `resume_name(Option<HostFunction>)` and `resume_error(message)`.
  Dropping a `ScriptRun` kills its worker (`kill_on_drop` signals and does not wait);
  `finish()` and the program's own end kill it and wait for the OS to confirm the exit.
  `next_until(deadline)` bounds the wait for the worker (F3, stage 2).
  `imperium-engine` depends on the trait only, and tests use a fake runner that replays scripted
  events, including events a real worker would never send. `imperium-core` builds the real
  runner and passes it in through the session's shared state, as it passes tool providers.
  `crates/imperium-script-worker` is the worker binary (C1).
- **Workers.** One per program (C1). At most `script_workers` (default 8) run at once across the
  daemon; a program that finds none free waits, and that wait counts against its wall budget.
  Workers are stopped when the daemon exits (process cleanup), and a test checks no worker
  outlives its program.
- **Events from the worker** (F7). At the pinned version a worker can send `Print`,
  `FunctionCall`, `OsCall`, `NameLookup`, `ResolveFutures`, `Complete`, `Error`, `TypingError`,
  `DumpResult`, `Ok`, `FatalError`, `ShutdownDump`, `Ping`, `SessionRefused` and `StreamedCalls`.
  The driver accepts only: `Print`; `FunctionCall` (C4's checks); `NameLookup` (C2's resolver);
  `OsCall`, always answered with an error (`OSError: not available in programs`), never with a
  value; `Ping` (answered with `Pong`); `Ok` only as the reply to `Configure`; and the
  turn-ending `Complete`, `Error`, `FatalError` and `SessionRefused`. Every other kind, any kind
  added by a later version (unknown `oneof` tags), and any event out of order is a protocol
  violation: the worker is killed and the program fails with "the interpreter misbehaved". The
  host sends only `Configure`, `Feed`, `ResumeCall`, `ResumeNameLookup`, `AbortFeed`, `Shutdown`
  and `Pong`; never `InstallDependencies`, `Dump` or `Load`. `Configure` never carries a
  `MountSpec`, an `OSAccess` handler or external modules, and type checking is off. Its OS
  policy keeps the worker-side clock and entropy (so `datetime.now()`, `time.time()` and `random`
  work without suspending) and sets the sleep mode to `zero`: `time.sleep()` returns at once,
  since a program has nothing to wait for (tools are synchronous). `max_total_sleep` is 0. A
  `system_sleep` or any other `OsCall` that still arrives is answered with an error, like every
  `OsCall`.
- **Suspension budget, enforced by the driver** (N1). `monty.proto` makes `max_suspensions` and
  `max_total_sleep` the client's to enforce; the worker only echoes them. The driver therefore
  counts every suspension it answers (`FunctionCall`, `NameLookup` and `OsCall`) against
  `script_suspensions` (C5), and at the first one over the budget sends `AbortFeed` instead of an
  answer, so the program ends with "the program made too many requests". Tool calls are also
  counted separately against `script_calls`.
- **The driver** (`agent/script.rs`, engine). It runs inside `run_tool_once`'s `run_script` arm,
  on the agent's task:
  1. Save the agent's per-call state (below) and set `in_program`, both through an RAII guard
     (N7), then start the program with the limits (C5).
  2. Loop on `next()` (each event first counted against the suspension budget, N1):
     - `NameLookup(name)`: resolve through C2 and C4. A callable tool gets a host function whose
       identity the driver records in a **handle table** (function name to engine tool name).
     - `Call { function, .. }` (F1): the worker is untrusted, so the driver trusts nothing it
       says. The function must be in the handle table; otherwise the call is refused as
       "not a function this program was given". Then the stop flag and the call budget are
       checked, an inner `ToolCall` is built with id `<outer id>:<n>` (1-based), and it runs
       through `self.run_tool(&inner)` (the whole path of a direct call). Inside, after
       `unwrap_call`, `run_tool_once` checks the **effective** name against the script-refused
       set and the universe whenever `in_program` is set, before every other check. So a
       lookup-free call, a forged handle, a `call_tool` wrapper or a tool that left the universe
       since its lookup are all refused at dispatch. The driver then resumes with the text, or with
       the error.
     - `Print`: append to the bounded buffer.
  3. On `Done`, assemble the result (C2), let the guard restore the state, and return the
     result through `tool_finished` for the outer call.

  The six per-call fields, `in_program` and the program's remaining approval-wait budget live in
  one `Arc<CallState>` on the agent. The guard holds its own clone of that `Arc`, so its `Drop`
  restores the fields without borrowing the agent across `self.run_tool(&inner)`. It restores the
  six saved fields and clears `in_program` when it is dropped, so it happens on every exit path: `Done`, a stop, a budget, a worker error, a protocol violation and
  the outer future being dropped mid-program (N7). Without it, a cancelled program would leave
  `in_program` set and the agent's next direct `spawn_agent` would be refused.
- **Per-call state around inner calls** (F2). `run_tool` and the turn loop keep per-call state on
  the agent: `pending_rewrite`, `rewritten_from`, `ran_rewritten`, `call_started`,
  `call_args_failed` and `last_tool_ok`. The driver saves all six and clears them before the
  first inner call, lets each inner `run_tool` set and read them as a direct call does, reads
  `ran_rewritten` and `last_tool_ok` after each inner call itself (to collect touched directories,
  C6), and restores the saved values before the outer `tool_finished`. So an outer rewrite (a
  hook rewrote the program) and an inner rewrite are both honoured and neither shows in the
  other's result. Schema advice (item 61) for an inner call is added to the error the program
  sees, but the tool is not added to `schema_seen`, because the model never saw that text.
- **Inner calls are sequential.** The dispatch takes `&mut self`, so one inner call at a time is
  sound. No async host functions are offered (`asyncio.gather` over tools suspends on each call
  in turn; `ResolveFutures` and `StreamedCalls` are protocol violations). Revisit with R2.9.
- **Waiting on approvals** (F8, N4). A worker whose program waits on an approval stays alive and
  idle (its execution clock is paused) and holds one of the `script_workers` slots. It is not
  freed and later restored: that would mean loading a snapshot, which Monty says to trust only
  from a trusted producer, and the worker is not one. The wall budget excludes approval waits, so
  it does not bound them. Instead each program has a total approval-wait budget,
  `script_approval_wait_secs` (default 600 s, C5). It is enforced inside `approve_with`, not in
  the driver (which is blocked inside `run_tool` while an approval waits, D1): when `in_program`
  is set, `approve_with` races the broker's answer against the program's remaining wait budget,
  held in the shared call state. If the budget runs out first, it calls the new
  `PermissionBroker::cancel(request_id)`, emits `PermissionWithdrawn { request_id, reason }`
  ("the program waited too long for approval"), and returns a denial; every later approval the
  program needs is denied at once without a request. An answer that arrives after the withdrawal
  is reported to its client as expired. So a slot is held by approval waits for at most ten minutes
  per program; eight programs waiting together can still hold every slot for that long, and a
  ninth program waits for a slot (that wait counts against its wall budget). The desktop shows the
  waiting approvals. The driver's per-turn worker timeout (`request_timeout`, C5) runs only while
  the worker is executing, never while the host is answering.

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
  `load_skill`, `activate_plugin`, `context_load`;
- memory writes: `memory_remember`, `memory_edit`, `memory_forget` (S3), so every change to
  memory is a visible step in the conversation.

Reasons: a program's results are hidden from the conversation, so anything whose effect is meant
to be seen by the model or the user in the conversation (a plan, a loaded skill, a question)
belongs outside it. A subagent started from a program would outlive the program's budgets.
Activating a plugin would change the offered list mid-turn. Everything else is callable: file
tools (reads and writes), `run_command` (foreground and background) with `command_output` and
`command_stop`, web, browser and media providers, `memory_search` and `memory_read`, MCP tools and
activated plugins' tools.

A refused name resolves to a function that raises `RuntimeError("<name> cannot be called from a
program; call it directly")`, so the model learns why. That is only for the model's benefit: the
same set and the universe are checked again at dispatch on every call (C3, F1). Every other check
(fork, launcher, named-agent list, profile, permission rules, hooks, approvals) happens in
`run_tool` as for a direct call.

**Approvals inside a program.** An inner call that asks blocks the program like a direct call
blocks the turn. The interpreter's time budget is paused while the host answers (Monty's feed
clock), and the wall-clock budget excludes time spent waiting for the user (C5). The approval
summary is prefixed "From a program: " so the person knows the call came from code. It composes
with the MCP bridge's marker (`bridge::SUMMARY_PREFIX`) by going after it; programs are not
offered through the bridge (C8), and a test pins the order should that change. In an unattended
session, pre-approved tools run and a floored call waits, as today. In a headless run nobody
answers, so the call is denied, the program gets `RuntimeError("The user denied this action.")`,
and it can carry on.

**Auto mode is stricter for inner calls** (F6). Inside a program, values from earlier tool
results can flow into a later call's arguments without the model ever reading them (a path or
URL taken from a file, a command built from a search result). So in auto mode an inner call of
kind Execute or Edit, or **any call to a provider or MCP tool** (a `ToolGroup::Builtin` provider
or a `ToolGroup::External` server, activated plugins' tools included; "reaches the network" is
not judged per tool, N5), is **not** settled by the classifier: it is decided as in `default`
mode (a rule or a session allowance, otherwise the user is asked; headless, denied). The check
sits in `auto_gate`, beside the existing `bridged` check, and returns `AutoGate::NotApplicable`
for those calls while `in_program` is set, so the ordinary approval path runs. Inner reads are classified as direct ones are, and the
classifier's input then also carries the program's code (cut to 4,000 characters) as data.
Why not classify them: the classifier sees the code and the final arguments, but not where an
argument's value came from, and that is exactly what an injection controls.

### C5. Budgets and cancellation

New `[harness]` keys, defaults chosen for a small local model; all are per program:

| Key | Default | Enforced by |
| --- | --- | --- |
| `script_calls` | 40 inner tool calls | Driver counts `FunctionCall` events that run a tool |
| `script_suspensions` | 200 suspensions of any kind | Driver counts every `FunctionCall`, `NameLookup` and `OsCall` and sends `AbortFeed` past it (N1); also sent to the worker as `max_suspensions` |
| `script_approval_wait_secs` | 600 s in total per program | Driver; past it the pending approval and every later one in the program are denied (N4) |
| `script_exec_secs` | 10 s of interpreter time | Monty `max_feed_duration` (paused during host calls) |
| `script_wall_secs` | 300 s, excluding time waiting on approvals | Engine deadline, checked between events |
| (fixed) `request_timeout` | `script_exec_secs` + 5 s per worker turn | Driver timeout from each request it sends to the worker's next event; a worker that does not answer is killed (F8) |
| `script_memory_mb` | 256 MB | Monty `max_memory` in the worker, plus the OS limit at twice this as backstop (job on Windows, `RLIMIT_DATA` on Linux and cgroups when delegated; none on macOS, see C1, F10) |
| `script_workers` | 8 workers at once, daemon-wide | `imperium-script` (C3) |
| `script_output_chars` | 6,000 | Engine print buffer (C2) |
| `script_code_chars` | 20,000 | Engine refuses longer programs before starting a worker (compilation is not charged to the time budget) |
| (fixed) inner result size | 4 MB | Engine raises `RuntimeError` instead of passing a larger text into the program |

- **Stop.** Before each inner call and on each event, the driver checks the session's stop flag.
  If stopped, it drops the run (killing the worker) and returns the output so far with "the
  session was stopped; the program did not finish". This matches `Session::stop`: a tool call in
  flight finishes, nothing new starts.
- **Turn cancellation and errors.** Dropping the outer future drops the `ScriptRun`, which kills
  the worker. A crashed worker returns "the program's interpreter stopped unexpectedly". Every
  worker is used once, so there is nothing to replace.
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
| Sandbox escape from the interpreter (VM bug) | Monty's language-level sandbox; a separate worker process per program; OS confinement applied before any program is read, failing closed (C1); pinned versions under Imperium's lock, escape suite on every update | Linux and macOS: an escaped worker can do little beyond its pipes (no file opens or reads, no network, no processes); on macOS it can still exhaust host memory, which has no OS limit (C1). Windows: see [below](#windows-confinement-mitigation-and-accepted-risk) |
| A lying worker (forged calls, unknown events) | The driver trusts no worker output: every call must use a handle it issued and is re-checked against the script-refused set and universe at dispatch (C3, F1); events outside the allowlist kill the worker (F7) | None beyond the dispatch rules |
| Capability through the bindings | Every inner call is `run_tool`: same refusals, rules, hooks, approvals as direct calls; script-refused set (C4) | A program chains read-then-send in one model step (read a secret, `web_fetch` a URL with it in the query). Today the model can do the same in two steps; code mode removes the pause between them, not a check. Auto mode does not settle inner sends, writes or commands (C4); the desktop shows each inner call live |
| Prompt injection from tool results | **The control is per-call dispatch**: whatever a program does, each call it makes is judged on its own name and actual arguments by the same rules, hooks and approvals as a direct call, and the stricter auto-mode rule for inner sends, writes and commands (C4). Removing `eval` and `exec` (A2) is hygiene, not a boundary: without them a program can still pass tool output on as data. The printed output enters the context as a tool result with the existing untrusted-data framing | **Data to arguments**: a value from one result (an injected URL, path or command) can become a later call's argument without the model reading it. Each such call is still judged, and in auto mode it is asked rather than classified, but a person approving it sees only the final arguments. The tests that a renamed or aliased tool cannot bypass a refusal stay (stage 2, stage 5) |
| Resource exhaustion | C5 budgets; memory limit in the worker plus an OS limit (Linux, Windows; none on macOS, C1); at most `script_workers` workers; code length cap; output cap; `request_timeout` | Many programs at once queue for workers, bounded by each wall budget |
| Secrets | Worker environment cleared, so no `GH_TOKEN`, API keys or daemon token; no file access on Linux and macOS; on Windows an inheritable `NO_READ_UP` label on the data, config, sessions and scratch folders; files reach the program only through `read_file` and its path checks | Windows: user files outside those folders are readable after an escape, and the OS keyring depends on the stage 1 test (below) |
| Network | None inside the interpreter; Linux and macOS confinement deny it; web tools go through their own rules | Windows: an escaped worker can open connections (below) |
| Denial through approvals | A program that asks 40 times is bounded by `script_calls`, and a denial is an ordinary error the program sees | The user sees up to 40 prompts. Approve-for-session works as today |

#### Windows confinement: mitigation and accepted risk

Mitigation (C1, root decisions on F3 and N2): a restricted, Low-integrity token, so the worker
cannot write the user's files, registry or medium-integrity processes; a Job Object (memory, one
process, kill on close, UI limits); an empty environment and folder; only its two pipes inherited
(N6; on Linux and macOS the worker closes every other descriptor, F1); and one inheritable Medium `NO_READ_UP` label on the data folder, the config folder and
every environment's sessions and scratch folders. Everything Imperium keeps there is unreadable to
a Low process whatever its name or age: the endpoint file and token, the encrypted secret store
and its key, journals, results, history, logs, memory, attachments, and the plaintext MCP
environment variables and headers in the config files. The daemon's named pipe keeps its Medium
`NO_WRITE_UP` label, so a Low process cannot connect, and the loopback listener needs the token it
cannot read.

**The OS keyring.** On Windows the keychain backend is Credential Manager. Stage 1 tests whether a
Low worker can `CredRead` an Imperium entry. If it cannot, the keyring is covered. If it can,
secrets in the keyring are part of the accepted residual below, accepted **only while
`harness.code_mode` is off by default**: the stage 6 default change is then blocked until the
secrets are moved out of reach of Low processes (for example into the labelled encrypted store) or
the worker moves to an AppContainer, and 13 records this revisit.

What remains: an escape from the language-level sandbox on Windows is a **user-level read
compromise**. The escaped code could read any file the user can read outside the labelled
folders (the user's documents and projects, SSH keys, browser profiles, other tools' tokens) and,
subject to the keyring test, Imperium's keyring entries, and send it over the network; and it
could use CPU and memory up to the job's limits. It cannot write the user's files, start
processes, read anything in Imperium's labelled folders, or drive the daemon. The risk is accepted
because:

1. An escape needs a memory-safety bug in Monty's VM followed by native code execution. Monty has
   no C dependencies and no FFI, implements no file, network or process operation, and has a
   private-disclosure and bounty programme. The stage 4 escape suite and the update rule test the
   boundary directly.
2. Without an escape the program has no capability at all, so the residual applies only to a
   chained exploit, not to ordinary misbehaviour or injected text.
3. The mode is off by default until stage 4 passes (S2), then on only for local models (S1).
4. Stage 1 tests the token, the job and the labels (below), and the worker fails closed when any
   of them cannot be applied.

Revisit (an AppContainer profile with no network or file capabilities, or Monty's WebAssembly
runtime under wasmtime on Windows) if the stage 4 review finds a practical escape, if a Monty
escape advisory is published, or when 05's AppContainer work lands for commands.

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

- **A1.** The worker's confinement is applied in the order C1 gives on each OS: seccomp and
  Landlock before the first read from stdin on Linux, `sandbox_init` before it on macOS, and on
  Windows the job and token before the suspended process resumes. The seccomp allowlist fits the
  `monty-proto` worker loop (no file opens after start, including for time zones: the `worker`
  feature bundles the tz database). If a needed system call is found, it is added only if it opens
  nothing.
- **A2.** `eval` and `exec` can be removed from a program's builtins (a Monty option or a name
  policy). If not, the engine refuses a program whose source uses either name outside a string
  literal, before it starts (a token scan with Monty's parser, or a conservative regex), and the
  refusal says why. This is hygiene (C9), not a boundary.
- **A3.** On Windows, `CreateProcessAsUserW` with a restricted Low token derived from the daemon's
  own token, created suspended and assigned to the job before it resumes, works for an
  unprivileged user and keeps the stdio pipes; `SetSecurityInfo` with a `NO_READ_UP` Medium label
  works on files the daemon owns.
- **A4.** A fresh confined worker starts and answers its first `Configure` in under 50 ms on each
  OS (cold start, F5), and the binary is under 20 MB, with and without `monty-type-checking` (F12).
  Stage 1 records the measurements here. If cold start fails by more than 2×, the one-ahead
  spawned worker (C1) is turned on; if size fails, the type checker is left out.
  Stage 1a measured: cold start 0.8 ms (Linux x86-64) and 1.7 ms (Linux aarch64); on macOS a
  median of about 20 to 31 ms with a maximum up to 56 ms; all under the 50 ms target on the
  median. **Decision: the one-ahead spawn is dropped on every OS.** The target is a median, the
  worst macOS start is once per program and invisible beside a model turn, and a spare idle
  worker costs a process and a slot. Stage 6 re-measures real programs and may revisit it for
  macOS only. Size 40 to 43 MiB (fails; the owned `MontyRun` loop in 1b removes the type
  checker, see the stage 1a Outcome).
- **A5.** Qwen3.5-4B writes short Python programs that stay inside Monty's subset often enough.
  Stage 6 measures syntax, `NotImplementedError` and `NameError` rates.
- **A6.** The guided-harness pilot's failures on aggregation are reachable by code: the knowledge
  base is files the program can `read_file` or `grep`, and the counts follow from them. True for
  the dev and hard suites (gold answers are computed from the structured world).

## Relevant paths

`crates/imperium-script/` and `crates/imperium-script-worker/` (new), `deny.toml` (new), `crates/imperium-engine/src/agent/{script,tools,turn,catalog,prompts,fork}.rs`,
`crates/imperium-engine/src/{agent.rs,session.rs,spill.rs}`, `crates/imperium-engine/src/agent/auto.rs`,
`crates/imperium-protocol/src/{tools,session,lib}.rs`, `crates/imperium-protocol/schemas/`,
`crates/imperium-config/src/{settings,registry}.rs`, `crates/imperium-core/src/{eval.rs,eval/variant.rs,state/create.rs}`,
`crates/imperium-core/src/` (runner construction, file labels), `crates/imperium-cli/src/{eval.rs,eval/,chat.rs}`,
`crates/imperium-cli/tests/headless_eval.rs`, `apps/desktop/src/sessions/`, `apps/desktop/src/panels/SessionPanel.tsx`,
`scripts/ci/build-desktop.sh`, `.github/workflows/{ci,build,advisories}.yml`, `runtimes/monty.env` (new).

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

- Objective: C1 and C3's crates, with the worker built and shipped on every target, confined on
  each OS, and failing closed; A1 to A4 settled and recorded here.
- Changes: `runtimes/monty.env` (pinned versions); `crates/imperium-script-worker` (the binary:
  `monty-alloc` allocator, self-confinement on Linux and macOS, the `monty-proto` worker loop);
  `crates/imperium-script` (the `ScriptRunner` trait, the protocol driver with the event allowlist,
  the Windows suspended launch with token, job and labels, the `NO_READ_UP` labelling of the
  daemon's files, one worker per program (no one-ahead spawn: dropped, F5), `script_workers`,
  kill-on-drop, and `finish()` with confirmed exit, worker path discovery beside the daemon binary, the
  fail-closed availability check and its reason) and a fake runner for engine tests;
  `scripts/ci/build-desktop.sh` builds the worker with the daemon per target and adds it to
  `externalBin` as `imperium-script`; the CLI package and the server image (item 63, if merged)
  carry it; `deny.toml` and a `cargo deny check` CI job; the weekly advisory workflow; workspace
  `rust-version` 1.98.
- Tests (`crates/imperium-script/tests/`, real worker, all three OSes unless marked):
  - Interpreter: a program prints; suspends at a host call and resumes with the value; a name
    lookup resolves to a host function and an unknown name raises `NameError`; `open()`,
    `import os`, `os.environ`, `os.getenv("PATH")`, `socket`, `subprocess`, `__import__` and (per
    A2) `eval`/`exec` fail, and the `OsCall` behind `open()` and `os.getenv` is answered with an
    error by the driver.
  - Limits: `while True: pass` ends on the time budget; `x = [0] * 10**9` ends on the memory
    budget; a program looping on lookups of an undefined name, and one looping
    `try: open("x") except OSError: pass` (an `OsCall` loop), are each stopped by the driver's
    `AbortFeed` at `script_suspensions`, not by the worker (N1); `time.sleep(5)` returns at once
    and the program completes; `datetime.now()` and `random.random()` work; a worker that stops
    answering is killed at `request_timeout`.
  - Protocol: a test worker build (`cfg(test)` feature) that sends `ResolveFutures`,
    `StreamedCalls`, `DumpResult`, `TypingError`, `Ok` outside `Configure`, an unanswered
    out-of-order event and an unknown tag each gets killed and the program fails as a protocol
    violation. If stage 1 chooses the owned loop over `monty::MontyRun` (F12), the same allowlist
    tests run against its own event set.
  - Processes (F4, F5): two programs run in workers with different PIDs; dropping a run
    mid-program leaves no worker process (PID checked after `finish()`'s confirmed exit, and shortly after a plain drop); a killed worker
    is reported as crashed and the next program runs; shutdown stops every worker; the worker's
    environment is empty; a long simulated approval (the host holds a `FunctionCall` for 20 s,
    beyond `request_timeout`) then resumes and the program completes (F8).
  - Confinement, Linux (x86-64 and aarch64 runners, N3): the worker cannot `open` a file it would
    otherwise read, cannot create a socket, cannot `execve`, cannot `kill` another process id;
    `clone3` returns `ENOSYS` and threads still start; an x32 system call is rejected (x86-64);
    `getrandom` works; `RLIMIT_DATA` is set.
  - Confinement, macOS: the worker cannot read a file in the user's home folder or the daemon's
    data folder, cannot connect to a loopback port, cannot spawn a process, and cannot reach the
    pasteboard or any other `mach-lookup` service (N3).
  - Confinement, Windows: the worker runs at Low integrity with no privileges; a write to the
    user's profile fails; a read of a file labelled `NO_READ_UP` (a fixture standing in for
    `daemon.json` and `secrets.enc`) fails; it cannot open the daemon's named pipe; its job
    refuses a second process; after start, the data, config, sessions and scratch folders carry
    the inheritable label, and a file created afterwards in each (and a file renamed into one) is
    unreadable to a Low process (N2); a second start does not re-propagate (labels unchanged,
    no child rewritten); a fixture file the daemon cannot relabel makes code mode unavailable
    with that path as the reason; the worker's working folder is outside the labelled folders and
    labelled Low; the worker holds no handle beyond its two pipes (an
    inheritable test handle opened by the daemon before launch is absent in the worker, N6); a
    Low worker's `CredRead` of an Imperium Credential Manager entry is tested and its outcome
    recorded in C9 (N2).
  - Fail closed (F10): with confinement forced to fail (a test hook on each OS), no program runs,
    `run_script` is reported unavailable, and the reason names what failed.
  - Measurements (A4, F12): cold start, binary size and resident memory with and without
    `monty-type-checking`, logged and recorded in this plan.
- Docs: 05 gains *Programs (`run_script`)*, with the interpreter and confinement subsection
  (implemented part only); 11 lists the worker build; 13 gains the next free decision number (C1).
- Depends on: none. Size: about 1,100 lines plus CI, so two pull requests: 1a (worker crate,
  protocol driver, event allowlist, Linux and macOS confinement, supply-chain checks and their
  tests) and 1b (the Windows launch, job, token and labels, the fail-closed check, bundling, and
  the measurements).
- Owned paths: `crates/imperium-script/**`, `crates/imperium-script-worker/**`, `Cargo.toml`,
  `Cargo.lock`, `deny.toml`, `runtimes/monty.env`, `crates/imperium-core/src/` (labelling the
  daemon's files at start), `scripts/ci/build-desktop.sh`,
  `.github/workflows/{ci,build,advisories}.yml`,
  `docs/05-tools-and-isolation.md`, `docs/11-ci-cd.md`, `docs/13-decisions.md`, this plan.
- Risks: a target where the pinned version does not build (Windows arm64 is the likeliest), or
  where confinement cannot be applied; then code mode is unavailable on that target and the
  setting says so (fail closed), rather than blocking the stage.

### Stage 1a: Status and Outcome

Status: built in pull request #303 (`feat/code-mode-1a`), default-off and unreachable: there is
no `run_script` tool and no setting yet.

Built: `crates/imperium-script-worker` (Monty 1.6.0 pinned exactly in `runtimes/monty.env` and
the lock; the worker loop; self-confinement before any request is read, failing closed),
`crates/imperium-script` (the driver: event allowlist, suspension budget with `AbortFeed`,
`OsCall` answered with an error, `request_timeout`, kill on drop), `deny.toml`, the `deny` CI job,
the weekly advisory workflow and the Monty pin check, and a `script-worker` CI job on four OSes.

Confinement per OS:

| OS | Confinement | State |
| --- | --- | --- |
| Linux (x86-64, aarch64) | `PR_SET_NO_NEW_PRIVS`, `RLIMIT_DATA` at twice the memory budget, Landlock (no file access) where the kernel has it, seccomp allowlist (`clone3` gives `ENOSYS`, x32 rejected, other architectures killed) | built and tested in CI |
| macOS | `sandbox_init` from `(deny default)`; `RLIMIT_CORE` 0 (must hold); `RLIMIT_DATA` best-effort | built and tested in CI; see the note below |
| Windows | none: the worker refuses to run | stage 1b |

macOS note: Darwin does not enforce `RLIMIT_DATA` and the release on the CI runner refuses to set
it (`EINVAL`), which failed all three confinement tests. It is now best-effort and reported as
`rlimit_data=unavailable (...)`; the memory bound on macOS is Monty's `max_memory` and the
`monty-alloc` hard limit inside the worker. The core-dump limit and the sandbox profile remain
mandatory stages.

Findings that refute or change assumptions:

- **A4 failed on size.** The release binary is 40 MiB on Linux aarch64 and 43 MiB on Linux
  x86-64, against the target of under 20 MB. Monty's `worker` feature forces in the type checker
  (ruff and ty) and `zip` 8.6.0, which carries two RUSTSEC advisories (0546 and 0547) that `deny.toml`
  ignores with reasons; `salsa` is pinned to 0.28.2. The other six zip ignores (0367, 0368,
  0369, 0429, 0545, 0548) are **pre-existing and not Monty-related**: they affect zip 4.6.1, used
  by `imperium-local` and `tauri-plugin-updater` (0545 also zip 0.6.6, via `monty-typeshed`).
  Their follow-up, a separate pull request, is to upgrade those to zip >= 9.0.5. Resolution: stage 1b builds the owned `monty::MontyRun`
  loop (F12) instead of `monty-proto`'s `worker` feature, which drops the type checker and `zip`,
  and then removes the two Monty-only ignores (0546 and 0547). The six pre-existing ones stay
  until the zip upgrade. Until then the ignores are reviewed by the weekly advisory workflow.
- **A2: `eval` and `exec` exist** and run inside Monty. The engine must refuse programs that
  name them (stage 1b or 2 requirement, with a test). They add no host capability, since every
  host effect still passes through the driver, but they defeat any static check of what a
  program calls. The refusal is hygiene, not a boundary (`getattr` exists).
- **Bare-name calls arrive as `FunctionCall` with no `NameLookup`.** The F1 handle table
  therefore cannot be the only gate: the engine must resolve the name and re-check it at call
  time (`in_program`) on every `FunctionCall`, not only on lookup.
- **A4/F5 cold start is 0.8 ms (Linux x86-64) and 1.7 ms (Linux aarch64)**, resident memory 10
  to 12 MiB, against a 50 ms target. The one-ahead spawned worker is dropped from the plan.

Moved to stage 1b (the plan permits each: stage 1a's scope above lists the worker crate, the
driver, the event allowlist, Linux and macOS confinement and the supply-chain checks):

- The `ScriptRunner` trait and the fake runner. They exist for the engine's tests (stage 2) and
  are best shaped together with the owned `MontyRun` loop, which changes the driver's event set;
  building them on the `worker`-feature protocol now would be rewritten.
- The cgroup v2 memory limit. C1 makes it conditional on a delegated cgroup, so it is an extra
  behind `RLIMIT_DATA` and Monty's own limit, and it is not needed to meet a 1a test.
- The Windows launch, token, job and labels, the fail-closed availability check, bundling in
  `build-desktop.sh`, and the measurements for the shipped (type-checker-free) binary.

Review fixes folded into 1a (accepted from the review of #303):

- **F1, inherited descriptors.** The worker closes every descriptor from 3 up before any
  confinement stage and fails closed (`confine/fds.rs`); the driver starts it in an empty
  folder. Tests: a descriptor opened without close-on-exec is open in the probe before the sweep
  and gives `EBADF` on `fstat` after it; a forced `fds` failure exits with the confinement
  status; a worker started by the driver finds its working folder empty.
- **F3, wall budget.** `ScriptRun::next_until(deadline)` and `ScriptError::WallBudget`; a test
  that a silent worker is cut at the deadline, not at the longer `request_timeout`.
- **F3 follow-up.** The deadline is checked before every read and bounds every write, so a worker
  that keeps writing frames, or stops reading, is still ended as `WallBudget`; a flood-of-prints
  test covers it.
- **Seccomp.** `prlimit64` and `sched_getaffinity` only for pid 0 or the worker's own id.
- **`--memory-mb`** has a floor of 32.
- **Driver tests added:** an oversized event frame; a call left unanswered when the next event
  is asked for.
- Plan text: F2 (zip ignores), F4 (macOS memory), F5 (macOS cold start, one-ahead spawn dropped).

Stage 1b changes from the findings: build the owned `MontyRun` loop and re-run the allowlist tests
against its own event set; refuse programs that name `eval` or `exec` (hygiene, not a boundary: `getattr` reaches the
same names, and every host effect is still judged per call); re-check every
`FunctionCall` name against the program's handle table at call time; no one-ahead spawn.

## Stage 2: The tool, the driver and dispatch

- Objective: C2 to C5 for the root and plain subagents, behind `harness.code_mode` (default
  `off`, hidden from the settings UI until stage 3).
- Changes:
  - Protocol (next free minor at merge): `run_script()` `ToolDefinition` in `tools.rs` (exported
    to `schemas/tools/`); `ToolStarted.parent_call_id`; the additive event
    `SessionEvent::PermissionWithdrawn { request_id, reason }` (D1); the 02 version paragraph.
  - Broker: `PermissionBroker::cancel(request_id)`, after which `session.respond` for that id
    returns an "expired" error.
  - Config: `HarnessSettings.code_mode: HarnessMode` (default `Off`) and the C5 budget keys.
  - Engine: `agent/script.rs` (the driver, resolver, callable set, script-refused set, argument
    mapping, output assembly, summary line, stop and budget checks); the `run_script` arm in
    `run_tool_once` with `tool.before`/`tool.after` on the code; the handle table and the
    `in_program` check on the effective name in `run_tool_once`; inner call ids and
    `parent_call_id`; `parent_call_id` and `from_program` in inner hook payloads; saving, clearing
    and restoring the six per-call fields; schema advice kept out of `schema_seen` for inner
    calls; touched directories collected by the driver; `run_script` in the offered list per C7 (core when reactive); the code-mode
    sentence as its own prompt region; the "From a program: " approval prefix after the bridge
    marker; auto mode deciding inner Execute, Edit and network calls as `default` mode, and its
    input for inner reads carrying the code; `run_script` in `spill::EXEMPT_TOOLS` (if R2.5 is merged).
- **Wall budget (F3).** `ScriptRun::next()` answers `OsCall`s and pings inside one call, and each
  worker read is bounded only by `request_timeout`, so the per-read timeouts add up and a stream
  of internal answers could hold a worker slot for tens of minutes. The engine therefore calls
  `next_until(deadline)` (added in stage 1a) with the program's wall deadline, moved forward by
  the time spent waiting on approvals, or wraps each call in `timeout(remaining_wall, ...)`; the
  former also kills the worker and ends with `ScriptError::WallBudget`. A test with a worker that
  sends endless pings or `OsCall`s checks the program ends at the wall budget.
- Tests (engine, fake runner unless marked):
  - Off mode: the tool list and prompts equal the item 61 off-mode snapshot (goal 6).
  - Dispatch twins (goal 2), each a copy of an existing direct-call test through a program: a
    profile refusal; a named-agent refusal; a universe refusal (reactive on); a permission-rule
    refusal; plan mode refusing an inner `write_file`; a `tool.before` hook that blocks an inner
    call (the program sees `RuntimeError`) and one that rewrites it (the same tool runs with the
    new arguments, and the outer `run_script` result carries no rewrite note); an inner hook
    payload carries `parent_call_id` and `from_program`; approval asked for an inner
    `run_command` with the prefixed summary, then allowed, then denied; the prefix composed with
    the bridge marker in that order; in auto mode an inner `run_command`, `write_file` and
    `web_fetch`, and an MCP tool and a plugin tool, are asked (headless: denied) rather than
    classified, with `auto_gate` returning `NotApplicable` for them, while an inner `read_file`
    is classified with the code in the classifier's input (N5); pre-approved tools in an unattended
    session run without asking.
  - Untrusted worker (F1, fake runner): a `Call` for `spawn_agent` with no prior lookup is refused
    and `spawn_agent` never runs; a `Call` naming a handle that was never issued is refused; a
    `Call` through a handle for `call_tool` whose inner name is `ask_user` is refused after
    unwrapping; a tool looked up, then removed from the universe (profile change), then called is
    refused at dispatch.
  - Guard (N7): a session stopped mid-program, a worker error, a protocol violation and a dropped
    outer future each leave `in_program` cleared and the six fields restored; after a stop
    mid-program, the agent's next direct `spawn_agent` is not refused.
  - Approval wait cap (N4, D1): with `script_approval_wait_secs` at 1 s, an inner approval left
    unanswered is denied after 1 s by `approve_with`; a subscribed client receives
    `PermissionWithdrawn` with the request id; a late `session.respond` for it is reported as
    expired; and the program's next call that asks is denied at once with no request.
  - Per-call state (F2): a `tool.before` hook rewrites the program, and inside it a second hook
    rewrites an inner call; the inner call runs rewritten, the outer result has exactly one
    rewrite note (the program's) and the summary line none; after the program the turn loop sees
    the outer call's `ran_rewritten` and `last_tool_ok`; an inner failed call with schema advice
    leaves `schema_seen` unchanged.
  - Each script-refused tool raises with the "call it directly" message and never reaches
    dispatch; an unknown name raises `NameError`; `tool("mcp-style.name", ...)` calls a provider
    tool with a dotted name; a renamed or aliased spelling of a refused tool is still refused.
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
    rows and prints a count gives the right number, with the inner calls in events; a program
    whose inner `write_file` waits 20 s on an approval, then is allowed, completes (F8).
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
  beyond a test; the desktop and CLI `chat` remove a withdrawn approval prompt on
  `PermissionWithdrawn` and show its reason (D1); the desktop's nested program card (code, live inner rows, output, summary) and
  CLI indentation; journal replay rebuilds nested cards from `parent_call_id`; the registry entry
  `harness.code_mode` ("Let agents write programs that call tools") and the 10 rows for it and the
  budgets.
- Tests: a subagent with a local model gets `run_script` and its program cannot call a tool its
  profile forbids; a named agent without `run_script` in its list is not offered it, and one with
  it cannot call an unlisted tool from a program; a question fork's program reads a file and is
  refused `write_file`; an initiative review refuses `run_script` with the review's message
  (`crates/imperium-core/tests/`, the initiative tests' daemon harness); a background command
  started from a program is listed by `command_output` after the program ends and its completion
  notice arrives; a resumed session renders the nested cards; a withdrawn approval disappears
  from the desktop's approvals panel with its reason, and the CLI prints it; compaction's transcript shows
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
- Work: a security reviewer (not the implementer) reviews the worker confinement on each OS
  (the seccomp filter, the macOS profile, the Windows token, job, handle list and labels), the
  protocol driver and its event allowlist, the worker loop (upstream's `monty_proto::worker`, or
  the owned loop over `monty::MontyRun` if stage 1 chose it, F12), the dispatch twins, the
  resolver, the suspension and approval-wait budgets, the RAII guard and the approval prefix, and
  tries an escape suite.
  Findings are fixed in this stage's pull request.
- Pass criteria (recorded in this plan; S2): no open finding that lets a program reach a file,
  the network, a process or a refused tool except through dispatch; the Windows accepted risk
  re-read and confirmed by the reviewer; the fail-closed paths exercised on each OS.
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
  permission mode** (the product default, where the shell is denied), plus the new aggregation
  suite below, as four arms:
  `code_mode` off and on, each with `tool_loading` at its released default and at `on`. Add a
  `harness_code` arm to `imperium_backend.py`/`arms.py` that passes `--flag code_mode=...`; the
  task prompts are unchanged (no "compute with the shell" policy). Record: accuracy (strict
  scorer), prompt and cached tokens per item, model calls per item, programs per item, inner calls
  per program, program outcomes (done, Python error by type, budget, crash), `NotImplementedError`
  and `NameError` rates (A5), and wall time. Report in Imperium-Research beside the pilot.
- **Aggregation suite (S4)**, built first, in Imperium-Models
  `experiments/guided-harness-pilot/suite/` beside the existing builders (`build_aggregate.py`,
  `aggregate_tasks.jsonl`, `aggregate_kb/`). Generated from a seeded structured world: tables of
  60 to 400 rows written as CSV files and as one-fact-per-line text files (both forms, so a
  program has to parse either); 60 items over counts with two or three conditions, grouped sums
  and means, top-k by a computed value, joins across two tables, and unit or currency conversion
  over many rows. Gold answers are computed by the builder script from the structured world, never
  written by hand or by a model, and `validate_suite.py` checks each gold answer is unique and
  reproducible from the files. Its items are too large to answer by reading every row within the
  20-step limit, so it separates computing from reading. The suite is evaluation data, not
  training data.
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
  - on the aggregation suite, code mode on scores at least 15 items (of 60) above code mode off;
  - at most 20 % of programs end in a Python error, and none in a crash.
- Then, in one small pull request: the default becomes `auto` (settings default, registry text,
  10, 03, roadmap item 65 marked implemented with the measured numbers, this plan's status). If it
  fails, the default stays `off` and 13 records why. If programs fail mainly on the subset, the next
  step is a prompt line listing what is unsupported; if the model rarely writes programs, the next
  step is the sentence's wording; neither changes the runtime choice.
- Depends on: stage 5 released, and the stage 4 review recorded as passed (S2); and, if the stage 1 keyring test found a Low worker can read Imperium's Credential Manager entries, that fixed first ([C9](#windows-confinement-mitigation-and-accepted-risk)).

## Rollout

- Stages 1 to 5 ship `harness.code_mode = off`. Stage 1 adds a binary to every bundle, unused
  until the setting or `--flag code_mode` turns the mode on. The default cannot change before the
  stage 4 review has passed (S2).
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

- **A young dependency.** Monty is 11 months old with 22 releases. Exact pins under Imperium's
  lock, a 14-day minimum age (security fixes excepted once the escape suite passes),
  `cargo deny`, the weekly advisory check, the escape suite on every update, one worker process
  per program and fail-closed OS confinement contain the risk. The WebAssembly runtime is the
  fallback.
- **Windows confinement.** A Low-integrity restricted token, a job object and `NO_READ_UP`
  label on Imperium's data, config, sessions and scratch folders stop writes, child processes,
  memory abuse and access to everything Imperium keeps; reads of unlabelled user files and network use after an escape are a
  user-level read compromise, accepted with its rationale
  ([C9](#windows-confinement-mitigation-and-accepted-risk)) and restated in 05.
- **Seccomp allowlist drift.** A Monty or Rust standard library update can need a new system
  call; the worker then fails closed and CI's confinement tests fail, rather than the allowlist
  being widened silently.
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

- **The `worker` feature's weight.** Until 1b, the binary is 40 to 43 MiB and carries `zip` 8.6.0
  with two ignored advisories (0546, 0547); unreachable from a program, but dead weight and an
  audit burden. The owned loop in 1b removes it. The other six zip ignores are pre-existing
  (zip 4.6.1 in `imperium-local` and the updater) and wait for a separate upgrade to zip >= 9.0.5.
- **`eval` and `exec` inside Monty** must be refused by the engine, and every `FunctionCall`
  must be re-checked at call time, not only at name lookup (stage 1a findings).

## Open questions

None for the user; the first round's are [settled](#settled-decisions).

Settled inside this plan (recorded for review): the runtime (C1), reviews refuse `run_script`
(C8), and the budget defaults (C5, tunable by flags in stage 6).
