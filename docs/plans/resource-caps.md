# Plan: resource caps on every child process

Revision: 2026-11-26, revision 4 (second review round: R1 to R7 folded in; revision 3 folded in
the first review's blockers B1 to B5, majors M1 to M7 and the minor and missing-resource
findings; revision 2 recorded the user's answers as
[settled decisions](#settled-decisions); revision 1 was the first draft). Written
against `experimental` at `cd791003`. Roadmap [item 68](../12-roadmap.md#next-in-order).
Status: stage 1 done (pull request #337); stages 2 to 8 planned.

A CAS test process grew to about 20 GB on the user's 64 GB Windows machine and took the machine
to about 100% RAM. Nothing stopped it: on Windows no process Imperium starts has a memory limit
today, and on Linux and macOS only two workers do. This plan treats unbounded resource use as a
security issue. An agent's command, an MCP server, a hook, a plugin or a runaway test should not
be able to take the host down, whatever text steered it there.

Every process Imperium starts gets a **cap unit** (the user's own terminals get one with no
caps, [S2](#settled-decisions)). A unit is one process tree with a memory cap,
a process-count cap and an optional CPU cap, enforced by the OS where the OS can, and by a
daemon-side poll where it cannot. Units are grouped per session under a session aggregate, and
all of them under an Imperium-wide total. A daemon watchdog watches the machine's free memory and
stops Imperium's own processes before the machine runs out. When something is stopped, the agent
and the interface are told which cap, what value, and which setting raises it. Only the user can
raise a cap.

Each stage below is one pull request against `experimental`. Every merge into `experimental`
publishes a build, so caps that could stop legitimate work start in warn-only mode where the
plan says so (see [rollout](#rollout)).

## Settled decisions

Recorded from the user's answers (2026-11-26). They override anything else in this plan.

- **S1. Commands: 25% of RAM, between 2 and 16 GiB** (16 GiB on the user's 64 GB machine), as
  [L6](#l6-default-caps-per-class) proposed.
- **S2. The user's own terminals are not capped.** Only processes an agent starts, or an
  agent-driven path starts (MCP servers, hooks, plugins, the CAS, code mode, the browser), are
  capped. A terminal pane still runs in a unit, so it keeps its kill-on-close and tree stop, but
  the unit has no memory, process or CPU cap. It is outside the session aggregate, and the
  watchdog never stops it. This holds only for terminals created and typed into through the
  desktop gate; any other terminal is a command unit ([L13](#l13-uncapped-terminals-only-behind-the-desktop-gate)).
- **S3. `limits.mode = "off"` is allowed, and only the user can set it.** Agents can never
  change `limits.mode`: an agent's `imperium config set|unset limits.mode` is refused outright,
  not asked. Every other `limits.*` key from an agent asks every time (the safety floor). The
  project and environment layers may only move it towards `enforce`
  ([L7](#l7-settings-user-layer-only-narrowing-elsewhere)). As the
  [threat model](#threat-model) says, "only the user" holds against agents' tool calls, the
  confined tiers, project files, plugins and protocol callers. It is a guard, not a boundary,
  at tier 0: a tier-0 command has the user's rights and can edit `config.toml` from a shell. Such
  a change is detected and announced (a toast and a journal entry naming the source, L7), not
  prevented.
- **S4. The watchdog acts on low memory whatever the cause.** It stops Imperium's
  lowest-value units first, then unloads idle models, and never touches a program Imperium did
  not start ([L11](#l11-the-daemon-watchdog)).

Kept as the plan's defaults, which the user can change in settings:

- **S5. No default CPU cap.** Agent classes run at lower priority, and
  `limits.<class>.cpu_percent` sets a cap.
- **S6. Containers count against the session aggregate when they start**, at their cap
  ([L5](#l5-containers-docker-and-wsl-get-their-own-flags)).
- **S7. The repository's scripts.** The pytest run gets a 4 GiB unit
  (`IMPERIUM_TEST_MEMORY_MB`). `scripts/check-rust.sh` gets a unit too if wrapping it costs only
  the one launcher `exec`: 50% of RAM, at most 32 GiB (`IMPERIUM_RUST_MEMORY_MB`), because rustc
  and the linker can legitimately use more ([L12](#l12-the-repositorys-own-tests)).

## Goals

Each goal is boolean, with a check that runs in CI on the three OSes and needs no manual step.

1. **Every spawn site is covered.** Every place in the workspace that starts a process goes
   through `imperium-limits` (one class per site), or appears on a short allow-list of
   daemon-internal one-shot probes with a reason. Check: a source test (in the style of the
   existing source checks) fails on any `Command::new`, `Command::from`, a `Command` imported
   under another name (`use ... Command as X`), `std::process::Command`,
   `tokio::process::Command`, a `portable_pty` spawn or `CreateProcess*` outside
   `imperium-limits` that is not on the list. It covers `crates/` and
   `apps/desktop/src-tauri/`.
2. **An allocation bomb is stopped at its cap.** A probe that allocates and touches memory in
   64 MiB steps, under a 256 MiB unit, is killed, and the reported peak is at most the cap. On
   macOS the peak may exceed the cap by the measured poll overshoot, which the test bounds.
3. **A fork bomb is stopped at its cap.** A probe that starts copies of itself in a loop is
   stopped: under a 32-process unit on Windows and macOS, and under a 256-task unit on Linux
   (`pids.max` counts tasks, which are threads as well as processes;
   [L6](#l6-default-caps-per-class)). No probe process is left afterwards, and the notice names
   the process or task cap.
4. **A CPU spin is held to its cap.** On Linux and Windows a four-thread spin under a 25% cap
   uses at most 35% of the machine over 3 s, measured as the unit's own CPU time (the job's
   accounting, `cpu.stat` `usage_usec`), not the machine's load. On macOS the cap is reported as "not enforced" (see
   [L4](#l4-macos-a-daemon-side-poll-no-jetsam)) and the test checks that report.
5. **The session aggregate holds.** Two units, each under its own cap, whose sum passes the
   session cap: the larger one is stopped with a `session memory` notice.
6. **The watchdog acts before the machine runs out.** With an injected free-memory reader that
   reports less than the reserve, the watchdog stops the lowest-value unit within two samples,
   and the fastest-growing one within that group. It never touches a model with a call in
   flight while non-model units remain, and never touches the daemon itself. When the reader
   shows no recovery after a stop, the watchdog treats the cause as external and stops acting
   until free memory is critical ([L11](#l11-the-daemon-watchdog)).
7. **Caps cannot be raised from outside the user layer.**
   - These fail to raise a cap: a project or environment `.imperium/config.toml` (refused as
     widening), a plugin manifest or an `.mcp.json` (the field is ignored with a warning), an
     `IMPERIUM_LIMITS__*` variable (narrow-only), an `.mcp.json` `env` block that sets
     `IMPERIUM_CGROUP_ROOT` (ignored by a daemon started with `--exit-when-idle`), and an
   agent's `imperium config
     set` (the safety floor; refused outright for `limits.mode`).
   - In the OS tier, writing the unit's cgroup files and reaching the daemon's endpoint are
     blocked.
   - The tier-0 bypasses are tested as the documented residuals they are: a tier-0 shell that
     edits `config.toml`, or writes its own cgroup's `memory.max`, succeeds. The first is
     detected at once (a toast and a journal entry naming the source), and the second shows in
     `imperium limits status` as a unit whose cap no longer matches.
   - A user-layer value raises a cap, with the same toast and journal entry.
8. **Notices are clear.** For each class, a stopped unit produces a tool result (or a
   server/runtime status) and a `resource.limited` event that name the class, the limit, its
   value, the peak and the setting key.
9. **Tests cannot take the runner down.** Every probe also enforces a hard ceiling of its own
   (four times the cap it is testing, and never more than 1 GiB), so a cap that fails to hold
   fails the test rather than exhausting the CI machine.

## Current state (found on `experimental` at `cd791003`)

What limits exist, per spawn site. "Daemon job" is the Job Object the daemon puts itself in at
start (`end_children_with_daemon` in `crates/imperium-core/src/main.rs`): kill-on-close and
**`BREAKAWAY_OK`**, no memory or process limit.

| Spawn site | Where | Windows | Linux | macOS |
| --- | --- | --- | --- | --- |
| `run_command`, foreground | `imperium-tools/src/exec.rs` | daemon job only; `kill_on_drop` kills the direct child | `kill_on_drop`, no process group; OS tier: bubblewrap's PID namespace | `kill_on_drop` |
| `run_command`, background | `imperium-tools/src/processes.rs` | own job (kill-on-close, assigned **after** spawn, then a tree walk on stop) | own process group | own process group |
| Container tier | `imperium-tools/src/sandbox.rs` (`container_args`) | `docker run` with no `--memory`, `--pids-limit` or `--cpus` | same | same |
| Scheduled and auto-fix commands | `imperium-core/src/scheduler.rs` | daemon job only | `kill_on_drop` | `kill_on_drop` |
| Environment runtimes (`ProcessRunner`) | `imperium-tools/src/runtime.rs` | daemon job only | `kill_on_drop` | `kill_on_drop` |
| Stdio MCP servers (plugin ones too) | `imperium-mcp/src/servers.rs` | daemon job only | `kill_on_drop`, no group: a server's children outlive it | same |
| Hooks | `imperium-extensions/src/hooks.rs` | daemon job only; timeout (at most 300 s) | `kill_on_drop` and the timeout | same |
| CAS server and workers | `sidecars/cas`, [D3](cas-mcp.md#d3-per-call-resource-limits-single-use-pre-warmed-workers), [D4](cas-mcp.md#d4-sandbox-a-fixed-allowlist-profile-for-the-cas) | **none until CAS stage 5**: `PosixSpawner` refuses Windows unless a test opts in to unlimited workers, which `tests/support.py` does | per-worker `RLIMIT_AS` and `RLIMIT_CPU`, the front's wall clock | `RLIMIT_CPU`, a 50 ms footprint poll in the front |
| Code-mode worker | `imperium-script`, [C1](code-mode.md#c1-the-runtime-monty-a-python-subset-interpreter-written-in-rust), [C5](code-mode.md#c5-budgets-and-cancellation) | worker refuses to run until stage 1b (pull request #328 adds a job: memory at twice `script_memory_mb`, one process) | `RLIMIT_DATA` at twice `script_memory_mb`; #328 adds a cgroup when delegated | Monty's own limit; #328 adds a 50 ms RSS watchdog |
| Terminal panes | `imperium-pty` | own job, kill-on-close, assigned after spawn plus a tree walk | per-terminal supervisor (freeze-and-kill walk) | session sweep only |
| Local model servers (llama.cpp, vLLM native, Docker, WSL) | `imperium-local/src/process.rs` | daemon job only (for WSL, the job holds `wsl.exe`, not vLLM) | `kill_on_drop`, ledger | same |
| Speech servers (whisper) | `imperium-local/src/speech.rs` | daemon job only | `kill_on_drop` | same |
| Browser (Chromium over CDP) | `imperium-web/src/browser.rs` | daemon job only | `kill_on_drop` | same |
| Daemon tools: git, `gh`, extension installs, runtime checks, GPU probes, checkpoints, review, PR monitor, updater | `imperium-core`, `imperium-github`, `imperium-local` | daemon job only | none | none |

Findings this plan fixes:

- **F1. Nothing on Windows has a memory or process limit.** Every unit above is either in the
  daemon job alone (no limits) or in a kill-on-close job with no limits.
- **F2. Most Windows children can leave the daemon job.** It has `BREAKAWAY_OK`, so a foreground
  command, an MCP server or a hook (which have no job of their own) can start a child with
  `CREATE_BREAKAWAY_FROM_JOB`, and that child then outlives the daemon. Whether the background
  commands' and terminals' own jobs (which lack `BREAKAWAY_OK`) block breakaway through the nested
  hierarchy is not documented clearly enough to rely on. Stage 1 tests it.
- **F3. Unix MCP servers, hooks and foreground commands are not in their own process group.**
  `kill_on_drop` kills the direct child only, so their grandchildren survive a stop.
- **F4. The container tier passes no resource flags.** A container can use all of the host's
  memory and PIDs.
- **F5. The likely incident path (inference, not confirmed).** On Windows, the CAS tests run
  SymPy in-process (the corpus and fuzz tests) and in workers with no limits
  (`unlimited_on_windows=True`), and the tests were run outside Imperium. A runaway
  simplification there has no bound. Imperium's caps would not have covered a test run that a
  development agent started outside Imperium, so [stage 2](#stage-2-the-daemons-cgroup-the-settings-and-a-capped-launcher-for-scripts)
  also gives the repository's own check scripts a capped launcher.

Reusable pieces already written or in review:

- `imperium-script/src/cgroup.rs` (#328): detects a delegated cgroup v2 (owned by the daemon's
  user, memory controller present, not root), moves the daemon into a leaf
  (`imperium-daemon`), enables `+memory` for the children, and places a worker in its own child
  with `memory.max` and `memory.swap.max = 0`. It also sweeps stale children.
- `imperium-script/src/watchdog.rs` (#328): a macOS RSS poll (50 ms, `proc_pidinfo`) that kills
  one process over a ceiling.
- `imperium-script/src/windows/launch.rs` (#328): a suspended `CreateProcessAsUserW`, a job with
  a process-memory limit, one active process, kill-on-close and die-on-unhandled-exception,
  assignment while suspended, then resume.
- The CAS plan's D4 server job (memory limit × 2 + 512 MiB, the measured active-process limit)
  and D3's broker, both in `imperium-ipc` (CAS stage 5, pending).
- The terminal supervisor's freeze-and-kill walk (`imperium-pty/src/supervisor.rs`), which
  overtakes a fork bomb without cgroups.
- The residency controller's memory budget and `Footprint::required_bytes`
  (`imperium-local/src/planner.rs`), and its live RAM and VRAM readings
  ([04](../04-models-and-routing.md#residency-controller-implemented)).

## Threat model

Assets: the host's memory, process table and CPU, and so the user's other work and the daemon
itself. Attackers: text injected into an agent's context (which steers commands it runs), a
malicious or buggy MCP server, plugin, hook or mod, a repository's own scripts (tests, builds,
`postinstall`), and plain runaway code with no attacker at all (the incident).

What the caps are:

- **A boundary for confined code.** Code in the OS tier, the container tier, the CAS profile and
  the code-mode worker cannot see or reach the cap's controls (see
  [L9](#l9-tamper-resistance)), so the caps hold against it.
- **A guard, not a boundary, at tier 0.** A tier-0 command runs with the user's rights
  ([05](../05-tools-and-isolation.md#isolation-tiers)). A deliberately hostile tier-0 process can
  raise its own cgroup's `memory.max` (the file belongs to the user), move itself to another
  cgroup in the delegated tree, open the daemon with `PROCESS_DUP_HANDLE` and change its job, or
  edit `config.toml` from a shell. The caps still stop every accident and every process that is
  not written to evade them, which covers the incident. A raised cap or a changed `limits.mode`
  is detected and announced, whatever its source (L7). The plan does not claim more, and the
  same holds for "only the user can raise a cap" (S3).

Not in scope: capping GPU memory (the residency controller owns VRAM; the watchdog only reads
it, L15), capping disk space and file descriptors (the watchdog only warns on low disk, L15),
network bandwidth, and processes the user starts outside Imperium.

## Decisions

### L1. One crate owns every OS mechanism: `imperium-limits`

**Accepted: a new leaf crate, `crates/imperium-limits`, holds the cap types and the OS code for
the three platforms. Every spawn site builds a `UnitSpec` and spawns through it.**

```rust
pub enum Class { Command, Terminal /* no caps, S2 */, Mcp, Hook, Cas, Script, Browser, Internal, Model }

pub struct Caps {
    memory_bytes: Option<u64>,
    processes: Option<u32>, // Windows (active processes) and macOS (the poll's count)
    tasks: Option<u32>,     // Linux: `pids.max` counts threads too
    cpu_percent: Option<u16>,
}

pub struct UnitSpec { class: Class, caps: Caps, session: Option<SessionId>, label: String }

impl Limits {
    /// Spawns `command` already inside its unit: no instruction runs outside it.
    pub fn spawn(&self, spec: UnitSpec, command: tokio::process::Command) -> Result<Unit, LimitError>;
    /// Adopts a process another mechanism started (ConPTY), as soon as possible after spawn.
    pub fn adopt(&self, spec: UnitSpec, child: &impl RawChild) -> Result<Unit, LimitError>;
}

impl Unit {
    pub fn usage(&self) -> Usage;          // memory now and peak, processes, CPU
    pub fn stop(&self) -> StopReport;      // kills the whole tree
    pub async fn ended(&self) -> Ending;   // exit, or `Limited { limit, value, peak }`
}
```

- `imperium-limits` depends only on `tokio`, `rustix` (Unix), `libc` (macOS, as #328 uses) and
  `windows-sys`. The `unsafe` code sits in per-OS modules with `#![allow(unsafe_code)]` and a
  soundness note on each block, the pattern `imperium-ipc/src/windows.rs` uses.
- `imperium-limits` lands first (stage 1), and #328 rebases onto it: #328's `cgroup.rs`,
  `watchdog.rs` and the job part of `windows/launch.rs` become calls into it, with the M5 setup
  fix of [L3](#l3-linux-a-delegated-cgroup-v2-tree-rlimit-and-a-poll-where-there-is-none) folded
  in. The code-mode worker keeps its token, labels, pipes and handle list in `imperium-script`,
  and asks `imperium-limits` for its job (one job, not two: see
  [L8](#l8-reuse-not-double-limits)).
- CAS stage 5's broker and server job (in `imperium-ipc`) create their jobs through
  `imperium-limits`' job wrapper, so the limits code exists once.

Rejected:

- **Extending `imperium-ipc`.** It is the daemon's socket crate. Caps are needed by crates that
  should not depend on the socket code (`imperium-mcp`, `imperium-extensions`, `imperium-web`),
  and the CAS plan's AppContainer code can depend on a limits crate as easily as the reverse.
- **Each crate doing its own** (as today in `imperium-script`, `imperium-pty` and
  `imperium-tools`). That gives three Windows job wrappers already, and none of them sets a
  limit.
- **`win32job` for the limits.** It wraps the extended limits but not CPU-rate control,
  notification limits or completion ports, and a second wrapper would remain for those.
  `imperium-pty` and `imperium-tools` drop it once they move over.

### L2. Windows: nested Job Objects, assigned while suspended

**Accepted: every unit is a Job Object nested in its session's job, which is nested in a
children-only total job. The process is created suspended, assigned, then resumed.**

- **Hierarchy.** `daemon job (kill-on-close, and BREAKAWAY_OK so the folder opener can leave) →
  total job → session job → unit job`. The daemon itself is only in the daemon job, so no limit
  can make the daemon's own allocations fail. Daemon tools have no session and go under
  the total job in an `internal` job. Model runtimes are outside the total (L6) and sit in a
  `models` job directly under the daemon job. The uncapped terminal job (S2) sits directly
  under the daemon job too, outside the total and every session job, with kill-on-close and no
  limits. Nested jobs need Windows 8 or later; Windows 10 and 11 have them. Every job is
  anonymous (no name), so no other process can open one by name.
- **Nesting order.** A job is nested by the first process assigned to it, under the job that
  process is in at that moment. So every spawn assigns in one order: total, then session, then
  unit (or models then unit; or the terminal job alone). The first process ever assigned to the
  total job makes it a child of the daemon job, the first of a session nests the session job
  under the total, and so on. A job already in the chain is never assigned out of order.
- **Limits per unit job.** `JOB_OBJECT_LIMIT_JOB_MEMORY` (the committed private memory of the
  whole tree), `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`, `KILL_ON_JOB_CLOSE`,
  `DIE_ON_UNHANDLED_EXCEPTION`, **no** `BREAKAWAY_OK` and no `SILENT_BREAKAWAY_OK`. A CPU cap is
  `JobObjectCpuRateControlInformation` with `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`. The session
  and total jobs carry only the job memory limit and the active-process limit.
- **Assignment with no window.** The command gets `CREATE_SUSPENDED`. After `spawn`, the process
  handle (held, so its id cannot be reused) is assigned in the nesting order above. The
  process's only thread is then found with a Toolhelp thread snapshot filtered by process id,
  opened, and resumed. This keeps `tokio::process::Command`, its pipes and `kill_on_drop` at
  every site. The process starts inside its job, and so does everything it starts.
  - **A failed assignment fails closed.** If any assignment or the resume fails, the suspended
    process is ended with `TerminateProcess` before any of its code runs, and the spawn reports
    `refused_start` with the reason.
  - **The snapshot's cost.** A Toolhelp thread snapshot walks every thread on the machine.
    Stage 1 measures it on a busy machine. If it is more than a few milliseconds, the resume uses
    the thread id from a `PROCESS_BASIC_INFORMATION` query instead.
- **Notices.** One I/O completion port per daemon is associated with every job.
  `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` and `JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT` mark the unit as
  limited. A memory limit on Windows makes allocations fail rather than killing anything, so on
  that message the daemon terminates the unit's job (`TerminateJobObject`) and reports
  `limited: memory`. This gives one behaviour on every OS: a unit that reaches its memory cap is
  stopped whole. An 80% warning uses `JobObjectNotificationLimitInformation` (a notification
  limit, not enforced) and gives one `near limit` notice per unit.
- **A session or total limit names the right unit.** The message on a session or total job
  comes from whichever process happened to allocate last, which may be small. The daemon does
  not blame that process: it stops the session's (or Imperium's) largest unit, preferring the
  fastest-growing, and reports `session_memory` (or `total_memory`) naming the unit it stopped.
- **The terminal.** ConPTY's spawn in `portable-pty` cannot take `CREATE_SUSPENDED`, so the
  terminal keeps its assign-after-spawn and tree walk (terminal plan D5) through
  `Limits::adopt`.
- **The daemon job's breakaway.** `BREAKAWAY_OK` stays on the daemon job, because opening a
  folder in Explorer must outlive the daemon. Units cannot use it: their own jobs lack it. Stage 1
  tests that a capped probe's `CREATE_BREAKAWAY_FROM_JOB` child either fails to start or stays in
  the unit job (F2). As a control, the same probe in a plain job with `BREAKAWAY_OK` must break
  away, which shows the probe really tries. If breakaway were to succeed through a nested
  hierarchy, the daemon job loses `BREAKAWAY_OK` and the opener uses a broker process outside it
  instead.

Rejected: `PROC_THREAD_ATTRIBUTE_JOB_LIST` through our own `CreateProcessW`. It assigns at
creation too, but needs our own pipe and environment plumbing at every site. The CAS broker and
the code-mode worker already need a custom launch, and they use `JOB_LIST` or suspend there.
Ordinary sites do not need it.

### L3. Linux: a delegated cgroup v2 tree, RLIMIT and a poll where there is none

**Accepted: the daemon runs in a delegated cgroup v2 subtree and gives every unit a child
cgroup. Where no subtree is delegated, the daemon falls back to per-process rlimits plus the
daemon-side poll of L4.**

- **When.** The daemon sets up its subtree at start, **before** it takes the single-instance
  lock and writes `daemon.json`, so a daemon that fails here has published nothing. Every step
  falls back cleanly: nothing is executed again, and a failure leaves the daemon where it was, in
  fallback mode.
- **Skipped inside a unit.** A daemon that already sits inside an Imperium unit (for example one
  an agent's command started on demand) does not move. It stays inside that unit's caps and
  runs in fallback mode. This is detected by **structure**, not by a name pattern: the daemon
  walks up from its own cgroup (`/proc/self/cgroup`), and if any ancestor holds both an
  `imperium-daemon` child and a `total/` child (the layout below), it is inside an Imperium
  tree. A scope's name can be anything and proves nothing. The `IMPERIUM_LIMITS_UNIT` marker the
  launcher sets on every child is only a hint, because a child can clear its environment.
- **Getting a delegated subtree, in order.**
  1. **Already delegated.** The daemon is in a cgroup that passes the M5 checks below (the
     package's systemd user unit, if the user installs one, sets `Delegate=yes`).
  2. **A transient scope over D-Bus.** The daemon asks the user's systemd manager to move its
     own process into a new scope: `StartTransientUnit("imperium-core-<random>.scope", "fail",
     [("PIDs", [own pid]), ("Delegate", true), ("CollectMode", "inactive-or-failed")], [])`. The
     call is made with `busctl --user call org.freedesktop.systemd1 /org/freedesktop/systemd1
     org.freedesktop.systemd1.Manager StartTransientUnit ...`, with `busctl` found by absolute
     path (`/usr/bin`, then `/bin`), and the daemon then waits up to 2 s for
     `/proc/self/cgroup` to show the scope. The process is not executed again, so the daemon's
     arguments, environment and open files are untouched.
     - **A late move cannot happen.** On timeout the daemon calls `StopUnit` on the scope it
       asked for, then reads `/proc/self/cgroup` once more. If the move did happen after all, it
       uses the scope; if not, it falls back. Either way it decides once, before the lock.
     - **Moving out of a root-owned cgroup.** A process in a cgroup owned by root (a login
       session's `session-N.scope`, which `logind` creates) cannot be moved by the user manager
       alone. systemd then asks PID 1 to do it (`AttachProcessesToUnit`, systemd 238 or later,
       A2). Where that is missing, the call fails and the daemon falls back.
     - **The clients wait long enough.** `imperium-client`'s launch and the desktop's daemon
       start both wait 20 s for `daemon.json` (`START_TIMEOUT` in
       `crates/imperium-client/src/launch.rs` and `apps/desktop/src-tauri/src/daemon.rs`). That
       covers the 2 s here and the call's own time with room to spare. Stage 2 asserts that the
       setup's worst case stays under a tenth of it. Cost: one `busctl` process and one
     D-Bus round trip at start, about 20 to 50 ms (estimate, measured in stage 2). A new scope
     holds only the daemon, which also meets M5's "alone in the cgroup" rule.
  3. **`IMPERIUM_CGROUP_ROOT=<path>`** names a subtree the administrator delegated (for CI,
     servers and containers: the server package, item 63). It is honoured only for a daemon
     started by hand or by the autostart entry, never for an on-demand start (L7), and only
     after the M5 checks. It selects **where** the units live, never a cap's value (R2).
  4. **None of these:** fallback mode, reported by `imperium limits status` and in Settings.
- **M5: checks before using a subtree.**
  - The path is on a `cgroup2` filesystem (`statfs` magic), under `/sys/fs/cgroup`, owned by
    the daemon's user (not root), and the daemon is not running as root.
  - `cgroup.controllers` lists both `memory` and `pids`. Without them the caps cannot be set, so
    the daemon falls back. `cpu` is used where it is listed, and its absence only disables CPU
    caps.
  - The daemon is **alone** in its cgroup (`cgroup.procs` lists only it). Under cgroup v2's
    no-internal-process rule, enabling controllers for children fails while other processes
    share the cgroup, and moving them is not the daemon's business. Otherwise it falls back.
  - Setup works on an opened directory handle, with every later file opened relative to it,
    and reads each value back after writing it. #328's setup checks a path and then writes to
    it; a path swapped between the two would be written blindly. The rebase onto
    `imperium-limits` fixes that.
- **systemd-oomd.** Where `systemd-oomd` runs, it can kill a whole user scope under memory
  pressure. The scope asks for `ManagedOOMPreference=avoid`, but the plan **expects it not to be
  honoured**: systemd applies that preference only to cgroups owned by root, and a user unit's
  is not. The protection is the caps themselves (a unit stops at its cap long before the scope
  is under the pressure oomd acts on) and the watchdog. Stage 2 records what oomd does on the
  CI image.
- **Layout.** `<scope>/imperium-daemon` (the daemon, a leaf, as in #328), `<scope>/models/` (model
  runtimes, outside the total), `<scope>/terminals/` (uncapped terminals, S2, outside the total),
  `<scope>/total/` with `internal/` and `s-<session>/`, and units below them (`cmd-p3`,
  `mcp-<name>`, `hook-<n>`, `script-<pid>`, `cas`, `browser`). The controllers enabled are
  `memory`, `pids` and `cpu` (where delegated).
- **Per unit.** `memory.max`; `memory.swap.max = 0`, so a capped unit is stopped rather than
  pushed to swap (models are exempt, see [L6](#l6-default-caps-per-class)); `memory.oom.group =
  1`, so the kernel kills the whole unit, not one process of it; `pids.max` set from the unit's
  **task** cap (it counts threads as well as processes, so Linux task caps are larger than the
  Windows process counts, L6); `cpu.max` for a CPU cap. Session and total cgroups carry
  `memory.max` and `pids.max`.
- **Joining with no window.** A hidden launcher, `imperium-core limit --cgroup <path> [--rlimit
  ...] [--oom-adj N] -- <program> <args>`, writes its own process id to the unit's
  `cgroup.procs`, reads `/proc/self/cgroup` back and refuses to go on (exit 125, reported as
  `refused_start`) unless it shows the unit, sets what it was asked to, sets
  `IMPERIUM_LIMITS_UNIT=<class>`, and then `exec`s the program. `exec` replaces it, so no extra
  process stays, and nothing of the program runs outside the unit. It needs no `unsafe` (a file
  write and `CommandExt::exec`), unlike a `pre_exec` hook in the daemon, which the workspace
  forbids. Cost: one extra `exec`, about 1 to 3 ms (estimate). For the OS tier the launcher
  comes first: `limit → bwrap → sh -c`. (Revision 2's `IMPERIUM_CGROUP_REEXEC` marker is gone
  with the re-exec. Apart from `IMPERIUM_LIMITS_UNIT`, which grants nothing, no launcher variable reaches children.)
- **Notices.** `memory.events` (`oom_kill`, `max`) and `pids.events` (`max`) are watched with
  inotify, which cgroup files support. A unit whose `oom_kill` count rose reports
  `limited: memory`, and `pids.events max` reports `limited: tasks`. Stop is `cgroup.kill`
  (kernel 5.14 or later), else the freeze-and-kill walk. The 80% warning comes from polling
  `memory.current` once a second.
- **Fallback (no delegation).** The launcher sets `RLIMIT_DATA` (not `RLIMIT_AS`, which large
  virtual reservations trip, as C1 found) to the unit's cap and `RLIMIT_CORE = 0`, and raises its
  own `oom_score_adj` by 500 (allowed unprivileged, since it only raises it), so the kernel's OOM
  killer picks a child before the desktop or the daemon. `RLIMIT_DATA` is per process, so the
  tree is bounded by L4's poll, which reads the unit's process tree (`/proc/<pid>/task/*/children`
  from the launcher's process group) every 250 ms, sums PSS, and kills the group past the cap.
  Process count: the same poll, plus the freeze-and-kill walk past the cap. `RLIMIT_NPROC` is
  **not** used: it counts all of the user's processes, so a child's limit would fail on the
  user's own unrelated processes.
- **Signalling a group.** `killpg` is sent only while the group's leader is a child the daemon
  has not reaped, so the group id cannot have been reused. After the leader is reaped, the walk
  signals each process through a pidfd whose start time was checked (the terminal supervisor's
  rule).

Rejected:

- **`systemd-run --user --scope` per spawn.** It needs a D-Bus round trip (tens of milliseconds)
  on every command, hook and MCP start, and a fallback where no user manager runs anyway.
- **Re-executing the daemon under `systemd-run` at start** (revision 2). A failure half-way
  leaves two daemons racing for the lock, the arguments and environment pass through a second
  program, and a fallback needs a second exec. Moving the running process over D-Bus has none
  of these problems.
- **`zbus` for the D-Bus call.** It would avoid the `busctl` process, but it adds a large
  dependency for one call at start. It is the revisit if `busctl` proves missing on supported
  distributions.
- **A daemon-wide cgroup without per-unit children.** It protects the host but cannot say which
  unit broke its cap, and it stops every session to stop one.
- **`clone3(CLONE_INTO_CGROUP)`.** It is the atomic form, but std has no way to call it without
  `unsafe` `pre_exec`-style code, and the launcher gives the same guarantee.

### L4. macOS: a daemon-side poll, no jetsam

**Accepted: macOS units are bounded by a daemon-side poll of each unit's process tree, with
rlimits where Darwin honours them. CPU caps are not enforced on macOS, and say so.**

- **Memory.** #328's watchdog, generalised to trees: every 100 ms (50 ms for the CAS, script and
  hook classes, which are small and short), sum `ri_phys_footprint` (`proc_pid_rusage`) over the
  unit's processes (its process group, and descendants found with `proc_listchildpids`).
  `SIGSTOP` the group past the cap, then `SIGKILL` it (`killpg` only while the leader is
  unreaped, as on Linux). Overshoot is bounded by the allocation
  rate times the interval. The CAS measured 22 to 56 MiB over at 50 ms; stage 1 measures the
  overshoot at 100 ms and 05 states it.
- **Processes.** The same poll counts the tree and runs the freeze-and-kill walk past the cap.
- **rlimits.** `RLIMIT_CORE = 0` always. `RLIMIT_DATA` best-effort (Darwin does not enforce it,
  as C1 found). `RLIMIT_CPU` is not used for caps: it is total CPU time, which would kill
  long-running servers.
- **CPU.** No hard cap exists for third-party code. Units of agent classes run at
  `PRIO_DARWIN_BG` (background priority) so the interface stays responsive, and a CPU cap
  setting is shown as "not enforced on macOS".
- **Residual.** A process that leaves its process group and is reparented to `launchd`
  (`setsid` and a double fork) leaves the poll's tree. This is the same residual as background
  commands and terminals have on macOS today ([05](../05-tools-and-isolation.md#background-commands-implemented)).

Rejected: jetsam limits. `memorystatus_control` and `posix_spawnattr_setjetsam_ext` are private
interfaces that need entitlements Imperium cannot hold. `RLIMIT_AS` is accepted but not enforced
on current macOS (the CAS plan measured this).

### L5. Containers, Docker and WSL get their own flags

- **The container tier.** `container_args` adds `--memory=<cap> --memory-swap=<cap>
  --pids-limit=<cap>` and, with a CPU cap, `--cpus=<n>`. The container's memory sits in the
  container runtime's cgroup, not in ours. So the session aggregate counts a container at its
  cap when it starts (S6; admission: a container whose cap does not fit in what the session has
  left is refused, with the reason). Each run writes `--cidfile`, and the container is stopped
  by that **ID** (`docker rm -f <id>`), not by its name.
- **Rootless runtimes.** Rootless Docker or Podman can apply `--memory` and `--pids-limit` only
  where the user's cgroup is delegated. Where the runtime reports that it cannot (a warning from
  `run`, or `docker info` / `podman info` showing no cgroup v2 delegation), the flags are
  dropped. The container is then polled with `stats`, its status says "limits not enforced by
  the container runtime", and the watchdog still covers it.
- **vLLM in Docker.** In `enforce` mode, `docker_run_args` adds the same flags with the model
  class's cap.
- **vLLM in WSL2.** A job on `wsl.exe` holds nothing, because vLLM runs in the WSL VM. The VM
  itself is bounded by `.wslconfig` `memory=`, which defaults to half the host's RAM. Imperium
  reads the effective value and shows it, and never writes `.wslconfig` (a user file that
  affects every distribution). In `enforce` mode, when the distribution runs systemd, the serve
  command is wrapped in `systemd-run --user --scope -p MemoryMax=<cap>`. Otherwise only the VM's
  bound applies, and the model's status says so.
- **Warn mode sets no enforced limit (B3).** In `warn` (`limits.mode`, or `limits.models.mode`
  for models, which start in `warn`) no OS-enforced limit is set anywhere. That means no
  `--memory` or `--pids-limit`, no WSL `MemoryMax`, no job memory or process limit, and no
  `memory.max` or `pids.max`. Warn mode may only set notification limits: a Windows
  notification limit, and on Linux and macOS the poll. Containers and WSL are polled instead
  (`docker stats` every 10 s, and for WSL the host's `vmmemWSL` process), and a crossing gives a
  `warned` notice.

### L6. Default caps per class

Caps are per **unit** (a whole process tree), not per process. "RAM" is the machine's physical
memory as `hardware` reads it (with `[hardware]` overrides). The 64 GB column is the user's
machine. "Processes" is the Windows active-process limit and the macOS poll's count; "Linux
tasks" is `pids.max`, which counts every thread as well as every process (B2), so it is set much
higher. A build with 16 rustc processes of 20 threads each is already 320 tasks, and Chromium
starts hundreds of threads. Stage 1 measures a real `cargo build` of this workspace and a
headless Chromium session in CI and adjusts the task numbers before any class enforces them.

| Class | What it covers | Memory | 64 GB machine | Processes (Windows, macOS) | Linux tasks | CPU |
| --- | --- | --- | --- | --- | --- | --- |
| `command` | `run_command` (foreground and background), scheduled and auto-fix commands, environment runtimes, extension installs (`git clone`, plugin setup) | 25% of RAM, between 2 and 16 GiB (S1) | 16 GiB | 1,024 | 16,384 | none; below-normal priority |
| `terminal` | the user's terminal panes, behind the desktop gate (L13) | **not capped** (S2) | none | none | none | none; normal priority |
| `mcp` | stdio MCP servers, plugin MCP servers | 2 GiB | 2 GiB | 256 | 4,096 | none; below-normal |
| `hook` | command hooks (and the existing 300 s clamp) | 2 GiB | 2 GiB | 64 | 1,024 | none; below-normal |
| `cas` | the CAS server tree (outer unit only) | D4's formula: twice D3's per-call memory + 512 MiB | 2.5 GiB | D4's measured limit (3 on Windows), 8 on macOS | 256 | D3's per-call CPU time stays |
| `script` | the code-mode worker | twice `script_memory_mb` (C5) | 512 MiB | 1 | 64 | C5's budgets stay |
| `browser` | the CDP browser | 4 GiB | 4 GiB | 512 | 4,096 | none |
| `internal` | git, `gh`, probes, the updater (installs are `command`) | 2 GiB | 2 GiB | 128 | 1,024 | none |
| `model` | llama.cpp, whisper, vLLM, the planned `ncm-server`, the media sidecar | 1.5 × the planner's RAM estimate + 2 GiB, at most RAM − the reserve | per model | 128 | 8,192 | none |
| session aggregate | every capped unit of one session (not models, not terminals) | 50% of RAM, at most 48 GiB | 32 GiB | 4,096 | 32,768 | none |
| total | every capped unit except models | 75% of RAM − the reserve | about 43 GiB | 8,192 | 65,536 | none |
| watchdog reserve | free memory the watchdog keeps | 8% of RAM, at least 2 GiB | 5.1 GiB | n/a | n/a | n/a |

Reasons:

- **Commands are sized for real builds.** A Rust build of this workspace or a webpack build can
  take several gigabytes legitimately, so commands get a quarter of RAM. The incident's process
  (20 GB) is stopped at 16 GiB on the user's machine. A user who wants a tighter guard sets it
  lower. The 2 GiB floor matters on small machines: below 8 GiB of RAM, 25% would be under
  2 GiB, so the floor applies (on a 4 GiB machine a command may use half of RAM).
- **Small classes are small.** MCP servers, CAS and the script worker have no legitimate reason
  to use gigabytes. A Playwright-style MCP server that starts a browser is the largest real case
  seen, and fits 2 GiB. Hooks get 2 GiB too, since a hook may run a formatter or a linter on the
  whole project.
- **Extension installs are commands.** An install runs a repository's own code paths (`git
  clone` with its filters, a plugin's setup), and it can be large, so it takes the `command` cap,
  not the small `internal` one.
- **Models are large and legitimate.** Their cap is derived from the residency planner's own
  estimate (`Footprint::required_bytes`), so it is a leak guard, not a tight bound. It is
  `warn` before `enforce` (see [rollout](#rollout)). Memory-mapped weights count differently per
  OS: they are file-backed, not committed, so a Windows job does not count them, while a Linux
  cgroup charges their page cache. `memory.max` above the estimate makes the kernel reclaim
  page cache before it kills anything. Model units keep swap (no `memory.swap.max`).
- **CPU is not capped by default.** A spin degrades the machine but does not take it down the
  way memory does. Lower priority for agent classes keeps the interface responsive, and a CPU
  cap is one setting away (S5).
- **The terminal is the user's** (S2), so it gets normal priority and no caps, and it is not
  counted in the session aggregate. Only agent-started processes are capped.

### L7. Settings: user layer only, narrowing elsewhere

**Accepted: a `[limits]` section in the user config. Only the user file and the app layer that
Settings writes can raise a cap. Every other source can only lower one, environment variables
included. As S3 says, this holds against everything except a tier-0 process with the user's
rights, which is detected rather than prevented.**

```toml
[limits]
mode = "enforce"                 # "enforce", "warn" (notification limits only, L5) or "off" (no caps; the watchdog stays)
commands.memory_mb = "auto"      # "auto" (the L6 default) or MiB
commands.processes = "auto"
commands.cpu_percent = 0         # 0 = no cap; else percent of the whole machine
mcp.memory_mb = "auto"           # likewise for mcp, hooks, browser, internal (terminals: none, S2)
models.mode = "warn"             # models: "enforce", "warn" or "off", separate from `mode`
models.margin_mb = 2048
session.memory_mb = "auto"
session.processes = "auto"
total.memory_mb = "auto"
watchdog.enabled = true
watchdog.reserve_mb = "auto"
mcp.servers."playwright".memory_mb = 4096      # a server by name, wherever it is defined
environments."<environment id>".commands.memory_mb = 32768   # one environment's commands
```

- **Per server.** A stdio server in the user config may carry `limits = { memory_mb = ...,
  processes = ... }` (`mcp.servers.<name>.limits`). The user layer can also set a cap for a
  server **by name** wherever that server is defined (`limits.mcp.servers."<name>"`), which
  covers plugin and project servers. A `limits` field in a plugin's `.mcp.json` or a trusted
  project's servers is ignored with a warning in `mcp.list`; those servers take the `mcp` class
  cap unless the user names them.
- **Per environment.** `limits.environments."<id>".commands.memory_mb` (user layer only) raises
  or lowers the command cap for one environment's sessions. This is how the user lets agents run
  `scripts/check-rust.sh` at its full S7 cap. A unit cannot exceed the unit it runs in, so
  inside an agent's command unit the 16 GiB command cap binds, and the script's 32 GiB launcher
  cap cannot apply (L12).
- **Environment variables narrow.** `IMPERIUM_LIMITS__*` may only lower a value, like the
  project layer, because an on-demand daemon can inherit the environment of whatever started it
  (an `.mcp.json` `env` block that runs `imperium`, an agent's command). A higher value is
  ignored and reported by `config explain`.
- **The on-demand rule keys on `--exit-when-idle` (R1).** Both on-demand starts already pass that
  flag, so no new flag is added:
  - `imperium-client`'s launch (`crates/imperium-client/src/launch.rs`), which starts
    `imperium-core --exit-when-idle[=SECONDS]`;
  - the desktop's daemon start (`start` in `apps/desktop/src-tauri/src/daemon.rs`, which adds
    `--exit-when-idle` unless the user keeps the daemon running).

  A daemon started with `--exit-when-idle` ignores `IMPERIUM_CGROUP_ROOT`, and both sites also
  remove it and `IMPERIUM_LIMITS__*` from the daemon's environment. A desktop set to keep the
  daemon running starts it without the flag. That start is treated as a hand start, which is
  right: the desktop is the user's own program, and its environment is the user's. It still
  strips the two variables when it spawns.
- **`IMPERIUM_CGROUP_ROOT` is a tier-0 residual (R2).** It selects only **where** the units'
  cgroups live, never a cap's value, so the worst it can do is point the daemon at a subtree
  whose limits someone else controls. A tier-0 process that can set the environment of a hand
  or autostart start can already edit `config.toml`. At every start, the daemon writes the
  mechanism it chose (`cgroup`, `rlimit + poll`, `job` or `poll`) and the root path to the change
  journal, and a change from the last start gives L7's toast.
- **The environment and project layers narrow.** `limits.*` joins the environment layer's
  allow-list as narrow-only ([10](../10-configuration.md#layers)): a value at or below the user's
  is applied, a higher one is listed as refused by `imperium env config` and `config explain`.
  `limits.mode` may only move towards `enforce` (S3).
- **Agents cannot raise them.** No tool takes a cap. `run_command` gets no new parameter.
  `config.toml` and `models.toml` are refused to the file tools (pull request #304). `imperium
  config set|unset limits.*` from an agent's command hits the safety floor (asked every time,
  never sticky, in every mode), the rule pull request #330 adds for `runtime.*` keys. The
  exception is `limits.mode`, which is refused outright to agents (S3). The OS tier
  already hides the daemon's endpoint and socket, so a confined command cannot call `config.set`.
- **Floors.** Values below a minimum (64 MiB, 8 processes, 5% CPU) are refused when saved, so a
  typo cannot make every command fail. `mode = "off"` is allowed and user-only (S3): Settings
  shows it as a warning, and the watchdog has its own switch.
- **Changes apply to new units.** A changed cap applies to processes started afterwards.
  Running units keep their caps (`restart: false`, no daemon restart needed), except that
  lowering the total or the reserve applies to the watchdog at once.
- **Every raise is detected and announced (B4).** The daemon keeps the effective limits it last
  ran with in `<data>/limits-seen.json`. A raised cap or a changed `limits.mode`, from any
  source, gives a toast and an entry in the change journal (Settings → *Changes*), naming:
  - the source: `config.set` with the connection's peer, a file changed between starts or while
    running (the daemon watches `config.toml` and `models.toml`), or an environment variable;
  - the old and new values.
  This is how a tier-0 edit of `config.toml` is caught: not prevented, but never silent. Running
  units whose OS cap no longer matches the daemon's record (a tier-0 write to `memory.max`, a job
  changed through a duplicated handle) are found by a 30 s check, and get the same toast and
  entry.

### L8. Reuse, not double limits

The rule: an existing per-call or per-worker limit stays where it is and keeps its meaning.
This plan supplies only the **outer unit** and the aggregate, with one OS object per process,
never two at the same level.

- **CAS.** D3's per-worker limits (`RLIMIT_AS`, `RLIMIT_CPU`, the footprint poll, the broker's
  per-worker jobs) are unchanged. The outer unit is D4's server job on Windows, created through
  `imperium-limits` and nested in the session job, with D4's formula as the `cas` class cap. On
  Linux and macOS the CAS server tree gets a `cas` cgroup or poll unit with the same formula. No
  per-call unit is added.
- **Code mode.** C5's `script_memory_mb` keeps Monty's own limit and the OS backstop at twice
  it. That backstop *is* the `script` unit: #328's Windows job, cgroup and macOS watchdog move
  into `imperium-limits` unchanged in value. There is still one job per worker, now nested in
  the session job. The code-mode plan's "cgroups when delegated" (C1) is delivered by this
  plan's L3.
- **Terminals.** The terminal plan's job and Linux supervisor stay. The job gets no limits
  (S2), so the unit only provides the stop, and the supervisor uses `cgroup.kill` on its unit where one is delegated
  (the "robust future form" that plan names), keeping its walk as the fallback.
- **Background commands.** The background-command job becomes the unit job (assigned while
  suspended, so the tree walk is needed only for terminals). The 8 running and 32 kept per
  session, and the 1 MiB of output, are unchanged.
- **Residency.** The controller still owns model memory decisions. A model unit stopped at its
  cap reports to it as a crash (`crashed`, `detail: memory limit 12.0 GiB`), so the existing
  crash backoff applies. The watchdog asks the controller to unload idle models (new reason
  `pressure`) rather than killing model processes itself.

### L9. Tamper resistance

- **OS tier (Linux).** The cgroup tree is under `/sys/fs/cgroup`, which bubblewrap's
  `--ro-bind / /` mounts read-only. Stage 3 adds a test that an OS-tier command cannot write its
  unit's `memory.max` or `cgroup.procs`. `$XDG_RUNTIME_DIR` is already hidden, so `systemd-run
  --user` cannot start a process outside the sandbox
  ([05](../05-tools-and-isolation.md#isolation-tiers)).
- **OS tier (macOS).** Seatbelt denies signals outside the sandbox, and the poll lives in the
  daemon.
- **Containers.** The container runtime enforces the flags. The container cannot change them.
- **CAS and code mode.** AppContainer, Low integrity, seccomp and Landlock already deny the
  calls that would reach a job or a cgroup.
- **Tier 0.** Not tamper-proof (see the [threat model](#threat-model)).

### L10. Notices: the agent, the interface and the journal

**Accepted: one event, `resource.limited`, for every stop or warning, plus a plain-text result
where an agent is waiting.**

- **Protocol.** `resource.limited { session_id?, agent_id?, class, unit, limit, value, peak,
  action, setting }`. `limit` is one of `memory`, `processes`, `tasks`, `cpu`, `session_memory`,
  `session_processes`, `total_memory`, `system_pressure` or `cap_changed` (L7's detection). `action` is `stopped`,
  `refused_start`, `near_limit` or `warned` (warn mode). `setting` is the key that raises it.
  This adds a protocol minor version.
- **What the agent sees.**
  - A foreground command's result: `stopped: memory limit 16 GiB reached (peak 16.0 GiB); the
    command and everything it started were stopped. The cap is the user's setting
    limits.commands.memory_mb; it cannot be raised from here.` The last line tells the model not
    to retry the same thing in a loop.
  - A background command: the existing completion notice gets `ended: memory limit ...` in the
    same `<background-notice>` block.
  - An MCP server: the tool call in flight fails with the same text, and the server's status in
    `mcp.list` becomes `failed: memory limit`. It is restarted on the next call at most three times
    in 10 minutes, then stays failed until the session restarts it.
  - A hook: it fails as a hook failure does today, with the limit in the reason.
  - CAS and code mode keep their own `limit` messages (D3, C5). The outer unit's stop reads as
    `crashed` there today, and becomes `limit: session memory` or `limit: memory`.
  - A model: `model.residency` with `crashed` and the detail.
- **What the interface shows.** A toast and a transcript line (a `notice`) for each `stopped`
  and `refused_start`; the resource monitor ([04](../04-models-and-routing.md#active-models-and-resource-monitor))
  lists each unit's memory against its cap; Settings → Limits shows the effective caps, the
  mechanism per OS (`job`, `cgroup`, `rlimit + poll`, `poll`) and whether a subtree is
  delegated; `imperium limits status` prints the same.
- **Data, not instructions.** The unit label (a command line, a server name) comes from outside,
  so it is cut to 200 characters and cleaned the way background notices are (control characters
  and harness tags defused).

### L11. The daemon watchdog

**Accepted (S4): a dedicated OS thread in the daemon samples the machine's free memory and
stops Imperium's own units, lowest value first, before the machine runs out, whatever is causing
the shortage.**

- **Readings.** Every second, and every 250 ms while below twice the reserve:
  - Windows: `GlobalMemoryStatusEx`, both available physical memory and available commit,
    because exhausted commit is what breaks Windows. A
    `CreateMemoryResourceNotification(LowMemoryResourceNotification)` wakes it early.
  - Linux: `MemAvailable` from `/proc/meminfo` and `some avg10` from `/proc/pressure/memory`
    (PSI, where present).
  - macOS: `vm_statistics64` (free + inactive + purgeable) and a `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`
    source that wakes it on a warning.
- **Acting.** Below the reserve for two samples in a row, it acts once, then waits 2 s before
  acting again. In order:
  1. stop the lowest-value unit that uses more than 256 MiB: `internal` and `browser` first,
     then lingering descendants of finished foreground commands (L14), then background
     `command`s, `mcp` and `hook`, then `cas` and `script`, then foreground `command`s. Within a
     group it picks the **fastest-growing** unit (memory gained over the last 10 s), then the
     largest;
  2. ask the residency controller to unload idle models (`pressure`);
  3. only below half the reserve for 5 s: stop a model with a call in flight (`pressure`).
  It never stops the daemon, the desktop app, the user's terminals (S2) or any program Imperium
  did not start, even when the pressure comes from elsewhere. Each act is a `resource.limited`
  with `system_pressure`, naming the free memory and the reserve.
- **Checking that it helped (M1).** After each stop, the watchdog checks the next three samples.
  If free memory has not recovered by at least half of what the stopped unit used, or is still
  below the reserve with no Imperium unit over 256 MiB left, it treats the cause as **external**.
  It then stops acting except at critical (below half the reserve), so it does not take
  Imperium apart for a shortage it did not cause. It also shows one notice naming the five
  non-Imperium processes using the most memory (name, process id, usage; read only, nothing is
  done to them). It goes back to normal once free memory has been above twice the reserve for
  a minute.
- **Self-protection.** The thread is started at daemon start with its buffers allocated, runs at
  above-normal priority, and takes no lock that a session holds, so it keeps running when the
  rest of the daemon is slow. The total job or cgroup keeps children from taking the daemon's own
  memory in the first place. The watchdog is the backstop for what caps cannot see: models,
  containers and many units each under their caps.
- **Warn mode.** With `limits.mode = "warn"` the watchdog still acts. It is the last line of
  defence and has its own switch, `limits.watchdog.enabled`.

### L12. The repository's own tests

F5's runaway ran outside Imperium. The launcher in L3 is also usable on its own:
`imperium-core limit --memory 4G --processes 256 -- <command>` puts any command in a unit (a job
on Windows, a cgroup or rlimits and a poll elsewhere). Used on its own there is no daemon to
poll, so on macOS and on Linux without delegation the launcher does **not** `exec` (M7). It stays
as the supervising parent: it starts the command in its own process group, runs L4's poll over
it, forwards signals and returns the command's exit code. On Windows it holds the job and waits.
Only with a delegated cgroup, where the kernel enforces the caps, does it `exec`. Stage 2 wraps `scripts/check-python.sh`'s
pytest run in it on every OS (4 GiB by default, `IMPERIUM_TEST_MEMORY_MB` to change). It
also wraps `scripts/check-rust.sh` if that costs only the launcher's `exec` (50% of RAM, at most
32 GiB, `IMPERIUM_RUST_MEMORY_MB`; S7). When an agent runs `check-rust.sh` inside its command
unit, the smaller command cap (16 GiB on the user's machine) binds, because a unit cannot exceed
the one it runs in. The user can raise one environment's command cap (L7) if agents should run
the full build there. Stage 2
also removes the CAS tests' `unlimited_on_windows` opt-in for in-process corpus and fuzz tests,
which then run inside that unit. A development agent that runs the scripts gets the cap without
having to know about it.

### L13. Uncapped terminals only behind the desktop gate

S2 leaves the user's terminals uncapped. That would be an escape if anything other than the
user could create a terminal or type into one. A terminal is uncapped only while both its
creation and every input it has received came through the terminal plan's gate. That gate is
`app_identity::InstalledApp` plus `peer::may_grant`, checked once per connection and cached
(the [terminal plan](terminal-file-panes.md#decisions), stage 2b, roadmap item 55).

- **Any other terminal is a command unit.** A terminal created on a connection that did not
  pass the gate (a stub gate in a test build, a future client such as the phone), or one that
  receives any `terminal.input` from such a connection, is placed under the `command` class caps
  from that moment. It stays capped until it is closed.
  - **Caps set in place (R7).** The terminal keeps its own job or cgroup, and the caps are set on
    it where it is: `SetInformationJobObject` on its existing job on Windows, `memory.max` and
    `pids.max` written to its own cgroup under `terminals/` on Linux, and the poll starts
    enforcing on macOS. Nothing is moved, so no process runs outside a unit during the switch.
  - **Counted by the daemon.** The terminal's unit is not under the session's job or cgroup, so
    the OS does not count it in the session aggregate. The daemon does: it adds the unit's usage
    to the session's total in its own accounting, and a session over its aggregate stops its
    largest unit, which may be this terminal.
  - **A watchdog candidate.** It joins the watchdog's order (L11) in the foreground-command
    group, unlike a gated terminal, which the watchdog never touches.
- **A stronger gate, if wanted.** A per-launch secret that the desktop's native side gets over
  its pipe at start and never writes to disk (a lease) would tie `terminal.*` to the running
  desktop process rather than to its identity. The plan does not require it, because the gate
  above is what item 55 specifies. It is the revisit if the gate is weakened or a non-desktop
  client gets terminals.
- **Mods do not reach `terminal.*`.** The mod host bridge (commit `c0b3a9b8`,
  `apps/desktop/src/mods/ModFrame.tsx`) answers only the methods in its permission table:
  `sessions.list`, `sessions.selected`, `pets.list`, `pets.strip`, `storage.get` and
  `storage.set`. Anything else is `unknown method`. Stage 7 adds a test that the table has no
  `terminal.*` entry.
- **Residual.** `may_grant` is not a boundary against a process that evades it, as 06 says. A
  tier-0 process that does so could get an uncapped terminal, the same tier-0 residual as
  editing `config.toml`.

### L14. What a unit's end means, per class

When a unit's main process exits while descendants still run (a dev server started with `&`,
`sccache`, a Gradle daemon, a language server), each class does this:

| Class | When the main process exits | Descendants left running |
| --- | --- | --- |
| `command`, foreground | the result is returned at once | they keep running in the unit, still capped and counted in the session, shown as *lingering* in the resource monitor and in the result ("2 processes still running: sccache, ..."). They are stopped when the session stops, is archived or deleted, or by the watchdog (first among commands) |
| `command`, background | as today: the command counts as running while any remain | as today, stopped by `command_stop` or with the session |
| `mcp` | the server is gone | stopped at once with the unit; the restart rule of L10 applies |
| `hook` | the hook's answer is read | stopped at once: a hook must not leave processes |
| `cas`, `script` | the server or worker is gone | stopped at once (they cannot start any) |
| `browser` | the browser is closed | stopped at once |
| `internal` | the tool's answer is read | stopped at once |
| `model` | the server is unloaded or crashed | stopped at once |
| `terminal` | the shell exited | the terminal plan's D5 (stopped) |

Build servers that should outlive one build should be started as background commands. That is
the one way to keep them across a session's turns without counting them against a foreground
command.

### L15. Other resources the watchdog watches

- **GPU memory.** The watchdog reads VRAM: NVML on NVIDIA, and `D3DKMTQueryVideoMemoryInfo`
  on Windows for any adapter. It reports VRAM in its notices and hands the readings to the
  residency controller, which owns VRAM decisions. It never stops a unit for VRAM.
- **Disk.** It checks free space on the volumes of `<data>`, the temporary folder and, on
  Windows, the pagefile, every 30 s. Below 2 GiB (or 2%) it gives a notice. A pagefile volume
  below 1 GiB is treated as memory pressure, because the commit limit can then no longer grow.
- **Shared memory and tmpfs.** On Linux, files in `/dev/shm` or another tmpfs are charged to the
  unit's cgroup but are **not** freed when the unit is killed. They stay charged to the parent
  cgroup until deleted. The OS tier has a private `/tmp` (freed with it), but a tier-0 command's
  `/dev/shm` files survive. The watchdog counts `Shmem` from `/proc/meminfo` in its readings and
  names a large value in its notice. It deletes nothing.
- **Pagefile-backed sections on Windows.** A section from `CreateFileMapping(INVALID_HANDLE_VALUE,
  ...)` is charged to the system commit, and it is not clear that it counts against a job's
  memory limit. Stage 1's `alloc --section` probe settles it. If such sections escape the job,
  the watchdog's commit reading is the backstop, and 05 states it.

## Scope and non-goals

In scope: every process Imperium starts on the three OSes, per-unit and aggregate caps, the
watchdog, settings, notices, CI tests, and the docs (05, 04, 10, 11, 13).

Not in scope: GPU memory caps, disk and descriptor caps (L15 only reads them), network
bandwidth, remote and SSH hosts
(their own daemon applies its own caps), and processes the user starts outside Imperium (except
the repository's scripts, L12).

## Assumptions

- A1. Windows 8 or later (nested jobs); the supported Windows 10 and 11 qualify.
- A2. Linux desktops with systemd 244 or later delegate `memory` and `pids` to the user manager,
  and its `StartTransientUnit` accepts a scope with the caller's own process and
  `Delegate=yes`. Moving a process that starts in a root-owned cgroup (a `logind` session
  scope, the usual case for a program launched from a desktop session) also needs PID 1 to
  attach it on the user manager's behalf (`AttachProcessesToUnit`, systemd 238 or later). `busctl`
  is at `/usr/bin/busctl` wherever systemd is. Stage 2 records what CI and common distributions
  show. Without these, the RLIMIT and poll fallback holds.
- A3. A suspended process created by `tokio::process::Command` has exactly one thread until it
  is resumed, so resuming the one thread a Toolhelp snapshot finds for its process id starts it.
  Stage 1 tests this.
- A4. `memory.oom.group` and `cgroup.kill` are present on the kernels in CI. Where `cgroup.kill`
  is missing (before 5.14), the freeze-and-kill walk stands in.
- A5. GitHub's runners allow `sudo` to create and `chown` a cgroup subtree (for the delegated-tree
  job), and `loginctl enable-linger` to start a user manager (for the D-Bus scope path).

## Relevant paths

- New: `crates/imperium-limits/**`, `crates/imperium-core/src/limits.rs` (the registry,
  sessions, the watchdog), `crates/imperium-core/src/main.rs` (the hidden `limit` launcher, the
  D-Bus scope move before the lock), `crates/imperium-protocol/src/limits.rs`.
- Changed: `crates/imperium-tools/src/{exec.rs,processes.rs,sandbox.rs,runtime.rs}`,
  `crates/imperium-core/src/scheduler.rs`, `crates/imperium-mcp/src/servers.rs`,
  `crates/imperium-extensions/src/hooks.rs`, `crates/imperium-local/src/{process.rs,speech.rs,vllm_env.rs,wsl.rs}`,
  `crates/imperium-web/src/browser.rs`, `crates/imperium-script/src/{cgroup.rs,watchdog.rs,windows/launch.rs}`
  (moved), `crates/imperium-pty/src/lib.rs`, `crates/imperium-config/src/{settings.rs,registry.rs}`,
  `crates/imperium-client/src/launch.rs` and `apps/desktop/src-tauri/src/daemon.rs` (the
  scrubbed variables),
  `apps/desktop/src-tauri/` (its spawn sites),
  `scripts/check-python.sh`, `sidecars/cas/tests/support.py`, `.github/workflows/ci.yml`, the
  desktop's Settings and resource monitor.

## Related docs

- [05 Tools and isolation](../05-tools-and-isolation.md): the tiers, background commands, MCP.
- [04 Models and routing](../04-models-and-routing.md#residency-controller-implemented): the
  residency controller and the resource monitor.
- [10 Configuration](../10-configuration.md#layers): layers and narrowing.
- [CAS plan](cas-mcp.md), [code-mode plan](code-mode.md),
  [terminal and file panes plan](terminal-file-panes.md#decisions).

## Stages

Each stage is one pull request against `experimental` of at most about 800 changed lines, with
the docs it changes. Every CI test of a cap uses the bounded probes of goal 9.

### Stage 1: `imperium-limits` and its probes

- Objective: the crate of L1 with the three OS back ends (L2, L3 with a given cgroup path and
  the fallback, L4), the completion-port and inotify notices, and a test-only `limit_probe`
  binary (`alloc <step-MiB> <ceiling-MiB> [--section]`, `fork <ceiling>`, `spin <threads>
  <secs>`, `breakaway`). It lands **before** #328; #328 then rebases onto it, replacing its
  three modules with calls into the crate and taking the M5 setup fix (L1).
- Depends on: none.
- Acceptance checks, `cargo test -p imperium-limits` on the matrix:
  - alloc bomb, fork bomb and CPU spin (goals 2 to 4) per OS and mechanism;
  - Windows:
    - the probe's `CREATE_BREAKAWAY_FROM_JOB` child stays in the unit or fails to start (F2),
      and the control breaks away from a plain `BREAKAWAY_OK` job;
    - the nesting order holds, and a session limit hit stops the session's largest unit, not
      the allocator;
    - a failed assignment leaves no running process;
    - A3, and the Toolhelp snapshot's cost on a runner with many threads;
    - `alloc --section` against the job limit (L15);
  - Linux, in a job that creates a delegated subtree with `sudo`: `memory.oom.group` kills the
    whole tree; `cgroup.kill` stops it; the fallback (no subtree) stops the alloc bomb through
    `RLIMIT_DATA` and the tree poll;
  - macOS: the measured overshoot at 100 ms, recorded in the outcome;
  - **measurements** for L6, recorded in the outcome: the peak tasks (Linux) and processes
    (Windows, macOS) of a real `cargo build` of this workspace and of a headless Chromium
    session, so the task and process defaults are set from data before stage 3 enforces them;
  - the units' CPU time is read from the job's accounting and `cpu.stat` (goal 4).
- Status: done (pull request #337).
- Outcome (2026-11-26):
  - **Delivered.** `crates/imperium-limits`: `Class`, `Caps`, `UnitSpec`, `Limits`,
    `UnitCommand`, `Unit` and `Ending`; the Windows jobs and completion port (`windows.rs`), the
    Linux cgroup tree, fallback poll and launcher (`cgroup.rs`, `linux.rs`, `launcher.rs`), the
    macOS poll (`macos.rs`); `limit_probe` (built only with `test-support`); `tests/caps.rs`;
    the `limits` CI job with `scripts/ci/limits-cgroup.sh` (a `sudo`-made subtree with a 6 GiB
    and 16,384-task ceiling of its own) and `scripts/ci/limits-measure.sh`. `unsafe` is allowed
    only in `windows.rs`, `macos.rs` and `notify.rs` (Tokio's `AsyncFd::register` for the
    inotify descriptor, `AsyncFd::new` being deprecated as unsound).
  - **#328's pieces, copied with their API.** `cgroup::{Cgroups, Lease, delegated}` (now
    directory-relative, the M5 fix), `macos::{Watchdog, resident_bytes}` and `windows::Job`
    (`limit`, `restrict_ui`, `assign` on a `BorrowedHandle`, `assign_child`, `terminate`).
    #328 merged first, so `imperium-script` still has its own `cgroup.rs`, `watchdog.rs` and
    job code. A follow-up (before stage 3) switches it to these calls, keeping its token,
    labels, pipes and handle list, and deletes its copies; until then the two copies agree in
    behaviour, except that only this crate's cgroup setup is directory-relative.
  - **Measured on the hosted runners** (4 CPUs each):

    | Check | Linux, cgroup | Linux, fallback | Windows, job |
    | --- | --- | --- | --- |
    | alloc bomb, 256 MiB cap, 64 MiB steps | stopped by `oom.group`, peak 256 MiB | refused by `RLIMIT_DATA` at 192 MiB | stopped, `PeakJobMemoryUsed` 1.2 to 1.25 MiB over the limit |
    | tree of two 192 MiB holders, 256 MiB cap | n/a | stopped by the 250 ms poll, sampled peak 291 MiB | n/a |
    | fork bomb | stopped at 256 tasks | n/a | stopped at 32 processes (sampled peak 31) |
    | four-thread spin, 25% cap, 3 s | 25.5% of the machine | not enforced (reported) | 28.5% (hard cap in a nested job works) |

  - **The L6 measurements** (Linux, a cgroup unit with no caps, sampled every 100 ms): a cold
    `cargo build --locked --workspace` peaked at 12 processes, 39 to 40 tasks and about 10.5 GiB
    charged (page cache included) over 260 to 281 s; a 20 s headless Chromium session on a real
    page (the CDP browser's flags) peaked at 14 to 20 processes, 115 to 179 tasks and 355 to
    503 MiB. **Defaults unchanged**: the `command` (16,384 tasks, 1,024 processes) and `browser`
    (4,096 tasks, 512 processes) caps sit two orders of magnitude above these, which a build on
    a machine with many more cores (cargo's jobs scale with them) still needs. The build's
    memory charge is mostly page cache, which `memory.max` reclaims before it kills; stage 3
    measures anonymous memory against the command cap on a small machine.
  - **F2 settled.** In a capped unit (nested total, unit, no `BREAKAWAY_OK`)
    `CREATE_BREAKAWAY_FROM_JOB` fails with access denied; the control in a plain
    `BREAKAWAY_OK` job breaks away. The daemon job keeps `BREAKAWAY_OK` for the folder opener.
  - **Nesting and sessions.** A unit's process is in its unit, session and total jobs, and a
    session memory limit stops the session's largest unit while the allocator that hit it
    goes on.
  - **Gap: pagefile-backed sections escape the job (L15).** `alloc --section` committed its
    whole 1 GiB ceiling under a 256 MiB job: section memory is not charged to the job's memory
    limit. The watchdog's commit reading (stage 6) is the only backstop, and 05 says so when the
    caps are documented (stage 8).
  - **Gap: the Toolhelp snapshot is not cheap (L2).** With 3,000 extra threads on the runner it
    took a median 33 ms (at most 46 ms) per spawn. L2's alternative needs the main thread's id
    without a snapshot, which `PROCESS_BASIC_INFORMATION` does not carry; `NtGetNextThread`
    (ntdll, not in windows-sys) enumerates only the process's own threads. Stage 3 adopts it
    (or `PROC_THREAD_ATTRIBUTE_JOB_LIST` for hot sites such as hooks) before hooks spawn through
    units, and stage 8 measures it. A3 holds: every suspended process had exactly one thread,
    and a spawn refuses to resume any other count.
  - **The job's peak tolerance.** The Windows alloc check allows 2 MiB over the cap, documented
    in the test: the limit is checked per commit request, while the peak also counts kernel
    charges made without one. A limit that let one more 64 MiB step through still fails.
  - **macOS.** The 100 ms poll's overshoot is printed by the alloc test (`MEASURE alloc`); the
    test bounds it at three 64 MiB steps. The value lands in the job summary of the `limits`
    job.
  - **Adjustments to the plan's API and scope.**
    - `Limits::command(spec, program)` returns a `UnitCommand` (a `tokio::process::Command`
      by `Deref`) that `spawn()` starts, instead of `spawn(spec, command)`: on Linux the
      program must be the launcher, which a finished `Command` cannot be changed to. Callers set
      arguments, environment and stdio as before; creation flags and the process group belong
      to the unit.
    - Not in stage 1, for the stages that wire spawn sites: the 80% `near limit` notice, the
      launcher's exit 125 reported as `refused_start` (stage 2 maps it; stage 1 reports it as an
      exit), `PRIO_DARWIN_BG` and below-normal priority, macOS session aggregates (the daemon's
      own accounting), `Limits::adopt` (stage 7) and the pidfd start-time check after the
      leader is reaped (the fallback and macOS check the group id instead).
    - About 4,300 lines rather than 800: half is the tests and probes the acceptance list asks
      for and #328's moved code with its tests.
  - **A neighbouring flake.** `imperium-pty`'s fork-bomb test set `ulimit -u` from the user's
    process count, but `RLIMIT_NPROC` counts threads. This change counted threads (`ps -L`);
    #342 then rewrote the test (bash, a task count, a refusal to start without the cap) and its
    version is the one kept.

### Stage 2: the daemon's cgroup, the settings and a capped launcher for scripts

- Objective: the cgroup bootstrap (L3's order: the D-Bus scope move before the lock, the
  M5 checks, the skip inside a unit), the `[limits]` settings and their registry rows (L7) with
  narrowing, narrow-only environment variables, the `--exit-when-idle` rule, the floor rule for `config set
  limits.*` and the outright refusal for `limits.mode`, the change detection and its journal,
  `imperium limits status`, the hidden `limit` launcher (exec or supervising, M7), and L12
  (check scripts and the CAS tests).
- Depends on: stage 1, and #330, which depends on #304 (the floor rule follows #330's pattern,
  and the `config.toml` refusal is #304's).
- Acceptance checks:
  - goal 7;
  - `imperium limits status` reports `cgroup`, `rlimit + poll`, `job` or `poll` correctly in each
    CI job, including a `loginctl enable-linger` job for the D-Bus path and one where the call
    fails (the daemon falls back, starts once, and writes `daemon.json` once);
  - the D-Bus path works for a daemon started from a root-owned cgroup and from a `logind`
    session scope (a CI job that starts the daemon under `systemd-run --scope` as root with
    `--uid`, and one under `loginctl`'s session);
  - a call that times out leaves the daemon either in the scope or in fallback, never moved
    after the lock (an injected slow `busctl`);
  - the setup's worst case stays under 2 s against the clients' 20 s wait for `daemon.json`;
  - a daemon started inside a command unit does not move, whatever the scope's name (the
    structural check, R6);
  - `IMPERIUM_CGROUP_ROOT` naming a root-owned path, a non-cgroup2 path, a shared cgroup or a
    cgroup without `memory` and `pids` in `cgroup.controllers` is refused, and ignored by a
    daemon started with `--exit-when-idle`;
  - every start writes the mechanism and the root path to the journal;
  - `python-cas` on Windows runs inside a 4 GiB unit.

### Stage 3: commands

- Objective: `run_command` (both kinds), scheduled and auto-fix commands, environment runtimes,
  and the container tier's flags (L5) through `imperium-limits`; the session aggregate; the
  `resource.limited` event and the agent-facing text (L10) for commands. Fixes F2 and F3 for
  commands.
- Depends on: stage 2.
- Acceptance checks: goals 2, 3, 5 and 8 through `run_command` in every tier on the matrix (the
  container tier where CI has Docker); an OS-tier command cannot write its unit's cgroup files
  (L9); a background command stopped at its cap gives its completion notice.

### Stage 4: MCP servers, hooks, the browser and daemon tools

- Objective: the `mcp`, `hook`, `browser` and `internal` classes, per-server user caps, the
  restart rule (L10), and the spawn-site source check (goal 1). The CAS outer unit is wired
  here if CAS stage 5 has landed, else when it lands.
- Depends on: stage 3.
- Acceptance checks: goal 1; a fake stdio server that allocates past its cap fails its call and
  shows `failed: memory limit`; a hook bomb is stopped and the hook reports the failure; a
  plugin `.mcp.json` `limits` field is ignored with a warning.

### Stage 5: local runtimes

- Objective: the `model` class for llama.cpp, whisper, vLLM (native, Docker, WSL as in L5) and
  the media sidecar, with caps from the planner estimate; the residency hooks (`crashed` detail,
  the `pressure` reason). Starts in `warn` (L7 `models.mode`). The planned `ncm-server` gets the
  class when it lands.
- Depends on: stage 3.
- Acceptance checks: a fake model server past its cap gives `warned` in warn mode, with no
  OS-enforced limit set (B3: no job memory limit, no `memory.max`, no Docker flags), and is
  stopped in enforce mode, reported as `crashed` with the detail; the WSL path reports the VM
  bound.

### Stage 6: the watchdog, the total cap and the interface

- Objective: L11, the total job or cgroup, Settings → Limits, the resource monitor's per-unit
  rows, and the toasts.
- Depends on: stages 3 and 5.
- Acceptance checks: goal 6 with an injected reader; the order of victims and the
  fastest-growing pick; the external-cause rule (M1); the VRAM, disk and `Shmem` readings in
  notices (L15); the watchdog's act with `limits.mode = "warn"`; the desktop shows the event (web
  tests).

### Stage 7: terminals

- Objective: terminal panes through `Limits::adopt` with the uncapped `terminal` class (S2),
  the switch to `command` caps for a terminal reached from outside the gate (L13), and
  `cgroup.kill` in the Linux supervisor where delegated (L8).
- Depends on: stage 3 and the terminal plan's stage 2a (#331).
- Acceptance checks: the terminal plan's stop tests pass through the unit; a terminal that
  allocates past the `command` cap is not stopped and is not counted in the session aggregate;
  the watchdog's victim order never includes a terminal; a terminal created or typed into by a
  connection that fails the gate (a stub gate) gets the command caps in place (its own job or
  cgroup), is counted in its session's aggregate and becomes a watchdog candidate; the mod
  bridge's permission table has no `terminal.*` method.

### Stage 8: review, measurement and enforce for models

- Objective: a security review pass over every spawn site and the tamper tests; measured spawn
  overhead per OS; models move to `enforce` if the measurements and a week of `warn` notices on
  `experimental` show no false stops; docs 05, 04, 10, 11 and 13 finished.
- Depends on: stages 1 to 7.

## Rollout

- Stages 1 and 2 add no caps to any agent-facing process (only the scripts of L12).
- Stages 3 and 4 ship in `enforce` for their classes with the L6 defaults, which are sized so
  ordinary builds and servers stay well under them.
- Models ship in `warn` in stage 5 and move to `enforce` only in stage 8.
- The watchdog ships enabled in stage 6.
- Rollback: `limits.mode = "warn"` or `"off"` (user layer) turns caps off without a new build;
  the watchdog has its own switch.

## Risks across stages

- **False stops of legitimate work** (a large link, a big test suite). Mitigated by sizing
  commands at a quarter of RAM, the 80% `near limit` notice, the warn mode for models, and one
  user setting to raise a class.
- **Windows commit versus working set.** A job counts committed memory, which some programs
  reserve and commit far beyond what they touch (some JITs, some GPU runtimes). A model or browser
  could be stopped while its working set is small. Stage 5 measures llama.cpp with and without
  `--no-mmap` and with CUDA, and the model cap adds the margin.
- **Linux without delegation.** The fallback is per-process rlimits plus a 250 ms poll: a fast
  tree can overshoot between polls. The watchdog backs it.
- **Nested-job semantics on Windows** (breakaway, CPU rate control in nested jobs). Stage 1
  tests each before anything depends on it.
- **Overhead.** A job per unit, a Toolhelp snapshot per spawn, an `exec` per spawn on Linux. All
  are estimated at a few milliseconds and measured in stage 8. Hooks run often, so their cost is
  reported separately.

## Open questions

The user's questions are answered ([settled decisions](#settled-decisions)). For the root:

1. **Roadmap number.** Item 67 is taken by the open Princeps plan (#325), so this is item 68. If
   #325 does not merge first, renumbering is a one-line change.
2. **Ordering (settled in revision 3).** `imperium-limits` (stage 1) lands first and #328
   rebases onto it. Stage 2 waits for #330, which waits for #304, and stage 7 for #331. CAS
   stage 5 creates its jobs through `imperium-limits`. If it lands before stage 1 after all,
   stage 4 moves its job code over.
