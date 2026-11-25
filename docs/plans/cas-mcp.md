# Plan: built-in CAS MCP server (SymPy)

Revision: 2026-11-26, revision 1; written against `experimental` at `7bdbabb7` (last updated).
Roadmap [item 62](../12-roadmap.md#next-in-order). Status: planned (no stage started).

Imperium ships a computer algebra system as a built-in [MCP](../05-tools-and-isolation.md#mcp)
server backed by SymPy, so agents (local models in particular) do exact symbolic maths instead of
working it out in text. The server is a `uv` project in `sidecars/cas/`, installed into a managed
environment after the user agrees, started per session like any stdio MCP server, and listed
under Connectors as built in.

The core of the plan is safety. Agent-written maths text reaches a Python library whose standard
parsers (`sympify`, `parse_expr`) call `eval`. The plan therefore never hands agent text to them.
Instead:

- **A parser of our own.** It covers a small grammar over a whitelist of tokens and builds SymPy
  objects through a fixed table.
- **An audit hook.** Each worker aborts on any code compilation, import, file open, process or
  socket event once it has warmed up.
- **One worker per call.** Each call runs in a fresh pre-warmed worker process under
  kernel-enforced CPU, memory and wall-clock limits.
- **A confined server.** The whole server runs in a fixed OS sandbox profile with no network and
  no access to files beyond its own environment, on Windows, Linux and macOS.

Each stage below is one pull request against `experimental` of at most about 800 changed lines
(eight stages), with the docs it changes. No stage changes the research session's files
(`crates/imperium-engine/src/agent/{tools.rs,turn.rs,compact.rs}`, `eval/record.rs`,
`eval/variant.rs`). The CAS tools reach agents through the existing MCP path in `imperium-mcp`
and `imperium-core`, so the engine is untouched.

## Goals

Each goal is boolean and has a check that runs in CI with no manual step. "The matrix" means
Ubuntu, Windows and macOS runners. "The `python` job" is the extended job of
[stage 1](#stage-1-sidecar-skeleton-parser-and-the-ci-job).

1. **Agent text never reaches `eval`, `exec`, `compile`, `__import__` or attribute access.**
   Check: the `python` job on the matrix runs these tests:
   - `test_no_dynamic_code.py`, a scan of the sidecar's source;
   - `test_hostile.py`, the hostile corpus plus a Hypothesis fuzz run under an audit hook that
     records `compile`, `exec`, `import` and `open`, with zero recorded events;
   - `test_no_string_to_sympy.py`, where `sympify` and `parse_expr` are wrapped to fail on any
     `str` argument.
2. **Runaway maths is stopped, not waited on.** These inputs each end within the wall-clock limit
   plus one second, with a `limit` error naming the limit hit, and the next call succeeds:
   `10^10^10`, `factorial(10^7)`, `expand((x+y+z)^5000)`, `integrate(exp(x^x^x), x)`, a 400×400
   symbolic determinant and a 50,000-level nested parenthesis. Check:
   `test_limits.py` in the `python` job on the matrix.
3. **The server cannot use the network or read the user's files.** Under the CAS profile the
   probe cannot do any of these:
   - read a canary file in the home folder, the workspace or `<data>`;
   - write outside its private temporary folder;
   - connect to a TCP port on loopback or the internet;
   - open the daemon's socket;
   - start a process outside its own environment.

   Check: `cargo test -p imperium-tools --test confine` on the matrix, with
   `IMPERIUM_REQUIRE_CONFINE=1` so that a missing sandbox fails rather than skips.
4. **One tool call works end to end through Imperium's MCP client.** A daemon with the CAS set up
   from the committed lock starts a session, and the session's MCP client calls
   `mcp__cas__solve` on `x^2 = 4`, getting text `[-2, 2]` and its LaTeX. Check:
   `cargo test -p imperium-core --test cas_mcp` on the matrix, with `IMPERIUM_REQUIRE_CAS=1`.
5. **Nothing is installed without the user's yes.** Without an approval, no `uv`, no Python and no
   package is downloaded, and the session runs without the server, with a notice. Check: the
   stage 6 and 7 tests with a fake runner, which record every command and download; none happens
   before approval.
6. **The environment is exactly the committed lock.** The managed environment is made by
   `uv sync --frozen` from the committed `uv.lock`, with hashes, using a `uv` binary whose
   SHA-256 is pinned in `runtimes/cas.env` and an exact Python version. Check: the stage 6
   fake-runner test asserts the argument list; the stage 7 daemon test runs the real setup.
7. **It appears under Connectors as built in and can be turned off.** Check: the stage 7
   `mcp.list` test (the `builtin` field and its states) and the stage 8 desktop test. Turning it
   off means new sessions do not start it.

## Decisions

### D1. Input language and parser

**Accepted: a restricted infix grammar of our own, parsed by a hand-written Pratt parser into a
plain-Python syntax tree. The tree is checked for size, and only then built into SymPy objects
through a fixed table.** Tools take strings in this language; nothing else is parsed.

The language (full grammar in stage 1):

- **Numbers.** Integers and decimals, with an optional exponent. A decimal becomes an exact
  `Rational` (`0.1` is `1/10`). Only `evaluate` produces floating-point values.
- **Identifiers.** `[A-Za-z][A-Za-z0-9]*(_[A-Za-z0-9]+)*`, so no leading, trailing or double
  underscore can occur. A known function name followed by `(` is a call. A known constant (`pi`,
  `E`, `I`, `oo`) is that constant. Any other identifier is a `Symbol`. An unknown name followed
  by `(` is an error that lists the known functions.
- **Operators.** `+ - * / ^ **`, unary minus, `!` (factorial), parentheses, and the relations
  `= == != < <= > >=`. A chained relation (`a < b < c`) is an error.
- **Lists and matrices.** `[a, b]` and `[[1, 2], [3, 4]]`.
- **Implicit multiplication.** A number followed by an identifier or `(`, and `)(`, multiply
  (`2x`, `3(x+1)`). Two identifiers side by side are an error that suggests `x*y`, because a
  name like `xy` is one symbol.
- **Unicode.** A short table maps the common maths characters to ASCII first (`×`, `·`, `−`,
  `÷`, `≤`, `≥`, `≠`, `π`, `√`, `²`, `³`). Any other non-ASCII character, a control character or
  a NUL is refused. Refusing all other non-ASCII also closes Python's NFKC folding of identifiers
  (`ｅｖａｌ`), although nothing here is a Python identifier anyway.
- **Caps.** At most 16 KiB per expression, 4,096 tokens, nesting depth 200 (checked while
  parsing, so Python's recursion limit is never reached), 20,000 tree nodes and integer literals
  of at most 1,000 digits.

Why it cannot reach `eval`, `exec`, `__import__` or attribute access:

1. **The language has no syntax for them.** There is no `.` outside a decimal number, no quote,
   no `__`, no `:`, `;`, `@`, `{`, `` ` ``, no keyword and no lambda. A call is only *a table
   name followed by `(`*.
2. **The modules contain no dynamic code.** The parser, guard and builder use only `re`,
   `dataclasses` and `math`. They contain no `eval`, `exec`, `compile`, `getattr`, `setattr`,
   `__import__`, `importlib`, `pickle`, `sympify`, `parse_expr` or `S(` with a string argument.
   `test_no_dynamic_code.py` walks the Python AST of every module in `imperium_cas` and fails on
   any of these names, on any `Attribute` whose object is a `Subscript` or `Call` result built
   from data, and on `globals()`, `locals()` or `vars()`. Ruff's `S102` (exec) and `S307` (eval)
   rules run too.
3. **Only fixed callables are built from text.** Function names resolve in a frozen
   `FUNCTIONS: Mapping[str, Callable]`, a literal dict of explicitly imported SymPy callables such
   as `"sin": sympy.sin`. A lookup miss is an error and never falls back to `getattr(sympy, ...)`.
4. **The only strings that reach SymPy are validated symbol names.** They go to
   `sympy.Symbol(name, **assumptions)`, which does not parse its name. Numbers reach SymPy as
   Python `int` (`Integer(int(text))` after the regex) or as a pair of ints for `Rational`.
   Every other argument is already a SymPy `Basic`, so SymPy's internal `sympify` calls take
   their non-string branch. `test_no_string_to_sympy.py` wraps `sympy.sympify` and
   `parse_expr` for the whole test session to fail on any `str`, and runs every tool over the
   corpus.
5. **Defence in depth: the audit hook.** After a worker has imported SymPy and warmed up, it
   installs a `sys.addaudithook` hook that cannot be removed. Any `compile`, `exec`, `import`,
   `open`, `os.system`, `subprocess.Popen`, `os.exec*`, `os.posix_spawn`, `os.fork`,
   `socket.*` or `ctypes.*` event makes the worker write a refusal and exit with a fixed code. A
   path that did reach `eval` or `parse_expr` raises `compile` and `exec` events first, so it
   would be stopped before any code ran. A legitimate SymPy code path that trips the hook (a lazy
   import) fails closed with a clear error. The corpus over every tool must find none; stage 3
   adds any lazy modules to the warm-up import list.

The hostile corpus (`tests/hostile.txt`, at least 150 lines) includes:

- `__import__('os').system('id')` and `().__class__.__bases__[0].__subclasses__()`;
- `eval("1")`, `exec`, `lambda: 0`, `open('/etc/passwd')`, `x.__class__`;
- `Symbol('x')`, `S('1')`, `sympify('1')`;
- f-string and `%` syntax, backslash escapes, Unicode homoglyphs and full-width underscores;
- NUL and control characters, BOMs, 1 MiB inputs and 50,000 nested parentheses;
- `10^10^10`, `factorial(10^7)`, `x^(10^100)`, and numbers with 10,000 digits.

Each line must either be refused with a parse or guard error or give a result, and the audit
hook must record nothing. Hypothesis generates strings from the grammar's alphabet plus random
bytes (2,000 examples in CI, seed printed) under the same assertions.

Rejected alternatives:

- **`parse_expr` with restricted `global_dict` and transformations.** It still compiles and
  `eval`s generated code. Restricting the namespace is a known-bypassable blacklist.
- **SymPy's LaTeX parser** (`sympy.parsing.latex.parse_latex`). Its ANTLR backend builds SymPy
  objects directly and has no `eval` (checked on SymPy `master`'s `_parse_latex_antlr.py`).
  However:
  - it requires exactly `antlr4-python3-runtime` 4.11;
  - SymPy documents it as experimental;
  - its grammar has no size or depth caps;
  - our safety argument would then rest on a third-party grammar across every SymPy upgrade.

  Models also write ASCII infix more reliably than LaTeX for input. LaTeX stays an output.
- **JSON expression trees** as the input. They are trivially safe, but small models produce them
  badly and they cost far more tokens. The internal tree is in effect this format, so it remains
  a cheap later addition if needed ([open question 4](#open-questions)).

Revisit if models keep writing a syntax that the grammar refuses (track the refusal messages in
evaluations), or if SymPy ships a parser with documented caps and no `eval`.

### D2. Huge numbers and output size

Huge numbers are refused before SymPy sees them where that is cheap, and the resource limits stop
the rest:

- **The guard.** It walks the plain-Python tree before anything is built and estimates the
  result size in floating-point logarithms, never computing the integer. It refuses:
  - an integer power whose estimate is over 100,000 digits (`10^10^10` estimates
    10<sup>10</sup> digits; refused with "about 1e10 digits");
  - `factorial`, `binomial` or `!` of a constant over 100,000;
  - a constant integer exponent over 10,000 on a non-numeric base inside `expand`;
  - a matrix with more than 30 rows or columns, or more than 400 entries.
- **Integer printing.** `sys.set_int_max_str_digits(100_000)` is set in the worker, so printing
  a bigger integer is an error, not a hang.
- **Output cap.** Each result's text plus LaTeX is capped at 64 KiB. A bigger result is an error
  that says how big it was and suggests `evaluate` or a smaller part. It is never cut silently.
- **Precision cap.** `evaluate` precision is 1 to 1,000 significant digits, and `series` order
  is 1 to 50.

### D3. Per-call resource limits: single-use pre-warmed workers

**Accepted: the server's front process (the MCP side) never touches SymPy. Each tool call runs in
a fresh worker process that has already imported SymPy, handles exactly one call and exits. The
front keeps one spare worker warm and starts the next spare as soon as one is taken.** Because a
worker lives for one call, process-lifetime limits are per-call limits on every OS, enforced by
the kernel where possible:

| Limit (default; user layer may lower, or raise up to the cap) | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CPU time, 15 s (cap 120 s) | `RLIMIT_CPU` soft = CPU used after warm-up + limit, hard = soft + 1 s (`SIGXCPU`, then `SIGKILL`) | same | Per-worker Job Object, `PerProcessUserTimeLimit` = CPU used after warm-up + limit |
| Memory, 1 GiB (cap 8 GiB) | `RLIMIT_AS` = address space after warm-up + limit | `RLIMIT_AS` is not enforced on macOS: the front polls the worker's `ri_phys_footprint` (`proc_pid_rusage` through `ctypes` on `libproc`) every 50 ms and kills it over the limit | Job Object `ProcessMemoryLimit` (committed memory) |
| Wall clock, 20 s (cap 300 s) | the front kills the worker | same | same (`TerminateJobObject`) |

- **Timing.** The worker sets its own `rlimit`s after warm-up and before reading the call, from
  values the front passes on its command line. On Windows the front creates the worker's Job
  Object, nested in the server's own job (Windows 8 and later), once the worker reports ready.
  It reads the worker's CPU time with `GetProcessTimes` and sets the limit then.
- **Messages and recovery.** A killed or limit-exceeded worker gives the agent a `limit` error
  naming the limit and its value. The next call takes the spare, so nothing waits on a restart.
- **Process count.** At most two workers exist at once (the running one and the spare). Calls
  from one session's server are served in order, and each session has its own server process.
- **Lazy start.** The spare is started on the first tool call, not when the session starts, so an
  idle session pays only for the front (about 30 MB; estimate). The first call pays SymPy's
  import time, about 1 s (estimate, measured in stage 2).

Rejected alternatives:

- **A long-lived worker pool, killed and restarted on a breach.** CPU limits are cumulative per
  process, so per-call CPU needs polling. Memory and SymPy's caches grow across calls, and one
  call's state (the cache, assumptions) can leak into the next.
- **A fresh cold process per call.** It pays the full import (about 1 s, estimate) on every call.
- **Limits inside one process** (signals, threads). `SIGALRM` does not interrupt C-level loops
  in `gmpy2` or `mpmath`, and Windows has no `SIGALRM`.
- **A front written in Rust** (`imperium cas-serve`), which would reuse Rust's process code. The
  roadmap fixes a Python project with the MCP Python SDK, and a Python server stays usable by
  other MCP clients outside Imperium.

Revisit if the measured first-call latency or memory per session is a problem: share one server
across sessions, or keep a pool of more than one spare.

### D4. Sandbox: a fixed OS-sandbox profile for the CAS

**Accepted: the daemon starts the CAS server through a new allowlist confinement profile in
`imperium-tools` (`confine`). It is tier 1 (OS sandbox) technology, but with a fixed CAS profile
that ignores the session's `isolation.tier`, `isolation.network` and folders.** The server gets
read and execute access to its own managed environment only. It gets no workspace, no network,
no home folder and no `<config>` or `<data>` beyond that environment, plus a private temporary
folder. The workers inherit the confinement. The CAS profile is the same in every session tier,
because the CAS touches no workspace. In the container tier it still runs on the host, so
Docker is not needed.

- **Linux.** `bwrap` starts from an empty root:
  - `--unshare-all` (network, PID, IPC, UTS and cgroup), `--die-with-parent`, `--new-session`
    and `--clearenv`;
  - `--ro-bind` of the environment root and of `/usr`, `/lib`, `/lib64` and `/lib32` (as they
    exist, or their merged-`/usr` links), nothing from `/home` or `/etc`;
  - `--proc /proc`, `--dev /dev` and `--tmpfs /tmp`;
  - only `PATH`, `LANG` and `PYTHONHASHSEED=0` set, and the working folder `/`.

  This is stricter than the commands' OS tier, which starts from `--ro-bind / /` and masks.
- **macOS.** `sandbox-exec -p` with a deny-default profile that allows:
  - `process-exec` and `file-read*` under the environment root;
  - `file-read*` of `/usr/lib`, `/System` and the dyld cache, and of `/dev/null` and
    `/dev/urandom`;
  - `file-write*` under its private temporary folder only;
  - `process-fork` and `sysctl-read`.

  It denies `network*`. `file-read-metadata` is allowed (Python needs `stat` on path ancestors),
  so the existence and size of files is visible, but not their contents. This is a stated limit.
- **Windows.** The first AppContainer in Imperium. The daemon re-executes itself as
  `imperium-core confine --profile cas -- <python> ...`, a small launcher that:
  1. creates or opens the AppContainer profile `Imperium.Cas` with no capabilities (so no
     `internetClient`, and loopback is closed to AppContainers by default);
  2. creates a Job Object with `KILL_ON_JOB_CLOSE`, an active-process limit of 4, a job memory
     limit (the memory limit × 2 + 512 MiB) and all UI restrictions;
  3. starts the server with `CreateProcessW` (`STARTUPINFOEX` with
     `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` and a handle list of only its own standard
     handles);
  4. waits, and exits with the child's code.

  Killing the launcher (`kill_on_drop` in `imperium-mcp`) closes the job and kills the tree. At
  setup, file access is granted by ACL: read and execute on the environment root for the
  container's SID only. User folders grant nothing to AppContainers, so the home folder,
  workspaces and `<data>` are unreadable. The launcher is a re-execution because
  `std::process::Command` cannot set the AppContainer attribute on stable Rust. A launcher keeps
  `imperium-mcp`'s spawn path unchanged on every OS: a confined launch is just a command and
  arguments.
- **Fail closed.** If the sandbox is unavailable (no `bwrap`, user namespaces blocked, no
  `sandbox-exec`, AppContainer creation fails), the server is not started and the session gets a
  notice with the reason. No setting runs the CAS unconfined ([open question 2](#open-questions)).
- **Tests.** `crates/imperium-tools/tests/confine.rs` runs a probe binary (a Rust test helper,
  not Python, so the test needs no environment) under the CAS profile on the matrix. It asserts
  each refusal in goal 3 and that the probe can read its allowed root.
  `IMPERIUM_REQUIRE_CONFINE=1` in CI turns "unavailable" into a failure, as
  `IMPERIUM_REQUIRE_OS_SANDBOX` does for the OS tier.

Rejected alternatives:

- **Running the server under the session's own tier.** Tier 0 confines nothing, Windows has no OS
  tier, and the commands' OS profile can read the whole filesystem by design.
- **The container tier.** It needs Docker for a maths tool.
- **Python-level sandboxing** (audit hooks alone). Hooks are defence in depth (D1), not a
  boundary against code that is already running.

Revisit when the general Windows OS tier lands: it can reuse `confine`'s AppContainer code. Also
revisit if Ubuntu's user-namespace restrictions break `bwrap` for users; that is
[risk 1](#risks-across-stages).

### D5. Process model, managed environment and setup

- **The sidecar.** `sidecars/cas/` is a `uv` project, package `imperium_cas`, with:
  - `pyproject.toml` and a committed `uv.lock` with hashes;
  - a `.python-version` with an exact version (3.12.x, chosen in stage 1);
  - runtime dependencies pinned exactly: `sympy`, `mpmath` and `mcp` (the MCP Python SDK);
  - dev dependencies `pytest`, `hypothesis` and `ruff`.

  The server speaks stdio MCP with `mcp`'s low-level server (no HTTP transport is installed or
  started).
- **How it ships.** The sidecar's source files (not its tests) are embedded in the daemon with
  `include_str!` from a list in `imperium-local/src/cas_env.rs`. A test checks that the list
  matches the tracked files under `sidecars/cas/src/`, `pyproject.toml`, `uv.lock` and
  `.python-version`. The CLI archive therefore needs no resource folder, and the environment's
  version is the SHA-256 of the embedded files (`<digest>`).
- **Layout.** Everything lives under `<data>/runtimes/cas/`:
  - `uv/<uv version>/uv[.exe]`: the pinned `uv`, downloaded from its GitHub release and
    verified against the per-platform SHA-256 in `runtimes/cas.env`. `uv` on `PATH` is never
    used.
  - `<digest>/project/`: the embedded files, written out.
  - `<digest>/python/`: `UV_PYTHON_INSTALL_DIR`, uv's managed CPython of the exact version.
  - `<digest>/venv/`: `UV_PROJECT_ENVIRONMENT`.
  - `managed.json`: written last, as for vLLM. It holds the digest, uv version, Python version,
    size, setup time and whether the user approved.
- **The setup command.** `uv sync --frozen --no-build --no-dev --no-editable --python-preference
  only-managed --compile-bytecode --project <digest>/project`, run without a shell through the
  vLLM [`Runner`](../04-models-and-routing.md) pattern:
  - `UV_CACHE_DIR` points at a temporary folder that is deleted afterwards;
  - `UV_NO_CONFIG=1` ignores user `uv.toml` files;
  - proxy and `UV_INDEX_*` variables are inherited, which cannot change what is installed,
    because the lock's hashes are checked;
  - the output is shown line by line with URL credentials masked;
  - setup is killed after 15 minutes, and a failure removes `<digest>/`.

  On Windows the last step grants the AppContainer SID read and execute on `<digest>/`. Download
  size is about 40 MB (estimate: uv about 15 MB, CPython about 25 MB, SymPy and mpmath about
  8 MB), shown before asking and measured in stage 6.
- **When setup runs (first use, after asking).** The CAS is on by default
  (`mcp.builtin.cas.enabled = true`). When a session would start it and no environment for the
  current digest exists:
  - If the user has never answered, the daemon raises one approval through the existing
    `approvals` queue (kind `builtin.setup`). Its summary says what is downloaded, its size and
    the folder. Every client shows it as it shows push approvals.
  - Until it is answered, sessions start without the server and with a notice.
  - **Approve:** setup runs in the background and `mcp.builtin_changed` announces each state.
    Running sessions keep their servers, and new sessions start it once it is ready.
  - **Decline:** the answer is recorded in `managed.json` (`declined: true`) and nothing asks
    again. Connectors and `imperium runtime cas setup` stay available.
  - **After an Imperium update** that changes the digest, the old approval covers rebuilding the
    environment for the new lock in the background, with a notice
    ([open question 1](#open-questions)). The old `<digest>/` is removed after the new one is
    recorded.
  - **CLI.** `imperium runtime cas setup [--yes] | status | remove` works like
    `runtime vllm` (asks, and `--yes` is the only approval without a terminal). An agent's
    command that runs it is left to a person to approve, as the other install commands are.
- **The launch.** `[python, "-I", "-B", "-m", "imperium_cas"]` (isolated mode: no `PYTHON*`
  variables, no user site, no current folder on the path), wrapped by D4's profile, with a
  cleared environment. `imperium_cas.worker` is started by the front with the same interpreter
  and flags.
- **In sessions.** `imperium-core/src/mcp.rs::session_servers` adds a `ServerSpec` named `cas`
  when the CAS is enabled, set up and the sandbox is available:
  - its `allow` is `["*"]` by default, because the tools have no side effects and run
    sandboxed. `mcp.builtin.cas.ask = true` makes each call ask. Deny rules still apply.
  - timeout is the wall-clock limit + 10 s.
  - A user server named `cas` in `[mcp.servers]` wins, and the built-in is skipped with a
    notice, the same rule as `.mcp.json` collisions.
- **In Connectors.** `mcp.list` reports the built-in as a server entry with a new `builtin`
  field: id, state `not_set_up` / `setting_up` / `ready` / `failed` / `declined` / `unavailable`,
  reason, size and version. The app shows it in a **Built in** group with *Set up*, the switch
  and *Remove*:
  - the switch is `mcp.set_enabled`, which writes `mcp.builtin.cas.enabled` for a built-in name;
  - *Remove* deletes the environment and keeps the setting.

  It cannot be edited or have its command changed.

Rejected alternatives:

- **`uv` on `PATH`, as vLLM does.** It is fine for a GPU runtime aimed at experts, but a built-in
  maths tool should not need a toolchain installed, and a pinned `uv` keeps `uv sync` behaviour
  reproducible.
- **Shipping the sidecar as a Tauri resource folder.** The CLI archives have no resources, and an
  embedded copy cannot drift from the daemon that launches it.
- **Installing silently on first start.** The roadmap and the vLLM precedent require asking.
- **Offering the tools before setup and installing on the first call.** It needs a static tool
  list and a lazy server in `imperium-mcp`, which is more machinery for little gain
  ([open question 3](#open-questions)).

### D6. Tools, schemas and outputs

There are eleven tools; an MCP client sees them as `mcp__cas__<name>`. Each takes expressions in
D1's language plus an optional `assume` object. Every tool declares an `inputSchema` and an
`outputSchema`. Schemas set `additionalProperties: false`, enum and integer bounds, and
`maxLength` 16384 on strings.

| Tool | Input (besides `assume`) | Result |
| --- | --- | --- |
| `simplify` | `expression` | simplified expression |
| `expand` | `expression` | expanded expression (guard: exponent ≤ 10,000) |
| `factor` | `expression`, `gaussian` (bool) | factored expression (integers too: `factor("360")` gives `2^3*3^2*5`) |
| `solve` | `equations` (1–10 strings; a bare expression means `= 0`; relations allowed), `variables` (optional names), `domain` (`complex` default, or `real`) | a set or a list of solution maps; inequalities give intervals or relations; "no solution found" is a result, not an error |
| `differentiate` | `expression`, `variable`, `order` (1–20) | derivative |
| `integrate` | `expression`, `variable`, `lower`, `upper` (both or neither) | antiderivative (no `+ C`, stated in the description) or the definite value; an integral SymPy cannot do is reported as `unevaluated`, with a hint to use `evaluate` for a definite one |
| `limit` | `expression`, `variable`, `to` (expression; `oo` allowed), `direction` (`+`, `-`, `both`) | limit, or `does not exist` |
| `series` | `expression`, `variable`, `at` (default `0`), `order` (1–50) | series with its `O()` term |
| `matrix` | `matrix` (an expression whose value is a matrix: literals with `+ - *` and `^` of an integer), `operation` (`value`, `det`, `inverse`, `transpose`, `rank`, `rref`, `nullspace`, `eigenvalues`, `eigenvectors`, `charpoly`, `solve`), `rhs` (for `solve`) | matrix, scalar or list |
| `evaluate` | `expression`, `digits` (1–1000, default 15), `at` (object: name → expression) | decimal value |
| `equal` | `left`, `right` (expressions, or matrices of the same shape) | `true` (`symbolic`), `false` with a counterexample point (`numeric`), or `unknown` |

How `equal` decides: it tries `simplify(left - right) == 0`, then SymPy's `equals`, then 10
random points at 30 digits under the assumptions. A difference over 10<sup>-20</sup> gives
`false` with the point. Agreement at every point without a proof is reported as `unknown`
("agrees at 10 random points"), never `true`.

**Assumptions** are an object from a symbol name to a list drawn from the enum `real`,
`positive`, `negative`, `nonnegative`, `nonpositive`, `nonzero`, `integer`, `rational` and
`finite`, for example `{"x": ["real", "positive"]}`. The symbols are created with those SymPy
assumptions. A contradictory list is an error. Without `assume`, symbols are SymPy's default
(complex). `solve`'s `domain` is separate, because it changes the solver (`solveset` over
`S.Reals`), not the symbols.

**Output.** Each result has one MCP text item, so Imperium's client (which passes text to the
model) shows both forms:

```text
[-2, 2]
LaTeX: \left[ -2, \  2\right]
```

It also has `structuredContent` `{ "text", "latex", "kind", ... }`, where `kind` is
`expression`, `set`, `solutions`, `matrix`, `boolean`, `number` or `unevaluated`. The text form
is SymPy's `sstr` with `**` replaced by `^`, and it is parseable by D1's language: stage 3
round-trips every corpus result. Errors are MCP tool errors (`isError: true`) with one line that
starts with a category (`parse`, `guard`, `limit`, `math`, `refused`), a short reason and, for
`parse`, the column and a caret line.

Tool descriptions are short: one sentence plus a syntax line, because they count against small
models' context. Stage 3 measures the total in tokens of the full `tools/list` and keeps it
under 1,500 (estimate).

### D7. Testing entirely in CI

- **The `python` job** keeps its name, which is reused for `sidecars/cas/`:
  - `scripts/ci/detect.sh` sets `python=true` when `sidecars/media/pyproject.toml` *or*
    `sidecars/cas/pyproject.toml` exists, and `scripts/ci/test-detect.sh` gains a case for it.
  - `scripts/check-python.sh` loops over the sidecars that exist and runs `uv sync --locked`,
    `uv run ruff check .`, `uv run ruff format --check .` and `uv run pytest` in each.
  - The job becomes a matrix (`ubuntu-latest`, `windows-latest`, `macos-latest`), because the
    limits are per OS.
  - `uv` comes from the already pinned `astral-sh/setup-uv` action. Python comes from uv's
    managed download of the exact `.python-version`, so CI and users run the same interpreter.
  - pytest covers the parser and its refusals, the guard, the hostile corpus and fuzz run, every
    tool, the audit hook and the limits on each OS. The limit tests run against the worker
    directly, without the sandbox; the sandbox is covered by the Rust `confine` test.
- **The `rust` job** (already a matrix) gains:
  - `imperium-tools --test confine` (stages 4 and 5, with the Rust probe);
  - the `cas_env` fake-runner unit tests (stage 6);
  - `imperium-core --test cas_mcp` (stage 7). This test runs the **real** setup into a scratch
    data folder: the pinned `uv` download, Python and `uv sync --frozen`. It then starts a
    daemon and a session and calls `mcp__cas__solve` through the session's MCP client, under the
    real confinement. The scratch `runtimes/cas` is cached with `actions/cache`, keyed on
    `runtimes/cas.env` and the hash of `sidecars/cas/**`, so most runs skip the downloads.
    `IMPERIUM_REQUIRE_CAS=1` is set in `ci.yml`, so the test fails rather than skipping in CI
    and skips elsewhere. Downloads retry three times with backoff.
- No local runs are needed. Developers may run `scripts/check-python.sh`.

### D8. Protocol

The additions are additive: one method, one notification and one optional field. The change takes
the **next protocol minor at merge** of stage 7 (0.61 if nothing else lands first). See
[protocol additions](#protocol-additions).

## Current state (found on `experimental` at `7bdbabb7`)

Implemented:

- **MCP client** (`crates/imperium-mcp`):
  - Stdio servers are started per session in `servers.rs::start` with
    `tokio::process::Command`, `kill_on_drop` and stderr discarded.
  - The `GH_*` variables are removed, and a cleared environment is used when `CLEAN_ENV` is set
    (plugins).
  - Tools are offered as `mcp__<server>__<tool>`, approved unless `allow` lists them.
  - The text content of results reaches the model.
- **Server choice** (`crates/imperium-core/src/mcp.rs::session_servers`): `[mcp.servers]` from
  the user config, then a trusted project's `.mcp.json`, with name collisions reported as
  notices.
- **Connectors** (`crates/imperium-core/src/connectors.rs`, `catalog/connectors.json`,
  `crates/imperium-protocol/src/methods/connectors.rs`): `mcp.list`, `mcp.set_enabled` and the
  others, plus the directory of add-able servers. There is no notion of a built-in server.
- **Approvals** (`crates/imperium-core/src/approvals.rs`): a generic queue with a string `kind`
  and `approval.requested`; no protocol change is needed for a new kind.
- **Managed vLLM environment** (`crates/imperium-local/src/vllm_env.rs`,
  `crates/imperium-cli/src/vllm.rs`, `runtimes/vllm.env`): a `Runner` without a shell, a
  `managed.json` record written last, and pins in a `runtimes/*.env` file.
- **Verified downloads** of llama.cpp builds (`crates/imperium-local/src/runtime.rs`, SHA-256
  checked).
- **OS isolation tier** (`crates/imperium-tools/src/sandbox.rs`): bubblewrap and Seatbelt for
  commands, run through a shell, starting from `--ro-bind / /` with masks. The
  `os_sandbox.rs` tests run in CI with `IMPERIUM_REQUIRE_OS_SANDBOX`.
- **CI**: the `python` job is keyed on `sidecars/media/pyproject.toml` (Ubuntu only,
  `scripts/check-python.sh`). The `rust` matrix runs `scripts/check-rust.sh`.

Missing:

- `sidecars/` does not exist; no Python code is in the repository.
- MCP servers and hooks are never confined ([05](../05-tools-and-isolation.md#mcp), planned).
  Windows has no OS sandbox at all, and no AppContainer or Job Object code exists.
- There is no built-in MCP server and nothing that sets up a Python environment from the daemon.

## Scope and non-goals

In scope: the sidecar and its tools, the parser, guard and limits, the `confine` profile on
three OSes, the managed environment and its setup, daemon wiring, the protocol, the CLI and
desktop surfaces, CI and docs.

Non-goals (each with what would reopen it):

- **Confining other MCP servers, hooks, or Windows commands.** `confine` is built so that they can
  reuse it. Reopen with the roadmap work on running local servers inside the isolation tier.
- **A prompt note telling models to prefer the CAS.** It would touch the engine's prompt files.
  Reopen after evaluations show that models ignore the tools.
- **ODEs (`dsolve`), plotting, units, statistics, number theory beyond `factor`, LaTeX input,
  and an HTTP transport.** Reopen on demand.
- **Sharing one server across sessions.** Reopen if the per-session memory (D3) is a problem.
- **Running the CAS on a remote SSH host's daemon.** It works there if set up there. Nothing
  forwards a local CAS.

## Threat model

Assets:

- the user's files and credentials, the daemon's token and socket, and the network;
- the session's responsiveness;
- the integrity of what the model is told is mathematically true.

Untrusted input:

- every tool argument (written by the model, which may relay injected text from a web page, file
  or issue);
- the PyPI and GitHub release servers during setup;
- other code of the same user at tier 0 (out of scope as a boundary, as in the
  [plugin marketplace plan](plugin-marketplace.md#threat-model)).

1. **Code execution through the expression parser.** Mitigations: D1's own grammar and table
   builder (no `eval` anywhere), the static scan, the no-string-to-SymPy wrapper, the hostile
   corpus and fuzz, and the audit hook. Residual: a SymPy bug that turns a `Basic` into code
   execution without a `compile` or `exec` event. The sandbox (threat 3) bounds that case.
2. **Denial of service.** Mitigations: the size caps and guard (D1, D2), single-use workers with
   CPU, memory and wall-clock limits (D3), the output cap, the job memory and process limits on
   Windows and the `bwrap` PID namespace. One session's runaway call does not affect another's
   server. Residual: within the limits, a model can spend 15 s of CPU per call. The session's
   own loop and budgets still apply.
3. **A compromised server process** (through a SymPy or `mpmath` bug, or a tampered environment).
   Mitigations: the D4 profile, with no network, no files outside its root and a private
   temporary folder; it cannot reach the daemon's socket or token. The environment lives under
   `<data>`, which the OS tier guards read-only from commands. A tier-0 command can already do
   anything as the user, so tampering by it is out of scope. Residual: kernel or sandbox escapes,
   macOS file metadata, and the Linux abstract-socket namespace, which `--unshare-net` closes.
4. **Supply chain at setup.** Mitigations:
   - the `uv` binary is checked against the SHA-256 in the repository;
   - the Python version is exact, and the packages come from the committed lock with hashes,
     installed with `--frozen`;
   - `UV_NO_CONFIG=1` ignores user `uv.toml` files;
   - no source distributions: setup and CI sync with `--no-build`, so no build script runs at
     install. Every locked package (SymPy, `mpmath`, `mcp` and its dependencies such as
     `pydantic-core`) installs from a wheel.

   Residual: a malicious release of a pinned package published before pinning. Dependabot
   proposes updates, and a person reviews them.
5. **Misleading maths.** An `unknown` equality is reported as unknown, never as true. An
   unevaluated integral or unsolved system is reported as such, and floating-point values come
   only from `evaluate`, with their digits stated.
6. **Prompt injection through results.** Results are SymPy output from the agent's own input. Any
   symbol name in them came from the input and matched the identifier regex, and the length is
   capped. Results are framed as MCP tool results, like any other.
7. **Approval and setup abuse.** The approval only installs pinned, hash-locked content, so
   approving it by mistake is low impact. Any client holding the daemon token can answer it, as
   with push approvals.

## Protocol additions

Taken at the next minor when stage 7 merges, all additive with serde defaults:

- **`McpServerInfo.builtin: Option<BuiltinServerInfo>`**: `{ id, state, reason, version,
  size_bytes }`. `state` is one of `not_set_up`, `setting_up`, `ready`, `failed`, `declined` and
  `unavailable` (no sandbox). The server's `config` is a read-only rendering: no command, and
  `catalog: "builtin:cas"`.
- **`mcp.builtin_setup { name } -> { started: bool }`**: starts setup (it counts as the user's
  approval). It returns at once; progress arrives as notifications.
- **`mcp.builtin_remove { name } -> { removed: bool }`**: deletes the environment and keeps the
  setting.
- **Notification `mcp.builtin_changed { name, builtin }`**: sent on each state change and with
  setup output lines (masked).
- **`mcp.set_enabled`** accepts a built-in name and writes `mcp.builtin.<name>.enabled`. Its
  wire shape does not change.

New settings (user layer; a project layer may only turn it off):

- `mcp.builtin.cas.enabled` (default `true`);
- `mcp.builtin.cas.ask` (default `false`);
- `mcp.builtin.cas.limits.{cpu_s, memory_mib, wall_s}` (defaults 15, 1024 and 20; capped at
  120, 8192 and 300).

## Assumptions

- `uv sync --frozen --no-editable` with `UV_PROJECT_ENVIRONMENT` and `UV_PYTHON_INSTALL_DIR`
  writes nothing into the project folder that would break a read-only project. Stage 6 checks
  this, and the project is copied to `<data>` anyway.
- uv-managed CPython (python-build-standalone) runs inside an AppContainer with read and execute
  granted on its folder, and inside the `bwrap` profile with `/usr` and the `/lib*` folders
  only. Stages 4 and 5 verify both with the Rust probe, and stage 7's daemon test verifies them
  with the real Python. If CPython needs more, the profile grows by the named path, never
  `/home` or `/etc` wholesale.
- An AppContainer process can create a nested, unnamed Job Object for a worker. If it cannot, the
  Windows front falls back to polling `GetProcessTimes` and `GetProcessMemoryInfo` every 50 ms
  (the macOS approach). Stage 2 decides from a CI run, and the plan is unchanged either way.
- The MCP Python SDK's stdio server works with the `2025-06-18` revision that `imperium-mcp`
  speaks, and returns `structuredContent`. Stage 3 pins a version that does.
- The importing workers' warm-up covers SymPy's lazy imports for all eleven tools. Stage 3's
  corpus run under the hook proves it.

## Relevant paths

- New: `sidecars/cas/` (`pyproject.toml`, `uv.lock`, `.python-version`, `src/imperium_cas/`,
  `tests/`), `runtimes/cas.env`, `crates/imperium-tools/src/confine/` (`mod.rs`, `linux.rs`,
  `macos.rs`, `windows.rs`), `crates/imperium-tools/tests/confine.rs`,
  `crates/imperium-local/src/cas_env.rs`, `crates/imperium-cli/src/cas.rs`,
  `crates/imperium-core/src/builtin_servers.rs`, `crates/imperium-core/tests/cas_mcp.rs`.
- Changed: `crates/imperium-core/src/mcp.rs`, `connectors.rs`, `main.rs` (the `confine` entry),
  `rpc/` (the connectors methods), `crates/imperium-protocol/src/methods/connectors.rs`,
  `crates/imperium-config/src/settings.rs` and `registry.rs`,
  `apps/desktop/src/ConnectorsWindow.tsx`, `scripts/ci/detect.sh`, `scripts/ci/test-detect.sh`,
  `scripts/check-python.sh`, `.github/workflows/ci.yml`.
- Not touched: `crates/imperium-engine/src/agent/{tools.rs,turn.rs,compact.rs}`,
  `eval/record.rs`, `eval/variant.rs`, and `crates/imperium-mcp/src/servers.rs`'s spawn path
  (a confined launch is a command and arguments).

## Related docs

- [05 Tools and isolation](../05-tools-and-isolation.md#mcp): MCP, Connectors and isolation
  tiers. 05 wins on how the tiers behave; this plan wins on the CAS profile.
- [04 Models and routing](../04-models-and-routing.md): the managed vLLM environment pattern.
- [09 Media generation](../09-media-generation.md#media-sidecar): the other planned sidecar. The
  `python` job's loop serves both.
- [11 CI/CD](../11-ci-cd.md): the `python` and `rust` jobs.
- [Roadmap item 62](../12-roadmap.md#next-in-order) is the spec; this plan wins on details.

## Stages

Each stage is one pull request against `experimental` with the docs it changes. Stages 1 to 3
(Python) and stages 4 and 5 (Rust confinement) are independent and can run in parallel. Stage 6
needs 1; stage 7 needs 3 to 6; stage 8 needs 7.

### Stage 1: Sidecar skeleton, parser and the CI job

- Objective: create `sidecars/cas/` with `pyproject.toml`, `uv.lock` and `.python-version`
  (exact 3.12.x), with ruff configured with the `S` (bandit) rules. The CAS sync in
  `check-python.sh` adds `--no-build`, so a lock that needs a source build fails CI. Write `parse.py`
  (tokenizer, Unicode table, Pratt parser and caps, to a plain tree) and `guard.py` (the size
  and digit estimates). Extend CI: `detect.sh`, `test-detect.sh`, `check-python.sh` looping over
  the sidecars, and the `python` job as a three-OS matrix. Record the grammar in the sidecar's
  docstring and in [05](../05-tools-and-isolation.md#mcp) (a short "planned" note).
- Depends on: none.
- Owned paths: `sidecars/cas/**` (no builder or tools yet), `scripts/ci/detect.sh`,
  `scripts/ci/test-detect.sh`, `scripts/check-python.sh`, `.github/workflows/ci.yml` (the
  `python` job), [11](../11-ci-cd.md).
- Acceptance checks: in the `python` job on the matrix, pytest passes:
  - parser unit tests (precedence, implicit multiplication, relations, matrices and every refusal
    with its column);
  - the guard tests (`10^10^10` refused without computing, `2^1000` allowed);
  - the hostile corpus at parse level (every line refused or parsed, within 100 ms each);
  - `test_no_dynamic_code.py`;
  - Hypothesis at 2,000 examples.

  Ruff passes. `test-detect.sh` passes the new case. About 700 lines.
- Status: pending.

### Stage 2: Worker and per-call limits

- Objective: write `build.py`, the table builder from the tree to SymPy, with `assume`.
  `worker.py` warms up, installs the audit hook, sets its limits, takes one call over a
  length-prefixed JSON pipe, answers and exits. `pool.py` is the front's side: the spare worker,
  the wall clock, the macOS footprint poll and the Windows per-worker Job Object (via `ctypes`).
  Measure the warm-up time and memory on each OS, and record them in the outcome.
- Depends on: stage 1.
- Owned paths: `sidecars/cas/src/imperium_cas/{build,worker,pool,limits_*}.py` and their tests.
- Acceptance checks:
  - `test_limits.py` on the matrix: each goal 2 input ends within its limit plus 1 s with the
    right category, and a normal call succeeds right after.
  - `test_audit_hook.py`: inside a worker, `open`, `socket`, `subprocess`, `compile` and
    `import` of a new module each end the worker with the refusal code.
  - `test_no_string_to_sympy.py` over the builder.
  - The Windows nested-job assumption is settled (kept, or the polling fallback).

  About 750 lines.
- Status: pending.

### Stage 3: MCP front and the eleven tools

- Objective: write `server.py` (MCP stdio via the SDK's low-level server; `tools/list` with the
  input and output schemas, and `tools/call` handed to `pool`), and `tools.py`, the eleven tools
  with D6's outputs and error categories. Finish the warm-up list from the corpus run. Measure
  the token size of the tool list.
- Depends on: stage 2.
- Owned paths: `sidecars/cas/src/imperium_cas/{server,tools,format}.py`, `__main__.py` and their
  tests.
- Acceptance checks:
  - per-tool tests, including `equal`'s three outcomes, `unevaluated` integrals and
    contradictory assumptions;
  - a round trip: every result's text re-parses to an equal expression;
  - an in-process MCP client test (the SDK's memory transport) and one test over a real stdio
    subprocess;
  - the full hostile corpus through `tools/call` with zero audit events;
  - the tool list under the measured token budget.

  About 800 lines. If it is over, split it into 3a (front, `simplify`, `expand`, `factor`,
  `solve`, `evaluate`, `equal`) and 3b (the calculus tools and `matrix`).
- Status: pending.

### Stage 4: `confine` on Linux and macOS

- Objective: `imperium-tools::confine` provides `Profile` (environment root, temporary folder),
  `available() -> Result<(), Unavailable>` and `launch(profile, program, args) -> (command,
  args)`. It builds the `bwrap` allowlist arguments and the Seatbelt deny-default profile, and
  adds a probe helper binary for the tests. Document the profile in
  [05](../05-tools-and-isolation.md#isolation-tiers).
- Depends on: none.
- Owned paths: `crates/imperium-tools/src/confine/{mod,linux,macos}.rs`,
  `crates/imperium-tools/tests/confine.rs`, the probe helper, `ci.yml` (`IMPERIUM_REQUIRE_CONFINE`).
- Acceptance checks: `cargo test -p imperium-tools --test confine` on Linux and macOS, with
  every goal 3 refusal asserted and the allowed root readable. Unit tests of the argument and
  profile builders cover no `/home`, no `/etc` and `--unshare-all`. About 600 lines.
- Status: pending.

### Stage 5: `confine` on Windows (AppContainer and Job Object)

- Objective: write `confine/windows.rs`, which creates or opens the `Imperium.Cas` profile,
  grants the ACL on a root, builds the job and calls `CreateProcessW` with security
  capabilities and a handle list. Add the `imperium-core confine --profile cas -- ...` entry in
  `main.rs` before the daemon's own startup. Use `windows-sys` (already a dependency) with the
  needed features.
- Depends on: stage 4 (the shared `Profile` API).
- Owned paths: `crates/imperium-tools/src/confine/windows.rs`, `crates/imperium-tools/Cargo.toml`
  (features), `crates/imperium-core/src/main.rs` (the entry only), and the Windows part of
  `tests/confine.rs`.
- Acceptance checks:
  - `cargo test -p imperium-tools --test confine` on Windows asserts goal 3's refusals: the home
    canary, `<data>`, a loopback TCP connect and the named pipe are refused; the probe's process
    count stays capped; killing the launcher kills the probe.
  - A test that the launcher refuses an unknown profile and any argument before `--` other than
    `--profile`.

  About 700 lines.
- Status: pending.

### Stage 6: Managed environment and the CLI

- Objective: write `imperium-local::cas_env` (the embedded files list and digest, `runtimes/cas.env`
  pins and the verified `uv` download, the setup steps through the `Runner`, the record written
  last, `status` and `remove`, and the ACL grant through stage 5's function on Windows). Add the
  CLI `imperium runtime cas setup|status|remove`. Document it in
  [04](../04-models-and-routing.md) next to vLLM, or in 05.
- Depends on: stage 1 (the files to embed). Stage 5 provides the ACL step; until it merges,
  stage 6 stubs that step behind `cfg(windows)`.
- Owned paths: `crates/imperium-local/src/cas_env.rs`, `runtimes/cas.env`,
  `crates/imperium-cli/src/cas.rs`, the CLI's `runtime` command table.
- Acceptance checks:
  - fake-runner tests: the exact `uv` arguments and environment (goal 6);
  - a `uv` digest mismatch refused with nothing kept;
  - a failure that leaves no record and no `<digest>/`;
  - an interrupted setup that leaves no record;
  - setting up again changes nothing;
  - no command or download before the user's yes (goal 5, the CLI part);
  - the embedded-files list matches the tracked files.

  About 650 lines.
- Status: pending.

### Stage 7: Daemon wiring, protocol and the end-to-end test

- Objective: write `builtin_servers.rs` (the built-in registry, state, approval on first use,
  background setup, notices and the digest-change rebuild). Add the `cas` spec in
  `session_servers` (the confined launch, `allow`, timeout, collision rule) and the settings.
  Add the protocol additions (next minor), the `mcp.list` `builtin` field, `mcp.set_enabled` for
  built-ins, and the regenerated TypeScript bindings. Add `tests/cas_mcp.rs` and its CI cache and
  `IMPERIUM_REQUIRE_CAS`. Update [05 Connectors](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented)
  and [02](../02-architecture.md#protocol) (the protocol list).
- Depends on: stages 3, 4, 5 and 6.
- Owned paths: `crates/imperium-core/src/{builtin_servers,mcp,connectors}.rs`, `rpc/`
  (connectors), `crates/imperium-protocol/src/methods/connectors.rs`, `lib.rs` (the minor),
  `packages/protocol/src/generated/`, `crates/imperium-config/src/{settings,registry}.rs`,
  `crates/imperium-core/tests/cas_mcp.rs`, `ci.yml` (cache and environment).
- Acceptance checks:
  - `cargo test -p imperium-core --test cas_mcp` on the matrix (goal 4, real setup and real
    confinement);
  - daemon tests with a fake runner: approval raised once, no download before it, decline
    recorded, approve then ready then served in a new session, a user `cas` server wins, a
    disabled built-in not started, an unavailable sandbox reported and not started;
  - `mcp.list` shows each state (goals 5 and 7).

  About 800 lines. If it is over, split it into 7a (registry, settings and protocol) and 7b
  (session wiring and the end-to-end test).
- Status: pending.

### Stage 8: Connectors UI, docs and close-out

- Objective: add a **Built in** group in `ConnectorsWindow.tsx` (state, *Set up* with the size,
  progress lines, the switch, *Remove*, and the reason when unavailable), and a desktop test.
  Finish the docs: 05 (the CAS server, its tools, language and limits), 04 or 05 (the
  environment), 11, and roadmap item 62 marked implemented with this plan's status.
- Depends on: stage 7.
- Owned paths: `apps/desktop/src/ConnectorsWindow.tsx` and its test, the docs above.
- Acceptance checks: `pnpm test` covers the group rendering each state, *Set up* calling
  `mcp.builtin_setup` and the switch calling `mcp.set_enabled` (goal 7). The `docs` job passes.
  About 450 lines.
- Status: pending.

## Risks across stages

1. **Sandbox availability on users' machines.** Ubuntu 24.04 restricts unprivileged user
   namespaces through AppArmor, and `bwrap` may be missing or blocked; corporate Windows policy
   may block AppContainer creation. Because the profile fails closed, the CAS is then
   `unavailable` with the reason. The docs say how to enable `bwrap`.
2. **CPython inside the allowlist profiles** may need paths found only by running. Stages 4 and 5
   probe them with Rust, and stage 7 runs the real interpreter on the matrix.
3. **SymPy lazy imports tripping the audit hook** cause false refusals. The corpus over every tool
   in stage 3 finds them, and the warm-up list grows.
4. **CI time and flakiness.** The real setup downloads about 40 MB on three OSes; the cache and
   retries reduce this. The `python` job grows to three OSes, about 3 minutes each (estimate).
5. **Per-session memory.** Each session with the CAS has a front, and after the first call a
   spare worker (about 80 MB; estimate). Stage 2 measures it. If it matters, share a server
   (non-goal for now) or start the spare only on demand.
6. **Tool-list size for small models.** Eleven schemas add context to every request. Stage 3
   keeps the descriptions short and measures them. Item 61's `tool_search` can hide them for local
   models once it lands.

## Open questions

1. **Re-setup after an update.** Should a changed lock (an Imperium update) rebuild the
   environment without asking again, as planned, or ask once per update?
2. **No sandbox available.** Should there be a user opt-out that runs the CAS without the OS
   sandbox, relying on the parser and audit hook alone? The plan says no (fail closed).
3. **When to ask.** Is the approval at the first session start the right "first use", or should
   the tools be offered before setup, with the question asked at the first call (needs a static
   tool list and a lazy server in `imperium-mcp`)?
4. **Input formats.** Should JSON expression trees also be accepted as a second input format,
   and should `solve` default to the `real` domain for small models?
