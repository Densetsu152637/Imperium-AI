# Plan: built-in CAS MCP server (SymPy)

Revision: 2026-11-26, revision 3 (plan-gate review round 2: R2-1 to R2-5 and the nits folded in;
revision 2 folded in F1 to F11 and decided the four open questions); written against
`experimental` at `56ce2da9` (last updated). Roadmap [item 62](../12-roadmap.md#next-in-order).
Status: in progress (stage 1 done in review; no other stage started).

Imperium ships a computer algebra system as a built-in [MCP](../05-tools-and-isolation.md#mcp)
server backed by SymPy, so agents (local models in particular) do exact symbolic maths instead of
working it out in text. The server is a `uv` project in `sidecars/cas/`, installed into a managed
environment after the user agrees, started per session like any stdio MCP server under the
reserved name `imperium_cas`, and listed under Connectors as built in.

The core of the plan is safety. Agent-written maths text reaches a Python library whose standard
parsers (`sympify`, `parse_expr`) call `eval`. The plan therefore never hands agent text to them.
Instead:

- **A parser of our own.** It covers a small grammar over a whitelist of tokens, checks every
  constant subtree against a size guard, and builds SymPy objects through a fixed table.
- **An audit hook.** Once each worker has warmed up, it allows only a measured list of audit
  events and exits on any other.
- **One worker per call.** Each call runs in a fresh pre-warmed worker process under CPU, memory
  and wall-clock limits, enforced by the kernel where the OS can.
- **A confined server.** The server and every process it starts run in a fixed OS sandbox profile
  with no network, no environment variables of the daemon's and no access to files beyond the
  server's own environment, on Windows, Linux and macOS.

Each stage below is one pull request against `experimental` of at most about 800 changed lines
(nine stages, with stage 2 split into 2a and 2b and three more with a planned split), with the docs it changes. No stage changes the files
another session owns:

- `crates/imperium-engine/src/agent/{tools.rs,turn.rs,compaction.rs,prompts.rs}` and
  `crates/imperium-engine/src/compact.rs`;
- `imperium-core`'s `eval*` files (`eval.rs`, `eval/`, including `eval/replay.rs`, and
  `state/eval.rs`);
- `imperium-models`' `chat.rs`;
- the `imperium-memory` crate.

The CAS tools reach agents through the existing MCP path in `imperium-mcp` and `imperium-core`.
Evaluation sessions start no MCP servers (`state/create.rs` gives them an empty list), so the CAS
needs nothing in the evaluation code, and evaluation tracking of CAS use is out of scope.

## Goals

Each goal is boolean and has a check that runs in CI with no manual step. "The matrix" means
Ubuntu, Windows and macOS runners. "The `python-cas` job" is the job added in
[stage 1](#stage-1-sidecar-skeleton-parser-guard-and-the-ci-jobs).

1. **Agent text never reaches `eval`, `exec`, `compile`, `__import__` or dynamic attribute
   access.** Check: these pass in the `python-cas` job on the matrix:
   - `test_no_dynamic_code.py`, the AST scan of D1;
   - `test_hostile.py`, the hostile corpus plus a Hypothesis fuzz run, under an audit hook that
     records every event, with zero events outside the allow-list;
   - `test_no_string_to_sympy.py`, where `sympify`, `parse_expr` and `lambdify` are wrapped to
     fail on any call with a `str` argument (`lambdify` on any call at all).
2. **Runaway maths is refused or stopped, never waited on.** Each of these ends within the
   wall-clock limit plus one second, with a `guard`, `parse` or `limit` error, and the next call
   succeeds: `10^10^10`, `1e999999999`, `sqrt(2)^(10^6)`, `factorial(factorial(20))`,
   `factorial(10^7)`, `expand((x+y+z)^5000)`, `integrate(exp(x^x^x), x)`, a 30×30 symbolic
   determinant, `[[2,1],[1,1]]^(10^6)` and a 50,000-level nested parenthesis. Separately, one
   deterministic test per limit (a CPU spin, a fast allocation and a blocking sleep) ends with
   `limit` naming that limit. Check:
   - `test_limits.py` in the `python-cas` job on Linux and macOS, with test-only worker modules
     for the deterministic tests;
   - on Windows, stage 5's broker test with the probe;
   - on all three OSes through the real confined server, stage 7's `cas_mcp` limit cases.
3. **The server and every process it starts are confined.** Under the CAS profile the probe, and
   a child the probe starts, cannot do any of these:
   - read a canary file in the home folder, the workspace or `<data>`;
   - write outside the private temporary folder;
   - connect to a TCP port on loopback or the internet;
   - open the daemon's socket or named pipe;
   - see a canary environment variable set in the daemon's environment;
   - start a program outside the environment root. On Windows a worker cannot start any process
     at all. On Linux this holds where Landlock is available; elsewhere, see
     [D4's residual](#d4-sandbox-a-fixed-allowlist-profile-for-the-cas).

   Check: `cargo test -p imperium-tools --test confine` on the matrix, with
   `IMPERIUM_REQUIRE_CONFINE=1` so a missing sandbox fails rather than skips, and the
   `confine (ubuntu-24.04)` job.
4. **One tool call works end to end through Imperium's MCP client.** A daemon with the CAS set up
   from the committed lock starts a session, and the session's MCP client calls
   `mcp__imperium_cas__solve` on `x^2 = 4`, getting text `{-2, 2}` with its domain and its LaTeX.
   Check: `cargo test -p imperium-core --test cas_mcp` on the matrix, with
   `IMPERIUM_REQUIRE_CAS=1`.
5. **Nothing is installed without the user's yes.** Without an approval no `uv`, no Python and no
   package is downloaded, and the session runs without the server, with a notice. An unattended,
   headless or remote session never raises the approval. After *Remove* it is asked again.
   Check: the stage 6 and 7 tests with a fake runner, which record every command and download.
6. **The environment is exactly the committed lock.** The managed environment is made by
   `uv sync --frozen --no-build` from the committed `uv.lock` with hashes, by a `uv` binary whose
   SHA-256 is pinned in `runtimes/cas.env`, with an exact Python version and an allow-listed
   environment. Check: the stage 6 fake-runner test asserts the argument list and the whole
   environment; the stage 7 daemon test runs the real setup.
7. **It appears under Connectors as built in, can be turned off, and its name is reserved.**
   Check:
   - the stage 7 `mcp.list` test (the `builtin` field and its states);
   - the stage 7 name-reservation tests: user config, environment config file and `.mcp.json`
     each refused with a notice;
   - the stage 8 desktop test.

   Turning it off means new sessions do not start it.

## Decisions

### D1. Input language and parser

**Accepted: a restricted infix grammar of our own, parsed by a hand-written Pratt parser into a
plain-Python syntax tree. The tree passes D2's guard, and only then is it built into SymPy objects
through a fixed table.** Tools take strings in this language and names that match the identifier
pattern; nothing else is parsed. JSON expression trees are not accepted (decided: one input
format).

The language (full grammar in stage 1):

- **Numbers.** Integers and decimals, with an optional exponent. A decimal becomes an exact
  `Rational` (`0.1` is `1/10`), so its exponent goes through the guard (`1e999999999` is
  refused). Only `evaluate` produces floating-point values.
- **Identifiers.** `[A-Za-z][A-Za-z0-9]*(_[A-Za-z0-9]+)*`, so no leading, trailing or double
  underscore can occur.
  - A name in `FUNCTIONS` followed by `(` is a call.
  - A known constant (`pi`, `E`, `I`, `oo`) is that constant.
  - Any other identifier is a `Symbol`.
  - An unknown name followed by `(` is an error that lists the known functions.
- **Operators.** `+ - * / ^ **`, unary minus, `!` (factorial), parentheses, and the relations
  `= == != < <= > >=`. A chained relation (`a < b < c`) is an error.
- **Lists and matrices.** `[a, b]` and `[[1, 2], [3, 4]]`.
- **Implicit multiplication.** A number followed by an identifier or `(`, and `)(`, multiply
  (`2x`, `3(x+1)`). Two identifiers side by side are an error that suggests `x*y`, because a
  name like `xy` is one symbol.
- **Unicode.** A short table maps the common maths characters to ASCII first (`×`, `·`, `−`,
  `÷`, `≤`, `≥`, `≠`, `π`, `√`, `²`, `³`). Any other non-ASCII character, a control character or
  a NUL is refused.
- **Caps.** At most 16 KiB per expression and 4,096 tokens. Nesting depth is capped at 200,
  checked while parsing. Every recursive step (a bracket, an operand, an argument) adds one to
  the depth, and a level costs at most three Python frames, so the parser uses about 600 frames at
  the cap and Python's recursion limit (1,000) is not reached from a worker's shallow stack. A tree has at most 8,192
  nodes: each token makes at most one node and each implicit multiplication one more, so the
  node cap is twice the token cap and is never the binding limit before the token cap. Integer
  literals have at most 1,000 digits.
- **Names in parameters.** `variable`, `variables`, the keys of `assume` and the keys of `at`
  must match the identifier pattern. This is checked twice: by the input schema (`pattern`) and
  again in the builder.

**The function table.** `FUNCTIONS` is a frozen mapping from name to an explicitly imported SymPy
callable and a guard class (D2). It is this list and nothing else. Stage 1 pins the list, and a
test fails if the table and this list differ.

| Guard class | Names | Why |
| --- | --- | --- |
| `symbolic` (no eager exact arithmetic; magnitude passes through for D2's estimate) | `sin cos tan cot sec csc asin acos atan acot atan2 sinh cosh tanh coth asinh acosh atanh exp log ln erf Abs sign re im arg conjugate` | SymPy leaves these unevaluated on exact constants (`sin(1)`, `exp(1000)`) or returns small exact values |
| `root` (treated as `Pow` with a rational exponent) | `sqrt cbrt root` | `sqrt(2)^(10^6)` is an eager integer power |
| `select` (magnitude at most the inputs' largest, or for `lcm` their product) | `floor ceiling Min Max Mod gcd lcm` | they compute from their inputs, and `lcm` is bounded by the product |
| `digits` (result size estimated; refused over 100,000 digits) | `factorial gamma binomial fibonacci lucas` | eager exact integers: `log10(n!)` by `lgamma`, `binomial` by `lgamma`, `fibonacci(n)` and `lucas(n)` about `0.209·n` digits |
| `capped` (an argument cap, because cost grows faster than result size) | `prime` (n ≤ 10<sup>6</sup>), `nextprime`, `prevprime`, `isprime` (argument ≤ 200 digits), `harmonic` (n ≤ 10,000), `bernoulli` (n ≤ 2,000), `catalan` (n ≤ 10,000) | sieves, primality tests and big rationals |

Why agent text cannot reach `eval`, `exec`, `__import__` or attribute access:

1. **The language has no syntax for them.** There is no `.` outside a decimal number, no quote,
   no `__`, no `:`, `;`, `@`, `{`, `` ` ``, no keyword and no lambda. A call is only *a table
   name followed by `(`*.
2. **The modules contain no dynamic code.** `test_no_dynamic_code.py` walks the Python AST of
   every module of `imperium_cas` and fails on any of these:
   - **R1.** A `Name` or `Attribute` whose identifier is one of `eval`, `exec`, `compile`,
     `__import__`, `getattr`, `setattr`, `delattr`, `globals`, `locals`, `vars`, `open`,
     `sympify`, `parse_expr`, `parse_latex`, `lambdify`, `S`, `importlib`, `pickle`, `marshal`
     or `system`. The only exceptions are listed per module in the test:
     - `pool.py` may use `subprocess` and `os`;
     - `limits_macos.py` and `landlock.py` may use `ctypes`. They take no agent data.
   - **R2.** An `Attribute` whose attribute name starts with `_`.
   - **R3.** An `import` inside a function.
   - **R4.** Any import in `parse.py`, `guard.py` and `build.py` other than `re`, `math`,
     `dataclasses` and `typing`, plus, in `build.py`, `from sympy import <name>` of the table's
     names.

   Ruff's `S102` (exec) and `S307` (eval) rules run too.
3. **Only fixed callables are built from text.** Function names resolve in the frozen table. A
   miss is an error, never `getattr(sympy, ...)`.
4. **The only strings that reach SymPy are validated symbol names.** They go to
   `sympy.Symbol(name, **assumptions)`, which does not parse its name. Numbers reach SymPy as
   Python `int` (`Integer(int(text))` after the regex) or as a pair of ints for `Rational`. Every
   other argument is already a SymPy `Basic`, so SymPy's internal `sympify` calls take their
   non-string branch. `test_no_string_to_sympy.py` wraps `sympy.sympify` and `parse_expr` to
   fail on any `str`, and `lambdify` to fail on any call, for the whole test session, and runs
   every tool over the corpus.
5. **Defence in depth: the audit hook is an allow-list.** After a worker has imported SymPy and
   warmed every tool's code path, it installs a `sys.addaudithook` hook, which cannot be removed.
   - The hook passes only the events in a fixed allow-list, measured in stages 2a and 3 from
     the corpus over every tool (expected: a handful such as `object.__getattr__` and
     `sys._getframe`). It never allows `compile`, `exec`, `import`, `open`, `os.*`,
     `subprocess.*`, `socket.*` or `ctypes.*`.
   - On any other event the hook calls `os._exit(REFUSED)` inside itself. It does not raise,
     because SymPy's own `try`/`except` blocks could swallow an exception. The front turns
     exit code `REFUSED` into a `refused` error.
   - A path that did reach `eval` or `parse_expr` raises `compile` or `exec` first, so it would
     be stopped before any code ran. A legitimate lazy import that trips the hook fails closed;
     stage 3 adds its module to the warm-up list.

The hostile corpus (`tests/hostile.txt`, at least 150 lines) includes:

- `__import__('os').system('id')` and `().__class__.__bases__[0].__subclasses__()`;
- `eval("1")`, `exec`, `lambda: 0`, `open('/etc/passwd')`, `x.__class__`;
- `Symbol('x')`, `S('1')`, `sympify('1')`, `lambdify(x, x)`;
- f-string and `%` syntax, backslash escapes, Unicode homoglyphs and full-width underscores;
- NUL and control characters, BOMs, 1 MiB inputs and 50,000 nested parentheses;
- every goal 2 input;
- hostile names in `variable`, `assume`, `at` and `variables` (`__class__`, `os.system`, `x y`).

Each line must be refused with a `parse` or `guard` error or give a result, and the audit hook
must record nothing outside its allow-list. Hypothesis generates strings from the grammar's
alphabet plus random bytes (2,000 examples in CI, seed printed) under the same assertions.

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
- **JSON expression trees** as a second input format (decided against). Small models produce
  them badly and they cost far more tokens.

Revisit if models keep writing a syntax the grammar refuses (watch the refusal messages), or if
SymPy ships a parser with documented caps and no `eval`.

### D2. The guard, huge numbers and output size

**Accepted: a bottom-up estimate of size over the plain tree, before anything is built.** For
every subtree whose leaves are all constants, the guard computes an upper bound on the result's
size in floating-point base-10 logarithms, and never computes the value:

- `mag`: log10 of the absolute value;
- `num` and `den`: digits of an exact rational's numerator and denominator, or `+inf` when the
  value is not an exact rational.

Rules:

- **Literals.** An integer's digits. A decimal `a.bEc` gives `num` and `den` from its exact
  rational, so `1e999999999` has about 10<sup>9</sup> digits and is refused.
- **`+` and `-`:** `mag` is at most the maximum plus `log10(2)`, and `den` is at most the sum of
  the `den`s. **`*`:** sums. **`/`:** sums, with numerator and denominator swapped on the right.
- **Powers** (`^`, `**`, and the `root` class) of an exact base by a constant exponent `p/q`:
  the result has about `|p/q|·max(num, den)` digits. A base that is not exact has its `mag`
  scaled the same way, since it may become exact (`sqrt(2)^(10^6)` is `2^500000`, about 150,515
  digits: refused). An exponent whose own `mag` is over 6 is refused unless the base is `0`, `1`
  or `-1`.
- **Function calls:** by the table's class (D1):
  - `digits` estimates the result's digit count (`factorial(n)` is `lgamma(n+1)/ln 10`), so the
    factorial cap *is* the 100,000-digit rule (about `n ≤ 25,000`). A nested call
    (`factorial(factorial(20))`) is refused when its argument's `mag` is over 6.
  - `capped` checks its argument cap.
  - `select` takes the largest input's non-negative magnitude (the sum of them for `lcm`); never
    a sum of signed logs.
  - `symbolic` passes the magnitude through: `exp` and `cosh` grow it, since the result may be
    raised to a power later.
- **Matrices:** at most 30 rows or columns and 400 entries. A matrix power `M^n` with a constant
  integer `n` is estimated as `n·(entry digits + log10(size))` and refused over 100,000 digits.
  `n` is also capped at 1,000 for a symbolic matrix.
- **`expand`:** a constant integer exponent over 10,000 on a non-numeric base is refused.
- **Threshold:** any estimate over 100,000 digits is a `guard` error that names the estimate, for
  example "about 1e10 digits".

Around the guard:

- **Integer printing.** `sys.set_int_max_str_digits(100_000)` is set in the worker, so printing a
  bigger integer is an error, not a hang.
- **Output cap.** Each result's text plus LaTeX is capped at 64 KiB. A bigger result is an error
  that says how big it was and suggests `evaluate` or a smaller part. It is never cut silently.
- **`evaluate`** substitutes with `expr.evalf(n=digits, subs=at)`, never `.subs()` followed by
  `evalf`, so a substitution is never evaluated exactly first. Precision is 1 to 1,000
  significant digits. `series` order is 1 to 50.
- **Limits.** What the guard cannot see (a simplification that blows up symbolically) is stopped
  by D3's limits.

### D3. Per-call resource limits: single-use pre-warmed workers

**Accepted: the server's front process (the MCP side) never touches SymPy. Each tool call runs in
a fresh worker process that has already imported SymPy, handles exactly one call and exits. The
front keeps one spare worker warm and starts the next spare as soon as one is taken.** Because a
worker lives for one call, process-lifetime limits are per-call limits:

| Limit (default; user layer may lower, or raise up to the cap) | Linux | macOS | Windows |
| --- | --- | --- | --- |
| CPU time, 15 s (cap 120 s) | `RLIMIT_CPU` soft = CPU used after warm-up + limit, hard = soft + 1 s (`SIGXCPU`, then `SIGKILL`) | same | per-worker Job Object `PerProcessUserTimeLimit` = CPU used after warm-up + limit, set by the broker |
| Memory, 1 GiB (cap 8 GiB) | `RLIMIT_AS` = address space after warm-up + limit | `RLIMIT_AS` is not enforced on macOS: the front polls the worker's `ri_phys_footprint` (`proc_pid_rusage` through `ctypes` on `libproc`) every 50 ms and kills it over the limit | per-worker Job Object `ProcessMemoryLimit` = committed memory after warm-up + limit (the same baseline as Linux) |
| Wall clock, 20 s (cap 300 s), counted from when the call was received | the front kills the worker | same | the broker terminates the worker's job |

Details:

- **macOS memory overshoot.** Polling is not a hard cap. Between two polls a worker can allocate
  as fast as the machine allows: about 1 to 2 GiB in 50 ms on current hardware (estimate,
  measured in stage 2b). So a macOS worker can briefly exceed its limit by that much before it
  is killed. Stage 2b records the measured worst overshoot, and 05 states the bound.
- **Core dumps.** Every worker sets `RLIMIT_CORE = 0`.
- **Hash seed.** `PYTHONHASHSEED=0` is set for every process on every OS, so a call's result does
  not depend on the process.
- **Unix spawning.** The worker sets its own `rlimit`s after warm-up and before reading the call,
  from values the front passes on its command line. `pool.py` spawns workers with `subprocess`,
  inside the sandbox they inherit.
- **Windows spawning: the broker.** On Windows the front cannot start processes at all (D4: child
  process policy). It asks the broker for a worker, over a pipe the launcher gave it. The broker
  is the `imperium-core confine` launcher, outside the AppContainer.
  - **The broker contract (R2-1).** The front is treated as untrusted by the broker.
    - **Requests.** A request is one of three fixed opcodes: `spawn`, `arm <worker-id>` and
      `kill <worker-id>`. A message is at most 64 bytes; a longer one, an unknown opcode or any
      extra field closes the broker pipe, and the server's job is then killed.
    - **What the broker decides itself.** It builds the worker's command line (the base
      interpreter, `-I -B -m imperium_cas.worker` and the limits), its environment and its limits
      from its own launch configuration, which the daemon gave it on its command line. Nothing in
      a request can name a path, an argument, a variable or a limit value.
    - **Worker ids.** `spawn` returns a worker id that the broker issued (a counter), plus the
      worker's PID for information. `arm` and `kill` take only an id that this broker issued for a
      worker that is still alive; a PID or any other number is refused.
    - **Creation.** The broker creates the worker in the same AppContainer, with
      `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY` (restricted) and
      `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so the worker is in its own job, nested in the server's
      job, from its first instruction. The worker's handle list holds only its own pipe ends,
      never the broker pipe.
    - **Default limits.** Each worker job is created with conservative limits: the configured
      CPU limit plus a warm-up allowance, and the configured memory limit plus the measured
      warm-up memory.
    - **`arm`.** Sent once the worker reports ready. The broker reads the worker's CPU time with
      `GetProcessTimes` and its committed memory, then sets the baseline: CPU used + the CPU
      limit, and memory after warm-up + the memory limit, the same baseline as Linux. `arm` may
      only lower a limit or set the baseline, never raise one above the default.
    - **Wall clock.** The broker keeps its own wall clock for each worker, counted from `arm`
      (or from `spawn` for a worker never armed) and at most the configured cap. When it runs
      out the broker terminates the worker's job, whatever the front does.
    - **Hand-over.** The broker hands the worker's pipe ends to the front with
      `DuplicateHandle`.
  - All of this is in `imperium-ipc` (F1, [D4](#d4-sandbox-a-fixed-allowlist-profile-for-the-cas)).
    Because the job is assigned at creation with default limits, there is no window in which the
    worker runs unlimited, and the nested-job question is settled by the broker being unconfined
    (stage 5 verifies it).
- **The interpreter's PID.** The front and workers are started with the **base interpreter** of
  the managed Python (`<env>/python/.../python[.exe]`), not the venv's `python`, which on Windows
  is a redirector that starts a second process. The managed environment is reached through a
  `.pth` file (D5).
  - Stage 2a asserts that a spawned worker's `os.getpid()` equals the PID the spawner holds (with
    the base interpreter on all three OSes). It also asserts the venv redirector does *not* on
    Windows, to document why.
  - Stage 5 asserts the same through the broker.
  - The Windows server job's active-process limit is set from what stage 5 measures: the front,
    one running worker and one spare, so 3 if nothing else is seen.
- **Queueing and cancellation.** Calls from one session's server run one at a time, in order. A
  call's deadline starts when the front receives it, so time spent queued counts against its wall
  clock. A call that has no time left when its turn comes fails `limit: wall clock (queued)`. An
  MCP `notifications/cancelled` for a running call kills its worker.
- **Messages and recovery.** A worker that dies with a limit exceeded gives `limit` and names the
  limit and its value. A worker that dies otherwise (a signal, a non-zero exit that is not a
  limit or `REFUSED`) gives `crashed` with the exit status. The next call takes the spare, so
  nothing waits on a restart.
- **Pipe framing.** Requests and replies are length-prefixed JSON. The reader checks the prefix
  against the cap (1 MiB for a request, 256 KiB for a reply) before allocating anything, and
  treats a bigger prefix as `crashed`.
- **Lazy start.** The spare is started on the first tool call, not when the session starts, so an
  idle session pays only for the front (about 30 MB; estimate). The first call pays SymPy's
  import time, about 1 s (estimate, measured in stage 2a).

Rejected alternatives:

- **A long-lived worker pool, killed and restarted on a breach.** CPU limits are cumulative per
  process, so per-call CPU needs polling. Memory and SymPy's caches grow across calls, and one
  call's state (the cache, assumptions) can leak into the next.
- **A fresh cold process per call.** It pays the full import (about 1 s, estimate) on every call.
- **Limits inside one process** (signals, threads). `SIGALRM` does not interrupt C-level loops
  in `gmpy2` or `mpmath`, and Windows has no `SIGALRM`.
- **Windows Job Objects from Python `ctypes` in the front.** The job could only be assigned after
  the process starts, the front would need the right to create processes (so a compromised front
  could start anything inside the AppContainer), and the Win32 code would be duplicated outside
  `imperium-ipc`.
- **A front written in Rust** (`imperium cas-serve`). The roadmap fixes a Python project with the
  MCP Python SDK, and a Python server stays usable by other MCP clients on Linux and macOS (on
  Windows it needs Imperium's broker).

Revisit if the measured first-call latency or memory per session is a problem: share one server
across sessions, or keep more than one spare.

### D4. Sandbox: a fixed allowlist profile for the CAS

**Accepted: the daemon starts the CAS server through a new allowlist confinement profile,
`imperium-tools::confine`. It is tier 1 (OS sandbox) technology, but with a fixed CAS profile
that ignores the session's `isolation.tier`, `isolation.network` and folders.**

The server and every process it starts get:

- read and execute access to the environment root (`<data>/runtimes/cas/<env-digest>/`);
- a private temporary folder;
- an explicit minimal environment: `PATH` (the environment's own folders only), `LANG`,
  `PYTHONHASHSEED=0`, and on Windows `SYSTEMROOT` and `TEMP`/`TMP` (the AppContainer's own
  temporary folder).

They get no workspace, no network, no home folder, no `<config>` or `<data>` beyond the root, and
none of the daemon's environment. The profile is the same in every session tier, because the CAS
touches no workspace. In the container tier it still runs on the host, so Docker is not needed.

**Linux.** `bwrap` is resolved by absolute path only: `/usr/bin/bwrap`, then `/bin/bwrap`, never
through `PATH`. It starts from an empty root:

- `--unshare-all` (network, PID, IPC, UTS and cgroup), `--die-with-parent`, `--new-session` and
  `--clearenv`, then `--setenv` for the minimal environment;
- `--ro-bind` of the environment root **at the same absolute path** (so the `.pth` paths and
  Landlock rules hold inside and out), and of `/usr`, `/lib`, `/lib64` and `/lib32` as they
  exist (or their merged-`/usr` links); nothing from `/home` or `/etc`;
- `--proc /proc`, `--dev /dev` and `--tmpfs /tmp`; the working folder is `/`.

This is stricter than the commands' OS tier, which starts from `--ro-bind / /` with masks.

Inside, the front applies a **Landlock** ruleset (`landlock.py`, raw syscalls through `ctypes`,
no agent data). The ruleset handles `LANDLOCK_ACCESS_FS_EXECUTE` and allows it only beneath the
environment root, so neither the front nor a worker can `execve` anything in `/usr/bin` (a
shell, `curl`). Workers inherit the ruleset.

- **Timing (R2-3).** The ruleset is applied by the first statement of `imperium_cas.__main__`,
  before any other import (the MCP SDK, `pool`) and before stdin is read. `landlock.py` imports
  only `ctypes`, `os` and `sys`.
- **Fail closed when available.** The front checks the Landlock ABI first. If the ABI is present
  but creating the ruleset, adding the rule or `landlock_restrict_self` fails, the front exits
  with an error before reading stdin. The session then reports the server as failed to start.
- **Residual.** Where Landlock is unavailable (a kernel before 5.13, or Landlock disabled), the
  front carries on, and a process could still run `/usr` programs. Those would still have no
  network, no home folder and no daemon socket.
- **How the note reaches `mcp.list`.** stderr is discarded, so the front reports it in the MCP
  `initialize` reply. Its `serverInfo` carries `name`, `version` and a `confinement` value
  (`landlock` or `no-landlock`), and its `instructions` field holds a one-line note: "Landlock
  unavailable: programs in /usr can be started inside the sandbox". `imperium-mcp` does not read
  either field today. Stage 7 makes its `initialize` keep the reply's `serverInfo` on the started
  server; this is a change to `initialize`, not to the spawn path. The daemon stores the
  `confinement` value in the built-in's status, and `mcp.list` reports the CAS as `ready` with
  the note in `notes`. Nothing passes `instructions` to the model.
- **Unavailable.** `available()` returns `Unavailable` with a reason when `bwrap` is missing or
  unsharing fails (for example Ubuntu 24.04's AppArmor restriction of unprivileged user
  namespaces without a `bwrap` profile). The desktop packages declare `bubblewrap` as a
  dependency (`bundle.linux.deb.depends` and `rpm.depends` in `tauri.conf.json`).

**macOS.** `/usr/bin/env -i <minimal variables> /usr/bin/sandbox-exec -p <profile> -- <python>
...`, both by absolute path. `env -i` starts from an empty environment and sets only the minimal
variables, so the environment really is minimal whatever the launching process had. The profile
is deny-default and allows:

- `process-exec` and `file-read*` under the environment root;
- `file-read*` of `/usr/lib`, `/System` and the dyld cache, and of `/dev/null` and
  `/dev/urandom`;
- `file-read-metadata` of the **ancestors** of those roots only, each as a `literal`, because
  Python needs `stat` on path ancestors;
- `file-write*` under its private temporary folder only;
- `process-fork` and `sysctl-read`;
- `(allow signal (target same-sandbox))` and `(allow process-info* (target same-sandbox))`, so
  the front can kill a worker and read its `proc_pid_rusage`.

It denies `network*`. Seatbelt is inherited by every child, and `process-exec` outside the root
is denied.

**Windows.** The first AppContainer in Imperium. The daemon re-executes itself as
`imperium-core confine --profile cas -- <python> ...`. That launcher:

1. creates or opens the AppContainer profile `Imperium.Cas` with no capabilities (so no
   `internetClient`; loopback is closed to AppContainers by default);
2. creates the server's Job Object with `KILL_ON_JOB_CLOSE`, the measured active-process limit,
   a job memory limit (the memory limit × 2 + 512 MiB) and all UI restrictions;
3. starts the front with `CreateProcessW`, using `STARTUPINFOEX` with:
   - `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`;
   - `PROC_THREAD_ATTRIBUTE_JOB_LIST`;
   - `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY` (restricted);
   - a handle list holding only its standard handles and the broker pipe;
   - an explicit environment block with the minimal environment;
4. serves the broker requests of D3, and exits with the front's exit code.

Killing the launcher (`kill_on_drop` in `imperium-mcp`) closes the job and kills the tree. File
access is granted at setup by ACL: read and execute on the environment root for the container's
SID only. User folders grant nothing to AppContainers, so the home folder, workspaces and `<data>`
are unreadable.

**Where the unsafe code lives (F1).** The workspace forbids `unsafe` (`Cargo.toml`,
`unsafe_code = "forbid"`), and `imperium-tools` and `imperium-core` inherit that. Every Win32
call goes into a new module, `crates/imperium-ipc/src/windows/confine.rs`, under the crate's
existing `#![allow(unsafe_code)]` pattern (`windows.rs`), with a soundness note on each `unsafe`
block. That covers the AppContainer profile and SID, the ACL grant, `STARTUPINFOEX`, the
attribute list, `CreateProcessW`, `DuplicateHandle`, the jobs and `GetProcessTimes`.

The module exposes safe wrappers:

- `AppContainer::open_or_create(name)`;
- `grant_read_execute(sid, root)`;
- `spawn_confined(spec) -> ConfinedChild`;
- `Broker::serve(pipe, limits)`.

`imperium-tools::confine::windows`, `imperium-core`'s `confine` entry and `imperium-local`'s
setup call only these. `windows-sys` is already an `imperium-ipc` dependency, which gains the
`Win32_System_JobObjects`, `Win32_System_Diagnostics_ToolHelp`, `Win32_Security_Isolation` and
`Win32_Storage_FileSystem` features. `imperium-tools` today has only `win32job` (a safe wrapper)
for Windows, and gains a dependency on `imperium-ipc`, whose only other dependency is `tokio`.

Linux and macOS need no `unsafe` Rust: the launch is `bwrap` or `sandbox-exec` arguments, and
the Landlock calls are in the Python front. The Rust probe in stage 4 restricts itself with the
same execute rule through the safe [`landlock`](https://crates.io/crates/landlock) crate, which
is a dev-dependency of `imperium-tools` on Linux only (R2-4). No shipped crate gains it.

- **Fail closed (decided).** If the sandbox is unavailable, the server is not started and the
  session gets a notice with the reason. There is no setting that runs the CAS unconfined.
- **Tests.** `crates/imperium-tools/tests/confine.rs` runs a probe binary (a Rust test helper,
  so the test needs no Python) under the CAS profile on the matrix. The probe tries each goal 3
  action itself, and then starts a copy of itself as a child (Unix) or asks the broker for one
  (Windows), which tries them again. On Windows the test also checks that the probe cannot start
  a process directly, and that killing the launcher kills the probe and its child. The probe
  reads the allowed root and a canary variable that the test sets in its own environment.
  `IMPERIUM_REQUIRE_CONFINE=1` in CI turns "unavailable" into a failure.
- **Linux CI on two kernels.** A small `confine (ubuntu-24.04)` job runs only
  `cargo test -p imperium-tools --test confine` on `ubuntu-24.04`, with
  `IMPERIUM_REQUIRE_CONFINE=0`. It asserts that the profile either works or reports
  `Unavailable` with a reason, never runs unconfined. A unit test forces `available()` down the
  "unsharing fails" path with an injected probe command and checks the reason text.

Rejected alternatives:

- **Running the server under the session's own tier.** Tier 0 confines nothing, Windows has no OS
  tier, and the commands' OS profile can read the whole filesystem by design.
- **The container tier.** It needs Docker for a maths tool.
- **Python-level sandboxing** (audit hooks alone). Hooks are defence in depth (D1), not a
  boundary against code that is already running.
- **Landlock from a Rust launcher before `bwrap`.** Landlock needs `no_new_privs`, which breaks a
  setuid `bwrap`, and the rules would have to allow executing `bwrap` itself.

Revisit when the general Windows OS tier lands, which can reuse `imperium-ipc`'s confine module.
Also revisit if the `confine (ubuntu-24.04)` job shows `Unavailable` on stock runners
([risk 1](#risks-across-stages)).

### D5. Process model, managed environment and setup

- **The sidecar.** `sidecars/cas/` is a `uv` project with:
  - `pyproject.toml` with `[tool.uv] package = false`: the lock holds dependencies only, so no
    build backend runs and `--no-build` holds;
  - a committed `uv.lock` with hashes;
  - a `.python-version` with an exact version (3.12.x, chosen in stage 1);
  - runtime dependencies pinned exactly: `sympy`, `mpmath` and `mcp` (the MCP Python SDK). The
    lock also carries `mcp`'s own dependencies: `anyio`, `httpx`, `httpx-sse`, `pydantic`,
    `pydantic-core`, `pydantic-settings`, `jsonschema`, `python-multipart`, `sse-starlette`,
    `starlette`, `uvicorn`, and `pywin32` on Windows. Only the stdio server is started; no HTTP
    listener is.
  - dev dependencies `pytest`, `hypothesis` and `ruff`.

  The code is the `imperium_cas` package under `src/`.
- **How it ships.** The sidecar's files (`pyproject.toml`, `uv.lock`, `.python-version` and
  `src/**`, not its tests) are embedded in the daemon with `include_str!` from a list in
  `imperium-local/src/cas_env.rs`. A test checks that the list matches the tracked files, so the
  CLI archive needs no resource folder. `include_str!` of a path outside the crate breaks
  `cargo package`. Imperium's crates are not published; a crate that ever is moves the files
  under itself.
- **Two digests (decided).**
  - The **environment digest** is the SHA-256 of `uv.lock`, `.python-version`, the pinned `uv`
    version and the platform triple. It names the folder `<env-digest>/`.
  - The **source digest** is the SHA-256 of `pyproject.toml` and `src/**`, recorded in
    `managed.json`.
  - After an Imperium update:
    - a new **source** digest only rewrites `<env-digest>/project/` with no download, no
      approval and no notice;
    - a new **environment** digest rebuilds in the background under the approval already given,
      with a notice that states the download size. The old folder is removed once the new one is
      recorded.
- **Layout.** Everything lives under `<data>/runtimes/cas/`:
  - `uv/<uv version>/uv[.exe]`: the pinned `uv`, downloaded from its GitHub release and verified
    against the per-platform SHA-256 in `runtimes/cas.env`. `uv` on `PATH` is never used.
  - `<env-digest>/project/`: the embedded files, written out.
  - `<env-digest>/python/`: `UV_PYTHON_INSTALL_DIR`, uv's managed CPython of the exact version.
  - `<env-digest>/venv/`: `UV_PROJECT_ENVIRONMENT`.
  - `setup.lock`: an exclusive OS file lock (`std::fs::File::lock`) held by whoever runs setup,
    *Remove* or a rebuild. The daemon and `imperium runtime cas` both take it. A second caller
    reports "setup is already running" (CLI) or waits (daemon).
  - `managed.json`: written last, as for vLLM. It holds both digests, the uv and Python
    versions, the size, the setup time and `approval` (`approved`, `declined` or absent).
- **The `.pth`.** After `uv sync`, Imperium writes `imperium_cas.pth` into the **base
  interpreter's** `site-packages` (under `<env-digest>/python/`) with two lines: the venv's
  `site-packages` and `<env-digest>/project/src`. The base interpreter started with `-I` (which
  keeps the interpreter's own `site-packages`) then imports both `imperium_cas` and its
  dependencies. This serves D3's PID rule and `package = false` at once.
- **The setup command.** It runs without a shell through the vLLM `Runner` pattern:

  ```text
  uv sync --frozen --no-build --no-dev --python-preference only-managed \
    --compile-bytecode --project <env-digest>/project
  ```

  - **The environment is an allow-list, not inherit-and-subtract.** It has only:
    - our own `UV_CACHE_DIR` (a temporary folder, deleted afterwards), `UV_PYTHON_INSTALL_DIR`
      and `UV_PROJECT_ENVIRONMENT`;
    - `UV_NO_CONFIG=1` and `UV_PYTHON_DOWNLOADS=automatic`;
    - `UV_PYTHON_INSTALL_REGISTRY=0` and `UV_PYTHON_INSTALL_BIN=0` (R2-2). Without them, `uv`'s
      managed Python install on Windows registers itself under `HKCU\Software\Python` (PEP 514),
      and on every OS places `python3.x` executables in uv's bin folder (`~/.local/bin`). Either
      would make the CAS's interpreter visible to other programs outside `<data>`;
    - from the caller, only the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and
      `NO_PROXY` in both cases), the CA bundle (`SSL_CERT_FILE`, `SSL_CERT_DIR`,
      `UV_NATIVE_TLS`) and index credentials (`UV_INDEX_<NAME>_USERNAME` and `_PASSWORD`);
    - on Windows, `SYSTEMROOT`, `TEMP`, `TMP` and `USERPROFILE`.

    Every other variable is dropped, including `UV_PYTHON_DOWNLOADS_JSON_URL`,
    `UV_PYTHON_INSTALL_MIRROR`, `UV_INDEX_URL`, `UV_EXTRA_INDEX_URL`, `UV_INSECURE_HOST` and
    every other `UV_*` and `PIP_*`. Setup lists the names of the variables it passed, never their
    values.
  - **Integrity.** Packages are checked against the lock's hashes. The Python download is checked
    against the SHA-256 checksums that the pinned `uv` release embeds for each python-build-
    standalone build, so its integrity rests on the `uv` binary's pinned digest.
  - **Output and failure.** The output is shown line by line with URL credentials masked. Setup is
    killed after 15 minutes, and a failure removes `<env-digest>/`.
  - **Windows.** The last step grants the AppContainer SID read and execute on `<env-digest>/`,
    through `imperium-ipc`.
  - **Size.** About 70 MB downloaded and about 250 MB on disk (estimates: `uv` about 18 MB,
    CPython about 30 MB, the wheels about 20 MB, of which `pywin32` is about 10 MB on Windows).
    The estimate is shown before asking and measured in stage 6.
- **When setup runs (first use, after asking).** The CAS is on by default
  (`mcp.builtin.cas.enabled = true`). When a session would start it and no environment for the
  current environment digest exists:
  - **Interactive local session, never answered.** A session created by an interactive local
    client (the desktop app, or the CLI or TUI chat on this machine) raises one approval through
    the existing `approvals` queue (kind `builtin.setup`). Its summary says what is downloaded,
    its size and the folder, and every client shows it as it shows push approvals.
  - **Unattended, headless or remote session (decided).** Scheduled, auto-fix and woken sessions,
    `imperium run --headless`, and sessions a remote or phone client created never raise it. They
    start without the server and leave a non-blocking hint in Connectors (`mcp.list` state
    `not_set_up` with "Set up to give agents maths tools").
  - **Until it is answered,** sessions start without the server, with a notice.
  - **Approve:** setup runs in the background and `mcp.builtin_changed` announces each state.
    Running sessions keep their servers, and new sessions start it once it is ready.
  - **Decline:** `managed.json` records `approval: declined` and nothing asks again. Connectors
    and `imperium runtime cas setup` stay available.
  - **Remove** (Connectors or CLI): deletes the environment and clears `approval`, so the next
    interactive first use asks again.
  - **CLI.** `imperium runtime cas setup [--yes] | status | remove` works like `runtime vllm`
    (asks, and `--yes` is the only approval without a terminal). An agent's command that runs it
    is left to a person to approve, as the other install commands are.
  - **Deviation from 04, stated there.** vLLM is set up only from the CLI. The CAS is also set up
    from the daemon after an approval, because it is a built-in that interactive users meet in
    the app. Both share the pinned, recorded, shell-free setup.
- **The launch.** `[<base python>, "-I", "-B", "-m", "imperium_cas"]` (isolated mode: no
  `PYTHON*` variables, no user site, no current folder on the path), wrapped by D4's profile.
  The `ServerSpec` carries `CLEAN_ENV` in its `env`, so `imperium-mcp` starts the launcher with a
  cleared environment plus `plugin_child_env()`. The profile then narrows that to the minimal
  environment.
- **In sessions.** `imperium-core/src/mcp.rs::session_servers` adds a `ServerSpec` named
  `imperium_cas` when the CAS is enabled, set up and the sandbox is available. It is called from
  `state/create.rs`, which stage 7 owns for this.
  - `allow` is `["*"]` by default, because the tools have no side effects and run sandboxed.
    `mcp.builtin.cas.ask = true` makes each call ask.
  - Permission rules and agent profiles key on the server name `imperium_cas` (a rule
    `mcp__imperium_cas__*`, or `allows_mcp(Some("imperium_cas"))`). Deny rules still apply.
  - Timeout is the wall-clock limit + 10 s.
- **Name reservation (F9).** The name `imperium_cas`, and any name starting with `imperium_`, is
  reserved for built-ins. A server with such a name is refused with a notice that names its
  source, whether it comes from:
  - the user config (`mcp.save` and `mcp.install` refuse it too, with a validation error);
  - an environment's config file;
  - a trusted project's `.mcp.json`.

  The built-in is still started. The notice names the source and tells the user to rename the
  server (an existing `imperium_*` server from before this change is not started until it is
  renamed). Stage 7 tests each source.
- **In Connectors.** `mcp.list` reports the built-in as a server entry with a new `builtin` field:
  id, state (`not_set_up`, `setting_up`, `ready`, `failed`, `declined`, `unavailable`), reason,
  notes (such as the Landlock note), size and version. The app shows it in a **Built in** group
  with *Set up*, the switch and *Remove*:
  - the switch is `mcp.set_enabled`, which writes `mcp.builtin.cas.enabled` for a built-in name;
  - *Remove* calls `mcp.builtin_remove`.

  It cannot be edited or have its command changed.

Rejected alternatives:

- **`uv` on `PATH`, as vLLM does.** It is fine for a GPU runtime aimed at experts, but a built-in
  maths tool should not need a toolchain installed, and a pinned `uv` pins both `uv sync`'s
  behaviour and the Python checksums.
- **Inheriting the caller's environment minus some variables.** Any `UV_*` variable added in a
  later `uv` release would pass by default.
- **Installing the project itself into the venv.** It needs a build backend, which conflicts with
  `--no-build`.
- **Shipping the sidecar as a Tauri resource folder.** The CLI archives have no resources, and an
  embedded copy cannot drift from the daemon that launches it.
- **Installing silently on first start.** The roadmap and the vLLM precedent require asking.
- **Offering the tools before setup and installing on the first call.** It needs a static tool
  list and a lazy server in `imperium-mcp`, which is more machinery for little gain.

### D6. Tools, schemas and outputs

There are eleven tools; an MCP client sees them as `mcp__imperium_cas__<name>`. Each takes
expressions in D1's language plus an optional `assume` object.

Every tool declares an `inputSchema` and an `outputSchema`. Schemas set:

- `additionalProperties: false`;
- enum and integer bounds;
- `maxLength` 16384 on expression strings;
- the identifier `pattern` on every name, including `propertyNames` for `assume` and `at`.

| Tool | Input (besides `assume`) | Result |
| --- | --- | --- |
| `simplify` | `expression` | simplified expression |
| `expand` | `expression` | expanded expression (guard: exponent ≤ 10,000) |
| `factor` | `expression`, `gaussian` (bool) | factored expression (integers too, up to 100 digits: `factor("360")` gives `2^3*3^2*5`) |
| `solve` | `equations` (1–10 strings; a bare expression means `= 0`; relations allowed), `variables` (optional names), `domain` (`complex` default, or `real`) | a solution set, stated with its domain: `solveset` for one equation or inequality, `linsolve` for linear systems and `nonlinsolve` for other systems, over `S.Complexes` or `S.Reals`; inequalities are real only, so a complex-domain inequality is an error saying so; a `ConditionSet` result is reported as "not solved" with the condition |
| `differentiate` | `expression`, `variable`, `order` (1–20) | derivative |
| `integrate` | `expression`, `variable`, `lower`, `upper` (both or neither) | antiderivative (no `+ C`, stated in the description) or the definite value; an integral SymPy cannot do is reported as `unevaluated`, with a hint to use `evaluate` for a definite one |
| `limit` | `expression`, `variable`, `to` (expression; `oo` allowed), `direction` (`+`, `-`, `both`) | limit, or `does not exist` |
| `series` | `expression`, `variable`, `at` (default `0`), `order` (1–50) | series with its `O()` term |
| `matrix` | `matrix` (an expression whose value is a matrix: literals with `+ - *` and `^` of an integer), `operation` (`value`, `det`, `inverse`, `transpose`, `rank`, `rref`, `nullspace`, `eigenvalues`, `eigenvectors`, `charpoly`, `solve`), `rhs` (for `solve`) | matrix, scalar or list |
| `evaluate` | `expression`, `digits` (1–1000, default 15), `at` (object: name → expression) | decimal value, by `evalf(n=digits, subs=at)` |
| `equal` | `left`, `right` (expressions, or matrices of the same shape) | `true` (`symbolic`), `false` with a counterexample point (`numeric`), or `unknown` |

**`solve` (decided).** Both domains use the same solver family (`solveset`, `linsolve`,
`nonlinsolve`); only the domain set changes. Every result states its domain, for example
`{-2, 2} (complex)`, so a model never mistakes a complex answer for a real one. The default stays
`complex`, SymPy's own.

**`equal`.** It tries `simplify(left - right) == 0`, then SymPy's `equals`, then 10 random points
at 30 digits under the assumptions. The points come from a `random.Random` seeded with a fixed
value, so results are reproducible. A difference over 10<sup>-20</sup> gives `false` with the
point. Agreement at every point without a proof is reported as `unknown` ("agrees at 10 random
points"), never `true`.

**Assumptions** are an object from a symbol name to a list drawn from the enum `real`,
`positive`, `negative`, `nonnegative`, `nonpositive`, `nonzero`, `integer`, `rational` and
`finite`, for example `{"x": ["real", "positive"]}`. The symbols are created with those SymPy
assumptions. A contradictory list is a `math` error. Without `assume`, symbols are SymPy's
default (complex). `solve`'s `domain` is separate, because it changes the solution set, not the
symbols.

**Output.** Each result has one MCP text item, so Imperium's client (which passes text to the
model) shows both forms:

```text
{-2, 2} (complex)
LaTeX: \left\{-2, 2\right\}
```

It also has `structuredContent` `{ "text", "latex", "kind", "domain"?, ... }`, where `kind` is
`expression`, `set`, `solutions`, `matrix`, `boolean`, `number` or `unevaluated`. The text form is
SymPy's `sstr` with `**` replaced by `^`, and it is parseable by D1's language: stage 3
round-trips every corpus result.

**Errors.** Errors are MCP tool errors (`isError: true`) with one line that starts with a
category, then a short reason and, for `parse`, the column and a caret line. The categories:

- `parse` and `guard`: the input was refused;
- `limit`: a resource limit was hit;
- `crashed`: the worker died without a limit;
- `refused`: the audit hook stopped it;
- `math`: SymPy raised an error.

Tool descriptions are short (one sentence plus a syntax line), because they count against small
models' context. Stage 3 measures the full `tools/list` in tokens and keeps it under 1,500
(estimate).

### D7. Testing entirely in CI

- **The `python-cas` job** is new, so the media sidecar keeps its own Ubuntu-only `python` job:
  - `scripts/ci/detect.sh` adds a `python_cas` output (`sidecars/cas/pyproject.toml` exists), and
    `scripts/ci/test-detect.sh` gains a case for it.
  - `scripts/check-python.sh` takes the sidecar's name (`media` by default, so the existing job is
    unchanged). For `cas` it runs `uv sync --locked --no-build`, `uv run ruff check .`,
    `uv run ruff format --check .` and `uv run pytest`.
  - The job runs on a three-OS matrix (`ubuntu-latest`, `windows-latest`, `macos-latest`),
    because the limits are per OS. `uv` comes from the already pinned `astral-sh/setup-uv` action,
    and Python from uv's managed download of the exact `.python-version`, so CI and users run the
    same interpreter.
  - pytest covers the parser and its refusals, the guard, the hostile corpus and fuzz run, every
    tool, the audit hook, the PID rule, and the limits on Linux and macOS. Those limits are tested
    against the worker directly, without the sandbox. The deterministic limit tests start
    test-only worker modules from `tests/` (CPU spin, fast allocation, blocking sleep) that call
    the same `limits` code. The production worker has no payload path.
- **The `rust` job** (already a matrix) gains:
  - `imperium-tools --test confine` (stages 4 and 5, with the Rust probe);
  - the `imperium-ipc` broker and job tests (stage 5);
  - the `cas_env` fake-runner tests (stage 6);
  - `imperium-core --test cas_mcp` (stage 7). This test runs the **real** setup into a scratch
    data folder (the pinned `uv` download, Python and `uv sync --frozen --no-build`, then the
    `.pth`). It then starts a daemon and a session and calls `mcp__imperium_cas__solve` through
    the session's MCP client, under the real confinement. It also has three limit cases: a CPU
    trip, a memory trip and a wall-clock trip, each followed by a successful call. The scratch
    `runtimes/cas` is cached with `actions/cache`, keyed on `runtimes/cas.env` and the
    environment-digest inputs, so most runs skip the downloads. `IMPERIUM_REQUIRE_CAS=1` is set
    in `ci.yml`, so the test fails rather than skipping in CI and skips elsewhere. Downloads
    retry three times with backoff.
- **The `confine (ubuntu-24.04)` job** (D4).
- **Presence end-to-end.** `detect.sh`'s presence paths include `crates/imperium-ipc/`, so
  stage 5's pull request also runs `presence-e2e`, as expected.
- No local runs are needed. Developers may run `bash scripts/check-python.sh cas`.

### D8. Protocol

The additions are additive: two methods, one notification and one optional field. The change
takes the **next protocol minor at merge** of stage 7. See
[protocol additions](#protocol-additions).

## Current state (found on `experimental` at `ddcff4c9`)

Implemented:

- **MCP client** (`crates/imperium-mcp`):
  - Stdio servers are started per session in `servers.rs::start` with
    `tokio::process::Command`, `kill_on_drop` and stderr discarded.
  - The `GH_*` variables are removed, and a cleared environment plus `plugin_child_env()` is used
    when the spec's `env` holds `CLEAN_ENV` (plugins today).
  - Tools are offered as `mcp__<server>__<tool>`, approved unless `allow` lists them, and the text
    content of results reaches the model.
- **Server choice** (`crates/imperium-core/src/mcp.rs::session_servers`, called from
  `state/create.rs`): `[mcp.servers]` from the user config plus an environment's file, then a
  trusted project's `.mcp.json`, with name collisions reported as notices. Evaluation sessions get
  no servers. Names are 1–40 of `[a-z0-9_-]`, and none is reserved.
- **Connectors** (`crates/imperium-core/src/connectors.rs`, `catalog/connectors.json`,
  `crates/imperium-protocol/src/methods/connectors.rs`): `mcp.list`, `mcp.set_enabled` and the
  others, plus the directory of add-able servers. There is no notion of a built-in server.
- **Approvals** (`crates/imperium-core/src/approvals.rs`): a generic queue with a string `kind`
  and `approval.requested`; no protocol change is needed for a new kind.
- **Managed vLLM environment** (`crates/imperium-local/src/vllm_env.rs`,
  `crates/imperium-cli/src/vllm.rs`, `runtimes/vllm.env`): a `Runner` without a shell, a
  `managed.json` record written last, pins in a `runtimes/*.env` file, and setup from the CLI
  only.
- **Verified downloads** of llama.cpp builds (`crates/imperium-local/src/runtime.rs`, SHA-256
  checked against GitHub's published digests).
- **OS isolation tier** (`crates/imperium-tools/src/sandbox.rs`): bubblewrap and Seatbelt for
  commands, run through a shell, starting from `--ro-bind / /` with masks. The `os_sandbox.rs`
  tests run in CI with `IMPERIUM_REQUIRE_OS_SANDBOX`.
- **Unsafe code:** forbidden in the workspace. `imperium-ipc` allows it per module
  (`windows.rs`, `macos_sandbox.rs`, `macos_presence.rs`) with a soundness note per block.
  `imperium-ipc` depends on `windows-sys` 0.61 (Win32 security, threading, pipes).
  `imperium-tools`' only Windows dependency is `win32job`.
- **CI:** the `python` job is keyed on `sidecars/media/pyproject.toml` (Ubuntu only,
  `scripts/check-python.sh`). The `rust` matrix runs `scripts/check-rust.sh` on `ubuntu-22.04`,
  Windows and macOS.

Missing:

- `sidecars/` does not exist; no Python code is in the repository.
- MCP servers and hooks are never confined ([05](../05-tools-and-isolation.md#mcp), planned).
  Windows has no OS sandbox at all, and no AppContainer code exists.
- No built-in MCP server, no reserved server names, and nothing that sets up a Python environment
  from the daemon.
- The Linux packages do not declare `bubblewrap`.

## Scope and non-goals

In scope:

- the sidecar and its tools, the parser, guard and limits;
- the `confine` profile on three OSes, with its Win32 module in `imperium-ipc`;
- the managed environment and its setup;
- daemon wiring and name reservation;
- the protocol, the CLI and desktop surfaces, CI and docs.

Non-goals (each with what would reopen it):

- **Confining other MCP servers, hooks, or Windows commands.** `confine` is built so that they can
  reuse it. Reopen with the roadmap work on running local servers inside the isolation tier.
- **A prompt note telling models to prefer the CAS.** It would touch the engine's prompt files,
  which another session owns. Reopen after evaluations show models ignore the tools.
- **Evaluation tracking of CAS use.** Evaluation sessions start no MCP servers, and the
  evaluation code is owned elsewhere.
- **ODEs (`dsolve`), plotting, units, statistics, number theory beyond the table, LaTeX or JSON
  input, and an HTTP transport.** Reopen on demand.
- **Sharing one server across sessions.** Reopen if the per-session memory (D3) is a problem.
- **An unconfined fallback** (decided: fail closed).
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
- the PyPI, GitHub release and python-build-standalone servers during setup, and any proxy or
  mirror on the way;
- other code of the same user at tier 0 (out of scope as a boundary, as in the
  [plugin marketplace plan](plugin-marketplace.md#threat-model)).

1. **Code execution through the expression parser.** Mitigations: D1's own grammar and table
   builder (no `eval` anywhere), the identifier check on every name parameter, the AST scan, the
   no-string-to-SymPy wrapper and the `lambdify` ban, the hostile corpus and fuzz, and the
   allow-list audit hook that exits inside itself. Residual: a SymPy bug that turns a `Basic`
   into code execution with only allow-listed audit events. The sandbox (threat 3) bounds that
   case.
2. **Denial of service.** Mitigations:
   - D1's caps and D2's guard;
   - single-use workers with CPU, memory and wall-clock limits that include time spent queued
     (D3);
   - the output and pipe caps;
   - the Windows job memory and process limits, and the `bwrap` PID namespace.

   One session's runaway call does not affect another's server. Residual: within the limits, a
   model can spend 15 s of CPU per call. The session's own loop and budgets still apply.
3. **A compromised server process** (through a SymPy or `mpmath` bug, or a tampered environment).
   Mitigations:
   - the D4 profile, inherited by every child, with no network, no files outside its root, no
     daemon environment, a private temporary folder, and no way to reach the daemon's socket or
     token;
   - on Windows, no process creation at all;
   - on Linux with Landlock, and on macOS, no execution outside the root.

   The environment lives under `<data>`, which the OS tier guards read-only from commands. A
   tier-0 command can already do anything as the user, so tampering by it is out of scope.
   Residual:
   - kernel or sandbox escapes;
   - Linux without Landlock (running `/usr` programs inside the sandbox);
   - on macOS, the metadata of the allowed roots' ancestors.
4. **Supply chain at setup.** Mitigations:
   - the `uv` binary is checked against the SHA-256 in the repository;
   - the Python download is checked against the checksums that pinned `uv` embeds;
   - the packages come from the committed lock with hashes, installed `--frozen --no-build`,
     with `package = false`, so no build script or backend runs at install;
   - an allow-listed environment, so no variable can point `uv` at another Python source or index
     URL. Index credentials and proxies pass, but cannot change content, because the hashes are
     checked.
   - the setup lock, so two setups never interleave.

   Residual: a malicious release of a pinned package published before pinning. Dependabot
   proposes updates, and a person reviews them.
5. **Misleading maths.** An `unknown` equality is reported as unknown, never as true. An
   unevaluated integral or unsolved system is reported as such, solution sets state their domain,
   and floating-point values come only from `evaluate`, with their digits stated.
6. **Prompt injection through results.** Results are SymPy output from the agent's own input. Any
   symbol name in them came from the input and matched the identifier pattern, and the length is
   capped. Results are framed as MCP tool results, like any other.
7. **Name squatting.** A project's `.mcp.json` or an environment file naming a server
   `imperium_cas` could inherit the built-in's `allow = ["*"]` and permission rules. The reserved
   prefix refuses it in every source (D5).
8. **Approval and setup abuse.** The approval only installs pinned, hash-locked content, so
   approving it by mistake is low impact. Unattended, headless and remote sessions never raise
   it. Any client holding the daemon token can answer it, as with push approvals.

## Protocol additions

Taken at the next minor when stage 7 merges, all additive with serde defaults:

- **`McpServerInfo.builtin: Option<BuiltinServerInfo>`**: `{ id, state, reason, notes, version,
  size_bytes }`. `state` is one of `not_set_up`, `setting_up`, `ready`, `failed`, `declined` and
  `unavailable` (no sandbox). The server's `config` is a read-only rendering: no command, and
  `catalog: "builtin:cas"`.
- **`mcp.builtin_setup { name } -> { started: bool }`**: starts setup (it counts as the user's
  approval). It returns at once; progress arrives as notifications.
- **`mcp.builtin_remove { name } -> { removed: bool }`**: deletes the environment, clears the
  recorded approval, and keeps the setting.
- **Notification `mcp.builtin_changed { name, builtin, line? }`**: sent on each state change and
  with setup output lines (masked).
- **`mcp.set_enabled`** accepts a built-in name and writes `mcp.builtin.<name>.enabled`. Its wire
  shape does not change.
- **`mcp.save` and `mcp.install`** refuse reserved names with their existing validation error.
  The wire shape does not change.

New settings (user layer; a project or environment layer may only turn it off), documented in
[10](../10-configuration.md) by stage 7:

- `mcp.builtin.cas.enabled` (default `true`);
- `mcp.builtin.cas.ask` (default `false`);
- `mcp.builtin.cas.limits.{cpu_s, memory_mib, wall_s}` (defaults 15, 1024 and 20; capped at
  120, 8192 and 300).

## Assumptions

- uv-managed CPython (python-build-standalone) runs from its base interpreter inside an
  AppContainer with read and execute granted on its folder, and inside the `bwrap` profile with
  `/usr` and the `/lib*` folders only.
  - Stages 4 and 5 verify the profiles with the Rust probe, and stage 7 with the real
    interpreter.
  - If CPython needs more, the profile grows by the named path, never `/home` or `/etc`
    wholesale.
- `-I` keeps the base interpreter's own `site-packages`, so `imperium_cas.pth` is read. Stage 2a
  checks this on all three OSes.
- The pinned `uv` verifies its Python downloads against the checksums it embeds (uv's documented
  behaviour for managed Python). Stage 6 records the `uv` version that does this.
- The MCP Python SDK's stdio server works with the `2025-06-18` revision that `imperium-mcp`
  speaks, and returns `structuredContent`. Stage 3 pins a version that does.
- The warm-up covers SymPy's lazy imports for all eleven tools, and the audit allow-list is small
  and stable. Stage 3's corpus run under the hook proves both.
- Landlock's `LANDLOCK_ACCESS_FS_EXECUTE` rule works inside `bwrap`'s namespaces on the CI
  kernels (ABI 1 or later). Stage 4 checks this on `ubuntu-22.04`; the `confine (ubuntu-24.04)`
  job checks it where `bwrap` works.

## Relevant paths

New:

- `sidecars/cas/` (`pyproject.toml`, `uv.lock`, `.python-version`, `src/imperium_cas/`,
  `tests/`);
- `runtimes/cas.env`;
- `crates/imperium-ipc/src/windows/confine.rs`;
- `crates/imperium-tools/src/confine/` (`mod.rs`, `linux.rs`, `macos.rs`, `windows.rs`);
- `crates/imperium-tools/tests/confine.rs` and the probe helper;
- `crates/imperium-local/src/cas_env.rs`;
- `crates/imperium-cli/src/cas.rs`;
- `crates/imperium-core/src/builtin_servers.rs` and `crates/imperium-core/tests/cas_mcp.rs`.

Changed:

- in `imperium-core`: `src/mcp.rs`, `connectors.rs`, `state/create.rs` and `main.rs` (the
  `confine` entry), and `rpc/` (the connectors methods);
- `crates/imperium-protocol/src/methods/connectors.rs` and `lib.rs` (the minor);
- `crates/imperium-config/src/{settings,registry,mcp_servers}.rs`;
- `crates/imperium-ipc/Cargo.toml` and `src/windows.rs` (the module declaration);
- `crates/imperium-tools/Cargo.toml`;
- `apps/desktop/src/ConnectorsWindow.tsx` and `apps/desktop/src-tauri/tauri.conf.json` (the
  `bubblewrap` dependency);
- `scripts/ci/detect.sh`, `scripts/ci/test-detect.sh`, `scripts/check-python.sh` and
  `.github/workflows/ci.yml`.

Not touched:

- the other session's files listed at the top;
- `crates/imperium-mcp/src/servers.rs`'s spawn path (a confined launch is a command and arguments
  with `CLEAN_ENV`). Stage 7 changes only `initialize`, to keep the reply's `serverInfo`.

## Related docs

- [05 Tools and isolation](../05-tools-and-isolation.md#mcp): MCP, Connectors and isolation
  tiers. 05 wins on how the tiers behave; this plan wins on the CAS profile.
- [04 Models and routing](../04-models-and-routing.md): the managed vLLM environment pattern, and
  where the setup deviation is stated.
- [09 Media generation](../09-media-generation.md#media-sidecar): the other planned sidecar, with
  its own `python` job.
- [10 Configuration](../10-configuration.md): the `mcp.builtin.*` settings.
- [11 CI/CD](../11-ci-cd.md): the `python-cas`, `rust` and `confine (ubuntu-24.04)` jobs.
- [Roadmap item 62](../12-roadmap.md#next-in-order) is the spec; this plan wins on details.

## Stages

Each stage is one pull request against `experimental` with the docs it changes, at most about 800
changed lines. Dependencies:

- stages 1 to 3 (Python; stage 2 is split into 2a and 2b) and stages 4 and 5 (Rust
  confinement) are independent and can run in parallel, except that stage 5's `BrokerSpawner`
  needs 2b;
- stage 6 needs 1, and 5 for the Windows ACL;
- stage 7 needs 3 to 6;
- stage 8 needs 7.

Commands are what CI runs; nothing needs a local Rust build.

### Stage 1: Sidecar skeleton, parser, guard and the CI jobs

- Objective:
  - Create `sidecars/cas/`: `pyproject.toml` with `[tool.uv] package = false`, ruff's `S`
    rules and `[tool.pytest.ini_options] pythonpath = ["src"]` (the package is not installed, so
    pytest needs `src` on its path), `uv.lock`, and `.python-version` (exact 3.12.x).
  - Write `parse.py` (tokenizer, Unicode table, Pratt parser, caps and the identifier check)
    and `guard.py` (D2's estimates).
  - Write `functions.py`: the frozen table's names and guard classes as data, with no SymPy
    import yet.
  - Add the `python-cas` job (`detect.sh` output `python_cas`, `test-detect.sh` case,
    `check-python.sh <sidecar>`, `ci.yml` three-OS matrix).
  - Write a short "planned" note in [05](../05-tools-and-isolation.md#mcp) and the job in
    [11](../11-ci-cd.md).
- Depends on: none.
- Owned paths: `sidecars/cas/**` (no builder or tools yet), `scripts/ci/detect.sh`,
  `scripts/ci/test-detect.sh`, `scripts/check-python.sh`, `.github/workflows/ci.yml` (the new
  job), [11](../11-ci-cd.md), [05](../05-tools-and-isolation.md).
- Acceptance checks:
  - `bash scripts/check-python.sh cas` in the `python-cas` job on the matrix;
  - `uv run pytest tests/test_parse.py tests/test_guard.py tests/test_hostile.py
    tests/test_no_dynamic_code.py tests/test_functions_table.py`:
    - precedence, implicit multiplication, relations, matrices and every refusal with its column;
    - the node cap is never reached before the token cap;
    - each goal 2 guard input refused with its estimate, without computing it;
    - `2^1000` and `factorial(1000)` allowed;
    - the corpus at parse level, each line under 100 ms;
    - Hypothesis at 2,000 examples;
    - the table equals D1's list.
  - `bash scripts/ci/test-detect.sh`;
  - the existing `python` job is unchanged and still skipped (no `sidecars/media`).

  About 750 lines.
- Status: done (pull request pending merge; see the outcome).
- Handoff notes: the media sidecar's job must stay Ubuntu-only. `check-python.sh` with no
  argument keeps its old behaviour.
- Outcome (2026-11-26):
  - **Delivered.** `sidecars/cas/` (Python 3.12.14, `package = false`, every dependency pinned
    exactly and locked with hashes: `sympy` 1.14.0, `mpmath` 1.3.0, `mcp` 2.3.0, and `pytest`,
    `hypothesis` and `ruff` as dev dependencies), `parse.py`, `guard.py`, `functions.py`, an
    empty `__init__.py`, the tests below, the `python-cas` job on the three-OS matrix,
    `scripts/check-python.sh <sidecar>`, the `python_cas` detect output with its
    `test-detect.sh` case, and the notes in 05 and 11. The `python-cas` job also runs
    `ruff format --check`.
  - **Checks to tests.** Precedence, implicit multiplication, relations, matrices, caps and every
    refusal with its column: `test_parse.py`. Goal 2's guard inputs with their estimates, the
    allowed cases, each guard class and matrix caps: `test_guard.py`. The hostile corpus
    (`hostile.txt`, 529 lines, plus 26 built-in huge inputs) with an audit hook recording every
    event, and four Hypothesis fuzz tests at 2,000 examples (seed printed in the pytest header,
    `CAS_FUZZ_SEED` repeats it): `test_hostile.py`. The R1 to R4 AST scan, its negative cases and
    the ruff `S` check: `test_no_dynamic_code.py`. The table equals this plan's D1 list, read
    from this file: `test_functions_table.py`.
  - **Audit events.** Parsing and guarding raise no audit event at all, so stage 1's allow-list is
    empty (`support.ALLOWED_EVENTS`). The guard avoids `id()` for this reason: Python 3.12
    audits `builtins.id`.
  - **Choices the plan left open.**
    - R4 as written forbids every import but `re`, `math`, `dataclasses` and `typing`, which would
      forbid `guard.py` using `parse.py`'s tree and `parse.py` using the function table. The test
      allows the sibling modules `functions` (parse, guard) and `parse` (guard), and nothing else.
    - R1 grants exceptions for `subprocess`, `os` and `ctypes` but does not list them, so the scan
      treats them as forbidden everywhere else. Two consequences for later stages: `worker.py`
      needs an exception for `os._exit` (R2 forbids `_` attributes), and `landlock.py` imports
      `os` and `sys` per stage 2b, while only `ctypes` is excepted. Adjust the test's
      `EXCEPTIONS` with the stage that needs it.
    - `re.compile` is `compile` to R1, so the tokenizer scans by hand and uses no regular
      expression. `is_identifier` is hand-written and tested against the published pattern.
    - The function table also records each function's argument count (`Min`, `Max`, `gcd` and
      `lcm` are variadic) and the `capped` caps. D1 lists no cap for `nextprime` and `prevprime`:
      they get `isprime`'s 200 digits.
    - A function name with no `(` after it is a symbol (D1: "any other identifier is a
      `Symbol`"); `sin x` is the two-names error. A relation is allowed only at the top level.
      `!` parses to a call of `factorial`, so the guard needs no separate rule.
    - `2e3` and `2E+3` are numbers; `2e` and `2E` are `2*e` and `2*E`.
    - D2 refinements (the first review found the first version unsound; this is the fixed one):
      - A symbol is an exact unit constant (`1`), so the constant parts of an expression with
        symbols are always sized: `(2x)^(10^6)` is refused like `2^(10^6)`, and `x^(10^6)`,
        `(x+y+z)^5000` and `exp(x^x^x)` pass. `(x+1)^(10^6)` is refused for the same reason.
      - Every estimate is an upper bound. It carries a magnitude `mag`, a digit budget for the
        numerator and denominator that runs through inexact values too (`(pi+1e70)-1e70` collapses
        to `pi`), the small exact value `val` (up to 64 digits, so `10^6` is known exactly), and an
        `unb` flag for values that may be arbitrarily large.
      - A division by an inexact value, `tan`, `cot`, `sec`, `csc`, `coth`, `atanh`, `log` of an
        inexact value, and `gamma` or `factorial` near a pole or with an unknown argument are
        unbounded. Unbounded values are fine in arithmetic and refused as an exponent or as an
        argument of a `digits` or `capped` function. A `floor`, `ceiling` or `Mod` of one is
        unbounded.
      - `select` no longer sums signed logs. `Max`, `Min` and `gcd` take the largest input,
        `lcm` the sum of the inputs' non-negative magnitudes, `floor` and `ceiling` the input plus
        one, `Mod(a, b)` the magnitude of `b`.
      - `root(x, n)` takes the exponent `1/n` from the index's value, or from its denominator's
        digits when the value is unknown, and refuses an inexact index.
      - `binomial(n, k)` with a negative or fractional `n` is bounded by `2^(|n|+k)` and refuses
        `k` over 100,000; a list or matrix argument to a function is refused, and a flat list
        holds at most 400 entries.
      - For a whole-number base the guard estimates `p/q*log10(base)+1` digits rather than
        `p/q*digits(base)`, so `isprime(10^199)` is allowed and `10^99999` is the largest power
        of ten. A constant `exp`, `sinh` or `cosh` of more than about 230,000 is refused
        (`exp(1000)` is allowed) because its result may be raised to a power.
      - A negative matrix power `M^(-n)` is sized as the inverse (`rows` times the digits), then
        the power.
    - Parser: every recursive call adds one to the depth, including the right side of a binary
      or implicit product, so the depth cap bounds Python's stack for every shape. `x²³` is
      refused rather than read as `x^(2^3)`.
    - The AST scan also forbids `__builtins__`, `builtins`, `breakpoint`, `attrgetter`,
      `methodcaller`, `runpy`, `code`, `codeop` and the attribute `modules`, and scans
      subpackages. The fuzz seed is the commit's in CI (`GITHUB_SHA`); `CAS_FUZZ_SEED` overrides
      it. The 100 ms per input check takes the best of three runs, with a 500 ms limit in CI.
  - **Not covered here.** The corpus runs at parse and guard level only. `test_no_string_to_sympy`
    and the audit allow-list for the worker are stage 2a. The `python-cas` job's limits tests are
    stage 2b.

### Stage 2a: Builder, worker and the audit hook

- Objective:
  - `build.py`: the table builder from the tree to SymPy, with `assume` and the second
    identifier check.
  - `worker.py`: warm-up, `RLIMIT_CORE = 0` (Unix), the allow-list audit hook with `os._exit`,
    one call over the capped length-prefixed pipe, then exit. It takes its limits from the
    command line, but sets none yet (stage 2b).
  - Measure the warm-up time and memory, and the first audit allow-list, per OS.
- Depends on: stage 1.
- Owned paths: `sidecars/cas/src/imperium_cas/{build,worker,framing}.py`, `sidecars/cas/tests/`
  (`test_build.py`, `test_worker.py`, `test_audit_hook.py`, `test_pid.py`,
  `test_no_string_to_sympy.py`).
- Acceptance checks:
  - `bash scripts/check-python.sh cas` in the `python-cas` job on the matrix;
  - `uv run pytest tests/test_build.py tests/test_no_string_to_sympy.py`: every table entry
    builds; hostile names in `assume`, `variable` and `at` are refused by the builder even when
    the schema is bypassed; no `str` reaches `sympify` or `parse_expr`;
  - `uv run pytest tests/test_worker.py`: one call in, one reply out, then exit; an oversized
    length prefix is refused before any allocation;
  - `uv run pytest tests/test_audit_hook.py`: inside a worker, `open`, `socket`, `subprocess`,
    `compile`, `exec` and the import of a new module each end the worker with `REFUSED`, even
    inside a `try`/`except`;
  - `uv run pytest tests/test_pid.py`: the base interpreter's spawned PID equals `os.getpid()`,
    path lines in a `.pth` are read under `-I`, and on Windows the venv redirector's PID differs.

  About 600 lines.
- Status: pending.
- Handoff notes: record the measured warm-up time, memory and allow-list in the outcome; stage
  2b's default limits and stage 5's warm-up allowance use them.
  The tree can be 2,000 levels deep (a long `1+1+1+...`), so `build.py` walks it with
  `parse.postorder`, never recursively. The AST scan's `EXCEPTIONS` and `R4_ALLOWED` in
  `tests/test_no_dynamic_code.py` need `os._exit` for `worker.py` and the SymPy names `build.py`
  imports (see stage 1's outcome).
  `test_no_string_to_sympy.py` must cover the string-taking methods too (`.subs`, `.solve` and the
  like called with a string), not only `sympify`, `parse_expr` and `lambdify`.

### Stage 2b: Front-side pool, Unix limits and Landlock

- Objective:
  - `limits_posix.py` and `limits_macos.py`: the worker sets `RLIMIT_CPU` and `RLIMIT_AS` after
    warm-up; the macOS footprint poll, with the overshoot measured.
  - `pool.py`: the front's side, behind a `Spawner` interface. `PosixSpawner` (subprocess with
    the base interpreter) and the wall clock, queue deadline, cancellation, and the `limit`,
    `crashed` and `refused` mapping. `BrokerSpawner` is a stub that stage 5's broker protocol
    fills.
  - `landlock.py` and the first `__main__.py`: the execute rule as the **first statement** of
    `imperium_cas.__main__`, before any other import and before stdin is read, failing closed
    when Landlock is present but the ruleset fails. The `confinement` value is kept for stage
    3's `serverInfo`.
- Depends on: stage 2a.
- Owned paths: `sidecars/cas/src/imperium_cas/{pool,landlock,limits_posix,limits_macos,
  __main__}.py`, `sidecars/cas/tests/` (`test_limits.py`, `test_landlock.py`,
  `workers/{spin,alloc,sleep}.py`).
- Acceptance checks:
  - `uv run pytest tests/test_limits.py`, on Linux and macOS:
    - the CPU spin, allocation and sleep test workers each end with `limit` naming their limit
      within the limit plus 1 s;
    - each goal 2 input gives `guard`, `parse` or `limit`;
    - a normal call succeeds right after;
    - a queued call's wait counts against its deadline;
    - a cancel kills the worker;
    - on macOS, the measured overshoot of the allocation worker is recorded and under the bound
      that 05 states.
  - `uv run pytest tests/test_landlock.py` on Linux:
    - the ruleset comes before stdin is read: an AST check that the first statement of
      `__main__.py` is the Landlock call and that `landlock.py` imports only `ctypes`, `os` and
      `sys`; and at run time, `landlock.py` records that the ruleset was applied, and the stdin
      reader asserts that record before its first read (a failed assertion exits the process);
    - after the rule, `execve("/bin/sh")` from the front and from a worker is refused with
      `EACCES`, while the interpreter can still start itself;
    - with an injected failure of `landlock_restrict_self` while the ABI is present, the process
      exits with an error before reading stdin;
    - it skips with a reason when Landlock is absent, and then `confinement` is `no-landlock`.

  About 650 lines.
- Status: pending.
- Handoff notes: on Windows these tests skip; the Windows limits are covered by stages 5 and 7.
  `imperium_cas/__init__.py` must stay empty, or import nothing: importing the package must not
  import `pool`, `worker` or the SDK before the Landlock statement. Stage 1 creates it empty and
  `test_no_dynamic_code.py::test_init_is_empty` enforces it; the AST check covers it.

### Stage 3: MCP front and the eleven tools

- Objective:
  - `server.py`: MCP stdio through the SDK's low-level server; `tools/list` with the schemas
    (identifier patterns included); `tools/call` handed to `pool`.
  - `tools.py` and `format.py`: the eleven tools with D6's outputs, domains and error
    categories.
  - `serverInfo.confinement` and the Landlock note in `instructions` in the `initialize` reply.
  - Finish the warm-up list and the audit allow-list from the corpus over every tool.
  - Measure the token size of the tool list.
- Depends on: stage 2b.
- Owned paths: `sidecars/cas/src/imperium_cas/{server,tools,format}.py`, `__main__.py` (the
  server call after the Landlock statement; the statement stays first), and their tests.
- Acceptance checks:
  - `uv run pytest tests/test_tools.py`: per-tool tests, including `equal`'s three outcomes with
    a fixed seed, `unevaluated` integrals, `ConditionSet` results, domains stated on every
    `solve` result, complex-domain inequalities refused, and contradictory assumptions;
  - `uv run pytest tests/test_roundtrip.py`: every result's text re-parses to an equal expression;
  - `uv run pytest tests/test_server.py`: the SDK's in-memory client, and one real stdio
    subprocess on Linux and macOS. The package is not installed, and `-I` ignores `PYTHONPATH`,
    so the path entry comes from a two-line bootstrap passed with `-c`: it puts `src` and the
    venv's `site-packages` on `sys.path`, then runs `imperium_cas` as `__main__` with `runpy`.
    The real launch through the `.pth` is covered by stage 7. The test checks
    `serverInfo.confinement` too;
  - the full hostile corpus through `tools/call` with zero events outside the allow-list;
  - the tool list under the measured token budget.

  About 800 lines. If it is over, split it into 3a (front, `simplify`, `expand`, `factor`,
  `solve`, `evaluate`, `equal`) and 3b (the calculus tools and `matrix`).
- Status: pending.
- Handoff notes: on Windows the stdio test waits for stage 7, because the server needs the
  broker there.

### Stage 4: `confine` on Linux and macOS

- Objective: `imperium-tools::confine` provides:
  - `Profile` (environment root, temporary folder, minimal environment);
  - `available() -> Result<(), Unavailable>`, with `bwrap` resolved by absolute path;
  - `launch(profile, program, args) -> (command, args, env)`, which builds the `bwrap` allowlist
    arguments (root bound at its own path) and the Seatbelt profile (ancestor metadata, same-
    sandbox signal and process-info).

  Also add the probe helper (with child-spawn and environment-canary checks), the
  `confine (ubuntu-24.04)` job, and `bubblewrap` in the deb and rpm dependencies. Document the
  profile in [05](../05-tools-and-isolation.md#isolation-tiers).
- Depends on: none.
- Owned paths: `crates/imperium-tools/src/confine/{mod,linux,macos}.rs`,
  `crates/imperium-tools/tests/confine.rs`, the probe helper, `crates/imperium-tools/Cargo.toml`
  (the `landlock` dev-dependency), `ci.yml` (`IMPERIUM_REQUIRE_CONFINE` and the new job),
  `apps/desktop/src-tauri/tauri.conf.json` (the dependency only).
- Acceptance checks:
  - `cargo test -p imperium-tools --test confine` on `ubuntu-22.04` and macOS: every goal 3
    refusal, for the probe and its child; the allowed root is readable; on Linux, with
    Landlock, `execve` of `/usr/bin/env` is refused;
  - `cargo test -p imperium-tools confine::`: the argument and profile builders (no `/home`, no
    `/etc`, `--unshare-all`, `--clearenv`, an absolute `bwrap`, and the root at the same path),
    and `available()` returning `Unavailable` with a reason through an injected failing probe;
  - the `confine (ubuntu-24.04)` job: works or reports `Unavailable`, never unconfined.

  About 650 lines.
- Status: pending.
- Handoff notes: the production Landlock rule is Python (stage 2b). The probe here restricts
  itself with a rule of the same shape through the `landlock` crate (a Linux-only dev-dependency
  of `imperium-tools`; `cargo tree -e normal` shows it in no shipped crate), so stage 4 does not
  wait for stage 2b.

### Stage 5: Windows confinement and the broker (`imperium-ipc`)

- Objective:
  - Write `crates/imperium-ipc/src/windows/confine.rs` (`#![allow(unsafe_code)]` through
    `windows.rs`, a soundness note per block): the AppContainer profile and SID, the ACL grant,
    the server job, `CreateProcessW` with security capabilities, job list, child-process policy,
    handle list and environment block, and the broker (worker spawn into a nested job at
    creation, `DuplicateHandle` of its pipes, `arm` with `GetProcessTimes`).
  - Write `crates/imperium-tools/src/confine/windows.rs` over the safe wrappers.
  - Add the `imperium-core confine --profile cas -- ...` entry in `main.rs`, before the daemon's
    own startup.
  - Fill in `BrokerSpawner` in `pool.py`.
  - Decide and record the nested-job behaviour and the active-process limit from measurement.
- Depends on: stage 4 (the shared `Profile` API); stage 2b for `BrokerSpawner`.
- Owned paths: `crates/imperium-ipc/src/windows/confine.rs`, `crates/imperium-ipc/src/windows.rs`
  (the declaration), `crates/imperium-ipc/Cargo.toml` (features),
  `crates/imperium-tools/src/confine/windows.rs`, `crates/imperium-tools/Cargo.toml` (the
  `imperium-ipc` dependency), `crates/imperium-core/src/main.rs` (the entry only), the Windows
  part of `tests/confine.rs`, `sidecars/cas/src/imperium_cas/pool.py` (`BrokerSpawner`).
- Acceptance checks:
  - `cargo test -p imperium-tools --test confine` on Windows: goal 3's refusals for the probe and
    a broker-spawned child (home canary, `<data>`, a loopback TCP connect, the named pipe, the
    environment canary); a direct `CreateProcess` from the probe fails; killing the launcher
    kills both.
  - `cargo test -p imperium-ipc confine`:
    - a broker-spawned probe reports the PID the broker returned;
    - its CPU spin is stopped by the job at the armed limit plus 1 s;
    - its allocation fails at the memory limit, measured from the post-warm-up baseline;
    - the active-process limit holds.
  - `cargo test -p imperium-ipc broker` (R2-1):
    - a `spawn` request carrying a path, an argument or any extra field is refused, and the
      broker pipe is closed;
    - a message over 64 bytes, or an unknown opcode, is refused the same way;
    - `arm` or `kill` with a worker id the broker did not issue, an id of a worker that has
      exited, or a PID is refused;
    - `arm` cannot raise a limit above the default;
    - a worker that is never armed is killed at the default CPU, memory and wall-clock limits;
    - an armed worker whose front never kills it is killed at the broker's own wall-clock cap;
    - the worker's handle list does not include the broker pipe: the probe, as a worker, cannot
      write to it.
  - A test that the launcher refuses an unknown profile and any argument before `--` other than
    `--profile`.
  - `bash scripts/check-rust.sh` (CI). The presence end-to-end job runs too, because
    `imperium-ipc` changed.

  About 800 lines. If it is over, split it into 5a (the AppContainer launch) and 5b (the broker
  and jobs).
- Status: pending.
- Handoff notes: `windows-sys` features are added to `imperium-ipc` only. No `unsafe` appears
  outside `imperium-ipc`.
  The broker sends `arm` when the call is handed to the worker, not when the worker reports ready.
  An unarmed spare therefore has no wall clock, only CPU, memory and active-process limits, so
  the "never armed" test becomes "an unarmed worker is stopped at the default CPU limit". (D3's
  `arm` and wall-clock wording, which count from "ready", needs this change when stage 5 edits it.)

### Stage 6: Managed environment and the CLI

- Objective:
  - Write `imperium-local::cas_env`:
    - the embedded files and both digests;
    - the `runtimes/cas.env` pins and the verified `uv` download;
    - setup through the `Runner` with the allow-listed environment;
    - the `imperium_cas.pth` in the base interpreter's `site-packages`;
    - `setup.lock`, the record written last, `status`, and `remove` clearing the approval;
    - the source-only rewrite;
    - the ACL grant through `imperium-ipc` on Windows.
  - Add the CLI `imperium runtime cas setup|status|remove`.
  - Document it in [04](../04-models-and-routing.md) next to vLLM, with the deviation.
- Depends on: stage 1 (the files to embed) and stage 5 (the ACL wrapper; until it merges, the
  step is stubbed behind `cfg(windows)`).
- Owned paths: `crates/imperium-local/src/cas_env.rs`, `crates/imperium-local/Cargo.toml` (the
  `imperium-ipc` dependency on Windows), `runtimes/cas.env`, `crates/imperium-cli/src/cas.rs`,
  the CLI's `runtime` command table, [04](../04-models-and-routing.md).
- Acceptance checks: `cargo test -p imperium-local cas_env` and `cargo test -p imperium-cli
  cas` (CI), covering:
  - the exact `uv` arguments, including `--no-build`;
  - `UV_PYTHON_INSTALL_REGISTRY=0` and `UV_PYTHON_INSTALL_BIN=0` are present in the child
    environment on every OS (R2-2);
  - the exact child environment: a set `UV_PYTHON_DOWNLOADS_JSON_URL`, `UV_INDEX_URL` or
    `PIP_INDEX_URL` is absent, and a set `HTTPS_PROXY` or `SSL_CERT_FILE` is present (goal 6);
  - the `.pth` content and location;
  - a `uv` digest mismatch refused with nothing kept;
  - a failure, or an interruption, leaves no record and no `<env-digest>/`;
  - setting up again changes nothing;
  - a source-digest change rewrites `project/` only, with no download;
  - an environment-digest change rebuilds;
  - a second setup while `setup.lock` is held reports "already running";
  - `remove` clears the approval;
  - no command or download before the user's yes (goal 5, the CLI part);
  - the embedded-files list matches the tracked files.

  About 700 lines.
- Status: pending.
- Handoff notes: the real setup runs first in stage 7's `cas_mcp`. Record the measured download
  and disk sizes there.
  The `.pth` is written as `import site; site.addsitedir("<venv site-packages>")` plus the
  project's `src` path line, so that `pywin32.pth` is processed; or this stage proves that
  `pywin32` is never imported by the server (D5 says the `.pth` holds the two paths).

### Stage 7: Daemon wiring, protocol and the end-to-end test

- Objective:
  - Write `builtin_servers.rs`: the built-in registry, state, the approval for interactive local
    sessions only, the Connectors hint for others, background setup and rebuild, and notices.
  - Add the `imperium_cas` spec in `session_servers` (the confined launch, `CLEAN_ENV`, `allow`,
    timeout), and the reserved-name refusal for user config, environment files and `.mcp.json`
    in `session_servers`, `state/create.rs` and `mcp_servers.rs` (`mcp.save` and `mcp.install`).
  - Add the settings and the protocol additions (next minor), with the regenerated TypeScript
    bindings.
  - Add `tests/cas_mcp.rs` with its limit cases, CI cache and `IMPERIUM_REQUIRE_CAS`.
  - Update [05 Connectors](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented),
    [10](../10-configuration.md) (`mcp.builtin.*` and reserved names) and
    [02](../02-architecture.md#protocol) (the protocol list).
- Depends on: stages 3, 4, 5 and 6.
- Owned paths: `crates/imperium-core/src/{builtin_servers,mcp,connectors}.rs`,
  `crates/imperium-core/src/state/create.rs`, `rpc/` (connectors),
  `crates/imperium-protocol/src/methods/connectors.rs`, `lib.rs` (the minor),
  `packages/protocol/src/generated/`, `crates/imperium-config/src/{settings,registry,
  mcp_servers}.rs`, `crates/imperium-mcp/src/servers.rs` (`initialize` keeps `serverInfo`),
  `crates/imperium-core/tests/cas_mcp.rs`, `ci.yml` (cache and environment),
  docs 02, 05 and 10.
- Acceptance checks:
  - `cargo test -p imperium-core --test cas_mcp` on the matrix:
    - goal 4 with the real setup and real confinement;
    - the real setup leaves the machine unchanged outside `<data>` (R2-2): on Windows the
      `HKCU\Software\Python` key and its subkeys are the same before and after; on every OS uv's
      bin folder (`UV_PYTHON_BIN_DIR`, else `~/.local/bin`) has the same entries;
    - `serverInfo.confinement` reaches the built-in's status, and with `no-landlock` the
      `mcp.list` note;
    - a CPU trip, a memory trip and a wall-clock trip through the real confined server, each
      followed by a successful call;
    - the worker PID rule on Windows through the broker;
    - the Windows stdio front started by the launcher.
  - `cargo test -p imperium-core --test builtin_servers`, daemon tests with a fake runner:
    - the approval is raised once for an interactive local session, and never for scheduled,
      headless or remote sessions;
    - no download before approval;
    - a decline is recorded;
    - approve, then `ready`, then served in a new session;
    - *Remove* makes the next interactive session ask again;
    - a disabled built-in is not started;
    - an unavailable sandbox is reported and not started;
    - `imperium_cas` and `imperium_x` are refused with a notice from user config, an environment
      file and `.mcp.json`, while the built-in still starts. The notice names the source and the
      fix, for example "MCP server imperium_x in .mcp.json is not started: names starting with
      imperium_ are reserved for Imperium's built-in servers. Rename it, for example to x.";
    - a permission rule on `mcp__imperium_cas__*` applies;
    - the spec carries `CLEAN_ENV`.
  - `cargo test -p imperium-config mcp_servers`: `mcp.save` refuses reserved names.
  - `mcp.list` shows each state (goals 5 and 7).
  - `bash scripts/check-rust.sh` regenerates the bindings with no diff.

  About 800 lines. If it is over, split it into 7a (registry, settings, reservation and
  protocol) and 7b (session wiring and the end-to-end test).
- Status: pending.
- Handoff notes: take the protocol minor at merge time. `state/create.rs` changes only where
  `session_servers` is called and its notices are merged; the evaluation branch stays as it is.
  The Windows launch needs the same `.pth` rule as stage 6: `site.addsitedir` of the venv's
  `site-packages`, so `pywin32.pth` is processed, or a proof that `pywin32` is never imported.

### Stage 8: Connectors UI, docs and close-out

- Objective:
  - Add a **Built in** group in `ConnectorsWindow.tsx`: state, *Set up* with the size, progress
    lines, the switch, *Remove*, the reason when unavailable, and the Landlock note.
  - Add a desktop test.
  - Finish the docs: 05 (the CAS server, its tools, language, limits and profile), 11, and
    roadmap item 62 marked implemented with this plan's status.
- Depends on: stage 7.
- Owned paths: `apps/desktop/src/ConnectorsWindow.tsx` and its test, the docs above.
- Acceptance checks:
  - `pnpm test` in the `web` job: the group renders each state, *Set up* calls
    `mcp.builtin_setup`, the switch calls `mcp.set_enabled` and *Remove* calls
    `mcp.builtin_remove` (goal 7);
  - the `docs` job passes (`python3 scripts/ci/check-links.py .` and markdownlint).

  About 450 lines.
- Status: pending.
- Handoff notes: mark this plan done and fill each stage's outcome.

## Risks across stages

1. **Sandbox availability on users' machines.** Ubuntu 24.04 restricts unprivileged user
   namespaces through AppArmor, and `bwrap` may be missing or blocked; corporate Windows policy
   may block AppContainer creation. Because the profile fails closed, the CAS is then
   `unavailable` with the reason. The docs say how to enable `bwrap`, and the
   `confine (ubuntu-24.04)` job shows the stock behaviour.
2. **CPython inside the allowlist profiles** may need paths found only by running. Stages 4 and 5
   probe them with Rust, and stage 7 runs the real interpreter on the matrix.
3. **The audit allow-list** may need more events than expected for some SymPy paths, which cause
   false refusals. The corpus over every tool in stage 3 measures it, and the list is fixed in
   the code with a test.
4. **CI time and flakiness.** The real setup downloads about 70 MB on three OSes; the cache and
   retries reduce this. The `python-cas` job adds about 3 minutes per OS (estimate), and the
   `confine (ubuntu-24.04)` job one Rust build of `imperium-tools`.
5. **Per-session memory.** Each session with the CAS has a front, and after the first call a
   spare worker (about 80 MB; estimate). Stage 2a measures it. If it matters, share a server
   (non-goal for now) or start the spare only on demand.
6. **Tool-list size for small models.** Eleven schemas add context to every request. Stage 3
   keeps the descriptions short and measures them. Item 61's `tool_search` can hide them for local
   models once it lands.
7. **Windows broker complexity.** The broker is the largest new `unsafe` surface. It is kept in
   one module with soundness notes, tested with the probe, and split (5a and 5b) if it grows.

## Open questions

None blocking. The four questions of revision 1 were decided by the root:

- the environment and source digests are split (D5);
- there is no unconfined opt-out (D4);
- unattended, headless and remote sessions never raise the approval (D5);
- there is no JSON input, and `solve` uses one solver family and states its domain (D1, D6).
