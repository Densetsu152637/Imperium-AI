# Plan: resource caps on every child process

Revision: 2026-11-26, revision 1 (first draft, for the root's and the user's review); written
against `experimental` at `cd791003`. Roadmap [item 68](../12-roadmap.md#next-in-order).
Status: planned (no stage started).

A CAS test process grew to about 20 GB on the user's 64 GB Windows machine and took the machine
to about 100% RAM. Nothing stopped it: on Windows no process Imperium starts has a memory limit
today, and on Linux and macOS only two workers do. This plan treats unbounded resource use as a
security issue. An agent's command, an MCP server, a hook, a plugin or a runaway test should not
be able to take the host down, whatever text steered it there.

Every process Imperium starts gets a **cap unit**. A unit is one process tree with a memory cap,
a process-count cap and an optional CPU cap, enforced by the OS where the OS can, and by a
daemon-side poll where it cannot. Units are grouped per session under a session aggregate, and
all of them under an Imperium-wide total. A daemon watchdog watches the machine's free memory and
stops Imperium's own processes before the machine runs out. When something is stopped, the agent
and the interface are told which cap, what value, and which setting raises it. Only the user can
raise a cap.

Each stage below is one pull request against `experimental`. Every merge into `experimental`
publishes a build, so caps that could stop legitimate work start in warn-only mode where the
plan says so (see [rollout](#rollout)).

## Goals

Each goal is boolean, with a check that runs in CI on the three OSes and needs no manual step.

1. **Every spawn site is covered.** Every place in the workspace that starts a process goes
   through `imperium-limits` (one class per site), or appears on a short allow-list of
   daemon-internal one-shot probes with a reason. Check: a source test (in the style of the
   existing source checks) fails on any `Command::new`, `tokio::process::Command::new`,
   `portable_pty` spawn or `CreateProcess*` outside `imperium-limits` that is not on the list.
2. **An allocation bomb is stopped at its cap.** A probe that allocates and touches memory in
   64 MiB steps, under a 256 MiB unit, is killed, and the reported peak is at most the cap. On
   macOS the peak may exceed the cap by the measured poll overshoot, which the test bounds.
3. **A fork bomb is stopped at its cap.** A probe that starts copies of itself in a loop, under a
   32-process unit, is stopped. No probe process is left afterwards, and the notice names the
   process cap.
4. **A CPU spin is held to its cap.** On Linux and Windows a four-thread spin under a 25% cap
   uses at most 35% of the machine over 3 s. On macOS the cap is reported as "not enforced" (see
   [L4](#l4-macos-a-daemon-side-poll-no-jetsam)) and the test checks that report.
5. **The session aggregate holds.** Two units, each under its own cap, whose sum passes the
   session cap: the larger one is stopped with a `session memory` notice.
6. **The watchdog acts before the machine runs out.** With an injected free-memory reader that
   reports less than the reserve, the watchdog stops the largest eligible unit within two
   samples. It never touches a model with a call in flight while non-model units remain, and
   never touches the daemon itself.
7. **Caps cannot be raised from outside the user layer.** A project or environment
   `.imperium/config.toml`, a plugin manifest, an `.mcp.json` and an agent's `imperium config set`
   each fail to raise a cap: the project file is refused as widening, the manifest field is
   ignored with a warning, and the command hits the safety floor. A user-layer value raises it.
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
  cgroup in the delegated tree, or open the daemon with `PROCESS_DUP_HANDLE` and change its job.
  The caps still stop every accident and every process that is not written to evade them, which
  covers the incident. The plan does not claim more.

Not in scope: GPU memory (the residency controller owns VRAM), disk space and file descriptors
(a later item could add them), network bandwidth, and processes the user starts outside Imperium.

## Decisions

### L1. One crate owns every OS mechanism: `imperium-limits`

**Accepted: a new leaf crate, `crates/imperium-limits`, holds the cap types and the OS code for
the three platforms. Every spawn site builds a `UnitSpec` and spawns through it.**

```rust
pub enum Class { Command, Terminal, Mcp, Hook, Cas, Script, Browser, Internal, Model }

pub struct Caps { memory_bytes: Option<u64>, processes: Option<u32>, cpu_percent: Option<u16> }

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
- #328's `cgroup.rs`, `watchdog.rs` and the job part of `windows/launch.rs` move into it. The
  code-mode worker keeps its token, labels, pipes and handle list in `imperium-script`, and asks
  `imperium-limits` for its job (one job, not two: see [L8](#l8-reuse-not-double-limits)).
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
  `models` job directly under the daemon job. Nested jobs need
  Windows 8 or later; Windows 10 and 11 have them.
- **Limits per unit job.** `JOB_OBJECT_LIMIT_JOB_MEMORY` (the committed private memory of the
  whole tree), `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`, `KILL_ON_JOB_CLOSE`,
  `DIE_ON_UNHANDLED_EXCEPTION`, **no** `BREAKAWAY_OK` and no `SILENT_BREAKAWAY_OK`. A CPU cap is
  `JobObjectCpuRateControlInformation` with `JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP`. The session
  and total jobs carry only the job memory limit and the active-process limit.
- **Assignment with no window.** The command gets `CREATE_SUSPENDED`. After `spawn`, the process
  handle (held, so its id cannot be reused) is assigned to the session job and then to the unit
  job, which nests the unit job under the session job. The process's only thread is then found
  with a Toolhelp thread snapshot filtered by process id, opened, and resumed. This keeps
  `tokio::process::Command`, its pipes and `kill_on_drop` at every site. The process starts
  inside its job, and so does everything it starts.
- **Notices.** One I/O completion port per daemon is associated with every job.
  `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` and `JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT` mark the unit as
  limited. A memory limit on Windows makes allocations fail rather than killing anything, so on
  that message the daemon terminates the unit's job (`TerminateJobObject`) and reports
  `limited: memory`. This gives one behaviour on every OS: a unit that reaches its memory cap is
  stopped whole. An 80% warning uses `JobObjectNotificationLimitInformation` (a notification
  limit, not enforced) and gives one `near limit` notice per unit.
- **The terminal.** ConPTY's spawn in `portable-pty` cannot take `CREATE_SUSPENDED`, so the
  terminal keeps its assign-after-spawn and tree walk (terminal plan D5) through
  `Limits::adopt`.
- **The daemon job's breakaway.** `BREAKAWAY_OK` stays on the daemon job, because opening a
  folder in Explorer must outlive the daemon. Units cannot use it: their own jobs lack it. Stage 1
  tests that a capped probe's `CREATE_BREAKAWAY_FROM_JOB` child either fails to start or stays in
  the unit job (F2). If breakaway were to succeed through a nested hierarchy, the daemon job
  loses `BREAKAWAY_OK` and the opener uses a broker process outside it instead.

Rejected: `PROC_THREAD_ATTRIBUTE_JOB_LIST` through our own `CreateProcessW`. It assigns at
creation too, but needs our own pipe and environment plumbing at every site. The CAS broker and
the code-mode worker already need a custom launch, and they use `JOB_LIST` or suspend there.
Ordinary sites do not need it.

### L3. Linux: a delegated cgroup v2 tree, RLIMIT and a poll where there is none

**Accepted: the daemon runs in a delegated cgroup v2 subtree and gives every unit a child
cgroup. Where no subtree is delegated, the daemon falls back to per-process rlimits plus the
daemon-side poll of L4.**

- **Getting a delegated subtree, in order.**
  1. The daemon is already in one: #328's check (cgroup v2, the cgroup owned by the daemon's
     user, the needed controllers in `cgroup.controllers`, not running as root). The package's
     systemd user unit (if the user installs one) sets `Delegate=yes`.
  2. Otherwise, at start, before its runtime or logging start, the daemon re-executes itself
     once with `systemd-run --user --scope --collect -p Delegate=yes
     --unit=imperium-core-<random>.scope -- <self> <args>`, marked with
     `IMPERIUM_CGROUP_REEXEC=1` so it never loops. `systemd-run` is found by absolute path
     (`/usr/bin`, then `/bin`). This is about 30 to 60 ms once per daemon start (estimate,
     measured in stage 2).
  3. `IMPERIUM_CGROUP_ROOT=<path>` names a subtree the administrator delegated (for CI, servers
     and containers: the server package, item 63).
  4. None of these: fallback mode, reported by `imperium limits status` and in Settings.
- **Layout.** `<scope>/imperium-daemon` (the daemon, a leaf, as in #328), `<scope>/models/` (model
  runtimes, outside the total), `<scope>/total/` with `internal/` and `s-<session>/`, and units
  below them (`cmd-p3`, `mcp-<name>`,
  `hook-<n>`, `term-<n>`, `script-<pid>`, `cas`, `browser`). The controllers enabled are
  `memory`, `pids` and `cpu` (where delegated).
- **Per unit.** `memory.max`; `memory.swap.max = 0`, so a capped unit is stopped rather than
  pushed to swap (models are exempt, see [L6](#l6-default-caps-per-class)); `memory.oom.group =
  1`, so the kernel kills the whole unit, not one process of it; `pids.max`; `cpu.max` for a CPU
  cap. Session and total cgroups carry `memory.max` and `pids.max`.
- **Joining with no window.** A hidden launcher, `imperium-core limit --cgroup <path> [--rlimit
  ...] [--oom-adj N] -- <program> <args>`, writes its own process id to the unit's
  `cgroup.procs`, sets what it was asked to, and then `exec`s the program. `exec` replaces it, so
  no extra process stays, and nothing of the program runs outside the unit. It needs no `unsafe`
  (a file write and `CommandExt::exec`), unlike a `pre_exec` hook in the daemon, which the
  workspace forbids. Cost: one extra `exec`, about 1 to 3 ms (estimate). For the OS tier the
  launcher comes first: `limit → bwrap → sh -c`.
- **Notices.** `memory.events` (`oom_kill`, `max`) and `pids.events` (`max`) are watched with
  inotify, which cgroup files support. A unit whose `oom_kill` count rose reports
  `limited: memory`, and `pids.events max` reports `limited: processes`. Stop is `cgroup.kill`
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

Rejected:

- **`systemd-run --user --scope` per spawn.** It needs a D-Bus round trip (tens of milliseconds)
  on every command, hook and MCP start, and a fallback where no user manager runs anyway.
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
  `SIGSTOP` the group past the cap, then `SIGKILL` it. Overshoot is bounded by the allocation
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
  cap when it starts (admission: a container whose cap does not fit in what the session has left
  is refused, with the reason), and the watchdog stops it with `docker rm -f` as today.
- **vLLM in Docker.** `docker_run_args` adds the same flags with the model class's cap.
- **vLLM in WSL2.** A job on `wsl.exe` holds nothing, because vLLM runs in the WSL VM. The VM
  itself is bounded by `.wslconfig` `memory=`, which defaults to half the host's RAM. Imperium
  reads the effective value and shows it, and never writes `.wslconfig` (a user file that
  affects every distribution). Inside the distribution, when it runs systemd, the serve command
  is wrapped in `systemd-run --user --scope -p MemoryMax=<cap>`. Otherwise only the VM's bound
  applies, and the model's status says so.

### L6. Default caps per class

Caps are per **unit** (a whole process tree), not per process. "RAM" is the machine's physical
memory as `hardware` reads it (with `[hardware]` overrides). The 64 GB column is the user's
machine.

| Class | What it covers | Memory | 64 GB machine | Processes | CPU |
| --- | --- | --- | --- | --- | --- |
| `command` | `run_command` (foreground and background), scheduled and auto-fix commands, environment runtimes | 25% of RAM, between 2 and 16 GiB | 16 GiB | 1,024 | none; below-normal priority |
| `terminal` | the user's terminal panes | 25% of RAM, between 2 and 16 GiB | 16 GiB | 2,048 | none; normal priority |
| `mcp` | stdio MCP servers, plugin MCP servers | 2 GiB | 2 GiB | 256 | none; below-normal |
| `hook` | command hooks (and the existing 300 s clamp) | 512 MiB | 512 MiB | 64 | none; below-normal |
| `cas` | the CAS server tree (outer unit only) | D4's formula: twice D3's per-call memory + 512 MiB | 2.5 GiB | D4's measured limit (3 on Windows), 8 on Unix | D3's per-call CPU time stays |
| `script` | the code-mode worker | twice `script_memory_mb` (C5) | 512 MiB | 1 | C5's budgets stay |
| `browser` | the CDP browser | 4 GiB | 4 GiB | 512 | none |
| `internal` | git, `gh`, installs, probes, the updater | 2 GiB | 2 GiB | 128 | none |
| `model` | llama.cpp, whisper, vLLM, the planned `ncm-server`, the media sidecar | 1.5 × the planner's RAM estimate + 2 GiB, at most RAM − the reserve | per model | 128 | none |
| session aggregate | every unit of one session (not models) | 50% of RAM, at most 48 GiB | 32 GiB | 4,096 | none |
| total | every unit except models | 75% of RAM − the reserve | about 43 GiB | 8,192 | none |
| watchdog reserve | free memory the watchdog keeps | 8% of RAM, at least 2 GiB | 5.1 GiB | n/a | n/a |

Reasons:

- **Commands are sized for real builds.** A Rust build of this workspace or a webpack build can
  take several gigabytes legitimately, so commands get a quarter of RAM. The incident's process
  (20 GB) is stopped at 16 GiB on the user's machine. A user who wants a tighter guard sets it
  lower.
- **Small classes are small.** MCP servers, hooks, CAS and the script worker have no
  legitimate reason to use gigabytes. A Playwright-style MCP server that starts a browser is the
  largest real case seen, and fits 2 GiB.
- **Models are large and legitimate.** Their cap is derived from the residency planner's own
  estimate (`Footprint::required_bytes`), so it is a leak guard, not a tight bound. It is
  `warn` before `enforce` (see [rollout](#rollout)). Memory-mapped weights count differently per
  OS: they are file-backed, not committed, so a Windows job does not count them, while a Linux
  cgroup charges their page cache. `memory.max` above the estimate makes the kernel reclaim
  page cache before it kills anything. Model units keep swap (no `memory.swap.max`).
- **CPU is not capped by default.** A spin degrades the machine but does not take it down the
  way memory does. Lower priority for agent classes keeps the interface responsive, and a CPU
  cap is one setting away. This is an open question for the user.
- **The terminal is the user's**, so it gets normal priority and the same memory default as
  commands. It is still capped, because a script the user runs there can be as runaway as an
  agent's.

### L7. Settings: user layer only, narrowing elsewhere

**Accepted: a `[limits]` section in the user config. Only the user layers (the user file, the
app layer that Settings writes, and `IMPERIUM_LIMITS__*` variables set for the daemon) can raise
a cap. Every other source can only lower one.**

```toml
[limits]
mode = "enforce"                 # "enforce", "warn" (notices only) or "off" (no caps; the watchdog stays)
commands.memory_mb = "auto"      # "auto" (the L6 default) or MiB
commands.processes = "auto"
commands.cpu_percent = 0         # 0 = no cap; else percent of the whole machine
terminal.memory_mb = "auto"      # likewise for terminal, mcp, hooks, browser, internal
models.mode = "warn"             # models: "enforce", "warn" or "off", separate from `mode`
models.margin_mb = 2048
session.memory_mb = "auto"
session.processes = "auto"
total.memory_mb = "auto"
watchdog.enabled = true
watchdog.reserve_mb = "auto"
```

- **Per server.** A stdio server in the user config may carry `limits = { memory_mb = ...,
  processes = ... }` (`mcp.servers.<name>.limits`). A server defined anywhere else (a plugin's
  `.mcp.json`, a trusted project's servers) takes the `mcp` class cap, and a `limits` field there
  is ignored with a warning in `mcp.list`.
- **The environment and project layers narrow.** `limits.*` joins the environment layer's
  allow-list as narrow-only ([10](../10-configuration.md#layers)): a value at or below the user's
  is applied, a higher one is listed as refused by `imperium env config` and `config explain`.
  `limits.mode` may only move towards `enforce`.
- **Agents cannot raise them.** No tool takes a cap. `run_command` gets no new parameter.
  `config.toml` and `models.toml` are refused to the file tools (pull request #304). `imperium
  config set|unset limits.*` from an agent's command hits the safety floor (asked every time,
  never sticky, in every mode), the rule pull request #330 adds for `runtime.*` keys. The OS tier
  already hides the daemon's endpoint and socket, so a confined command cannot call `config.set`.
- **Floors.** Values below a minimum (64 MiB, 8 processes, 5% CPU) are refused when saved, so a
  typo cannot make every command fail. `mode = "off"` is allowed (the user's machine), but
  Settings shows it as a warning, and the watchdog has its own switch.
- **Changes apply to new units.** A changed cap applies to processes started afterwards.
  Running units keep their caps (`restart: false`, no daemon restart needed), except that
  lowering the total or the reserve applies to the watchdog at once.

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
- **Terminals.** The terminal plan's job and Linux supervisor stay. The job gets the
  `terminal` limits, and the supervisor uses `cgroup.kill` on its unit where one is delegated
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
  action, setting }`. `limit` is one of `memory`, `processes`, `cpu`, `session_memory`,
  `session_processes`, `total_memory` or `system_pressure`. `action` is `stopped`,
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

**Accepted: a dedicated OS thread in the daemon samples the machine's free memory and stops
Imperium's own units, largest eligible first, before the machine runs out.**

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
  1. stop the largest unit of `command` (background before foreground), `mcp`, `hook`,
     `browser`, `cas`, `script` or `internal` that uses more than 256 MiB;
  2. ask the residency controller to unload idle models (`pressure`);
  3. stop the largest `terminal` unit;
  4. only below half the reserve for 5 s: stop a model with a call in flight (`pressure`).
  It never stops the daemon, the desktop app or any process Imperium did not start, even when
  the pressure comes from elsewhere. Each act is a `resource.limited` with `system_pressure`,
  naming the free memory and the reserve.
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
on Windows, a cgroup or rlimits and a poll elsewhere). Stage 2 wraps `scripts/check-python.sh`'s
pytest run in it on every OS (4 GiB by default, `IMPERIUM_TEST_MEMORY_MB` to change). Stage 2
also removes the CAS tests' `unlimited_on_windows` opt-in for in-process corpus and fuzz tests,
which then run inside that unit. A development agent that runs the scripts gets the cap without
having to know about it.

## Scope and non-goals

In scope: every process Imperium starts on the three OSes, per-unit and aggregate caps, the
watchdog, settings, notices, CI tests, and the docs (05, 04, 10, 11, 13).

Not in scope: GPU memory, disk and descriptor caps, network bandwidth, remote and SSH hosts
(their own daemon applies its own caps), and processes the user starts outside Imperium (except
the repository's scripts, L12).

## Assumptions

- A1. Windows 8 or later (nested jobs); the supported Windows 10 and 11 qualify.
- A2. Linux desktops with systemd 244 or later delegate `memory` and `pids` to the user manager,
  and `systemd-run --user` works in a graphical session. Stage 2 records what CI and common
  distributions show. Without it, the RLIMIT and poll fallback holds.
- A3. A suspended process created by `tokio::process::Command` has exactly one thread until it
  is resumed, so resuming the one thread a Toolhelp snapshot finds for its process id starts it.
  Stage 1 tests this.
- A4. `memory.oom.group` and `cgroup.kill` are present on the kernels in CI. Where `cgroup.kill`
  is missing (before 5.14), the freeze-and-kill walk stands in.
- A5. GitHub's runners allow `sudo` to create and `chown` a cgroup subtree (for the delegated-tree
  job), and `loginctl enable-linger` to start a user manager (for the `systemd-run` path).

## Relevant paths

- New: `crates/imperium-limits/**`, `crates/imperium-core/src/limits.rs` (the registry,
  sessions, the watchdog), `crates/imperium-core/src/main.rs` (the hidden `limit` launcher, the
  cgroup re-exec), `crates/imperium-protocol/src/limits.rs`.
- Changed: `crates/imperium-tools/src/{exec.rs,processes.rs,sandbox.rs,runtime.rs}`,
  `crates/imperium-core/src/scheduler.rs`, `crates/imperium-mcp/src/servers.rs`,
  `crates/imperium-extensions/src/hooks.rs`, `crates/imperium-local/src/{process.rs,speech.rs,vllm_env.rs,wsl.rs}`,
  `crates/imperium-web/src/browser.rs`, `crates/imperium-script/src/{cgroup.rs,watchdog.rs,windows/launch.rs}`
  (moved), `crates/imperium-pty/src/lib.rs`, `crates/imperium-config/src/{settings.rs,registry.rs}`,
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
  binary (`alloc <step-MiB> <ceiling-MiB>`, `fork <ceiling>`, `spin <threads> <secs>`,
  `breakaway`). #328's three modules move in, and `imperium-script` uses them with no change in
  values.
- Depends on: #328 merged.
- Acceptance checks, `cargo test -p imperium-limits` on the matrix:
  - alloc bomb, fork bomb and CPU spin (goals 2 to 4) per OS and mechanism;
  - Windows: the probe's `CREATE_BREAKAWAY_FROM_JOB` child stays in the unit or fails to start
    (F2); a unit job nests under a session job and the session's memory limit stops the pair; A3;
  - Linux, in a job that creates a delegated subtree with `sudo`: `memory.oom.group` kills the
    whole tree; `cgroup.kill` stops it; the fallback (no subtree) stops the alloc bomb through
    `RLIMIT_DATA` and the tree poll;
  - macOS: the measured overshoot at 100 ms, recorded in the outcome;
  - `imperium-script`'s tests pass unchanged.

### Stage 2: the daemon's cgroup, the settings and a capped launcher for scripts

- Objective: the cgroup bootstrap (L3's order, the `systemd-run` re-exec), the `[limits]`
  settings and their registry rows (L7) with narrowing and the floor rule for `config set
  limits.*`, `imperium limits status`, the hidden `limit` launcher, and L12 (check scripts and
  the CAS tests).
- Depends on: stage 1. The floor rule follows #330's pattern, so it comes after #330.
- Acceptance checks: goal 7; `imperium limits status` reports `cgroup`, `rlimit + poll`, `job`
  or `poll` correctly in each CI job (including a `loginctl enable-linger` job for the
  `systemd-run` path); `python-cas` on Windows runs inside a 4 GiB unit.

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
- Acceptance checks: a fake model server past its cap gives `warned` in warn mode and is stopped
  in enforce mode, reported as `crashed` with the detail; the WSL path reports the VM bound.

### Stage 6: the watchdog, the total cap and the interface

- Objective: L11, the total job or cgroup, Settings → Limits, the resource monitor's per-unit
  rows, and the toasts.
- Depends on: stages 3 and 5.
- Acceptance checks: goal 6 with an injected reader; the order of victims; the watchdog's act
  with `limits.mode = "warn"`; the desktop shows the event (web tests).

### Stage 7: terminals

- Objective: terminal panes through `Limits::adopt` with the `terminal` class, and `cgroup.kill`
  in the Linux supervisor where delegated (L8).
- Depends on: stage 3 and the terminal plan's stage 2a (#331).
- Acceptance checks: the terminal plan's fork-loop test also passes under a 64-process cap; an
  allocation in a terminal past its cap is stopped and shown in the pane.

### Stage 8: review, measurement and enforce for models

- Objective: a security review pass over every spawn site and the tamper tests; measured spawn
  overhead per OS; models move to `enforce` if the measurements and a week of `warn` notices on
  `experimental` show no false stops; docs 05, 04, 10, 11 and 13 finished.
- Depends on: stages 1 to 7.

## Rollout

- Stages 1 and 2 add no caps to any agent-facing process (only the scripts of L12).
- Stages 3, 4 and 7 ship in `enforce` for their classes with the L6 defaults, which are sized so
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

For the user:

1. **Default sizes.** Commands and terminals at 25% of RAM (16 GiB on the 64 GB machine), MCP
   servers at 2 GiB, hooks at 512 MiB, the browser at 4 GiB, a session at 50% (32 GiB), and the
   watchdog's reserve at 8% (5.1 GiB). Tighter (commands at 8 GiB) or looser?
2. **CPU caps by default.** None by default, with lower priority for agent classes (the plan's
   choice), or a default cap, for example 75% of the machine per session?
3. **The terminal.** Cap the user's own terminal like commands (the plan's choice), or leave it
   to the session aggregate and the watchdog only?
4. **Containers in the session aggregate.** Count a container at its cap on admission (the
   plan's choice), or leave containers out of the aggregate?
5. **The watchdog and pressure from elsewhere.** When the machine is short because of a
   non-Imperium program, should the watchdog still stop Imperium's units (the plan's choice: yes,
   largest first, never a model with a call in flight unless critical)?
6. **`limits.mode = "off"`.** Allow the user to turn caps off entirely (the plan's choice), or
   keep a hard minimum for the small classes?
7. **The repository's scripts (L12).** Is 4 GiB right for the pytest run, and should
   `scripts/check-rust.sh` get a unit too (rustc can legitimately use more)?

For the root:

1. **Roadmap number.** Item 67 is taken by the open Princeps plan (#325), so this is item 68. If
   #325 does not merge first, renumbering is a one-line change.
2. **Ordering with #328 and CAS stage 5.** Stage 1 moves #328's modules, so it waits for #328.
   CAS stage 5 should create its jobs through `imperium-limits` if stage 1 lands first; if CAS
   stage 5 lands first, stage 4 moves its job code over.
