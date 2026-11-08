# 14 — Projects and automation

## Projects

A **project** is the unit users work in, and it *is* the environment: there is no separate
environment object.

A project defines:

- **Where it runs:** a local folder, a container image with mounted folders, or a path on an
  SSH host (see [isolation](05-tools-and-isolation.md#isolation-tiers)).
- **Its configuration layer** (`.imperium/config.toml`), instructions, policy overrides,
  plugins, MCP servers, routing rules, and budgets.
- **Its sessions:** any number of sessions, each a conversation with a root agent. Sessions
  inherit the project's environment and may narrow it (a subfolder, a read-only mount).
- **Optionally, a project agent** and **schedules** (below).

Projects are listed in the sidebar; opening a folder that has `.imperium/` (or any folder)
creates or reopens its project.

## Worktrees

For Git projects, sessions and parallel writing subagents can each work in their own Git
worktree, so concurrent edits never collide. Worktree behaviour is explicit configuration, set
globally and overridable per project:

```toml
[worktrees]
mode = "per-session"            # off | per-session | per-writer (each writing subagent gets one)
location = "data"               # data | sibling | in-repo | custom
custom_path = ""                # used when location = "custom"; placeholders: {project}, {repo}, {session}, {task}
branch_prefix = "imperium/"     # branches are <prefix><session-or-task-slug>
base = "default"                # default branch, current branch, or a named ref
cleanup = "on-archive"          # on-archive | on-merge | manual
keep_unmerged = true            # never delete a worktree with unmerged or uncommitted work
max_worktrees = 20              # per project
max_disk_gb = 50                # per project; new worktrees are refused above this
```

| `location` | Path                                                      |
| ---------- | --------------------------------------------------------- |
| `data`     | `<data>/worktrees/<project>/<name>` (default; outside the repo) |
| `sibling`  | `<repo>/../<repo-name>.worktrees/<name>`                  |
| `in-repo`  | `<repo>/.imperium/worktrees/<name>` (added to `.git/info/exclude`) |
| `custom`   | `custom_path` with placeholders expanded                  |

- A Worktrees pane lists every worktree with its branch, owning session or task, ahead/behind
  counts, dirty state, and disk use, with actions to open, sync with base, merge, or remove.
- Merging a writer's worktree back is an explicit step (by the parent agent under policy, or by
  the user); conflicts are reported, never force-resolved.
- On SSH hosts, worktrees are created on the host using the host's configuration.
- Non-Git projects can use copy-on-write snapshots (where the filesystem supports them) or run
  writers sequentially.

## Project agent

A long-lived agent per project that watches for work and **dispatches it into new sessions**.

**Implemented (phase 6):** `[[projects]]` entries with `agent = true` get an agent when the
daemon starts: an ordinary session (it appears in the session list and can be talked to) with
coordinator instructions and a `start_session` tool. It is woken by messages only: a schedule
with `action.agent`, `imperium project tell "<text>"` (`projects.message`), or a report from a
session it started, delivered as `Session <id> finished. Its report: ...` when that session's
turn ends. It uses no tokens between events. `max_sessions` (default 2) caps sessions running
at once and `max_sessions_per_day` (default 20) per day. Permission requests from the agent and
its sessions, and from scheduled prompt runs, become approval requests (the Approvals panel,
`imperium approvals`). `imperium daemon autostart enable|disable|status` starts `imperiumd` at
login (a per-user `Run` registry value on Windows, a LaunchAgent on macOS, an XDG autostart
entry on Linux). The sessions an agent starts are [background sessions](#quiet-periods): they
rank below yours, wait for a quiet machine before starting (`prefer_quiet`, default on), and
pause while the machine is busy. Not yet: file, Git, and webhook wake-ups, cost budgets, a tray
icon, and notes beyond the agent's memory tools.

- **Toggle per project** (off by default). When off, nothing runs in the background.
- **Runs only while the daemon runs**, i.e. while the computer is on and the user is logged in.
  An optional "start daemon at login" setting (and a tray icon) keeps it available without the
  desktop window open. Nothing runs while the machine is off or asleep.
- **Reactive, not polling a model:** the project agent is woken by events — schedule fires,
  file or Git changes, webhooks, completed sessions, messages from the user or the phone app.
  Between events it consumes no tokens.
- On each wake it decides what to do with the event and either acts directly (small things) or
  **creates a new session** with a brief, which runs under normal routing, budgets, and
  permissions. It receives the session's completion report, not its transcript.
- It keeps a short project memory (notes file and task backlog) so it can resume after restarts.
- Budgets: a daily token/cost cap and a max number of concurrent sessions it may start.
- Permissions are the project's, with one extra rule: actions that would normally prompt the
  user are queued as **approval requests** (shown in the desktop app and phone app) instead of
  blocking a background session indefinitely.

## Schedules

Schedules let tasks run automatically with the flexibility of provider-side scheduled tasks and
routines, but locally.

**Implemented (phase 6, `imperium-automation` and the daemon's scheduler):** `[[schedules]]` in
the user configuration with a `cron` (five fields, local time; names, lists, ranges, steps),
`every` (`90s`, `30m`, `2h`, `1d`), or `at` (a local time) trigger, and a `prompt` (a new
session in `action.project`, with an optional `model`) or `command` (a shell command run there)
action, with `timeout_s` (default one hour). The daemon runs them whether or not a window is
open. Missed runs follow `missed` (`run_once_on_wake`, the default, or `skip`), judged from the
run history in `<data>/schedules/history.json`; `overlap` is `skip` (default) or `allow`. Every
connected client receives `schedule.event` notifications (`started`, `completed`, `failed`,
with the session id and a summary), and `notify = ["os"]` also shows a system notification.
`imperium schedule list` and `imperium schedule run <id>` use the `schedules.list` and
`schedules.run` protocol methods. A prompt run whose session asks for permission waits for an
answer from a client until it times out; approval queues arrive with the project agent. Not
yet: time zones other than local, jitter, file, Git, webhook, and event triggers, the project
agent and hook actions, conditions other than `when_quiet`, project-level schedules, `schedule.*` hook events, and the
desktop Schedules pane.

**Implemented (stage 2g): quiet periods.** A schedule's `prefer_quiet = true` (default `false`)
holds a due run until the machine is quiet, for at most `max_defer` (default `2h`; `90m`, `1d`
also work), then runs it anyway so nothing is postponed forever. The condition
`conditions = { when_quiet = true }` waits for a quiet machine however long it takes. A waiting
run shows as `deferred` in `imperium schedule list` and the `schedules.list` row, clients get a
`schedule.event` with status `deferred`, and `imperium schedule run <id>` (`schedules.run`)
starts it at once; a person's run never waits. The other conditions listed under
[Behaviour](#behaviour) (AC power, GPU idle, time window, previous run succeeded) are not
implemented yet.

```toml
[[schedules]]
id = "nightly-audit"
trigger = { cron = "0 2 * * *" }
action = { project = "D:/Git Repositories/Imperium-AI", prompt = "Audit new dependencies." }
prefer_quiet = true
max_defer = "3h"
```

### Quiet periods

Background work should not slow the sessions you are driving. The daemon keeps a **load view**
that reads `quiet`, `busy`, or `saturated`, and the work it starts on its own gives way to you:

- **The load view** samples every `load.sample_s` (5 s): your sessions with a turn running, slots
  in use and calls queued on local models (yours and background calls counted apart), models
  swapped out in the last minute, overall CPU use, and available memory. *Saturated*: a call of
  yours is queued for a local slot, `load.saturated_swaps_per_min` models were swapped out in a
  minute, memory is below `load.saturated_free_ram_mb`, or the CPU is at
  `load.saturated_cpu_pct`. *Busy*: a session of yours has a turn running or a call in flight, or
  the CPU or memory crosses the `busy` thresholds. *Quiet*: none of that. The level rises at once
  and drops only after the machine has looked calmer for `load.settle_s` (60 s). CPU use is not
  counted while a background session is working, because the daemon cannot tell its own load from
  yours (follow-up: subtract the CPU of the daemon's own child processes instead, so a heavy
  build of yours is still noticed while background work runs). Your keyboard and mouse idle time is not used (it needs OS calls this codebase does not
  make); your sessions stand in for it, and GPU utilisation is not polled (`nvidia-smi` runs once,
  at start, for memory); the slot queue and swaps are the GPU signal.
- **Background sessions.** A session started by a schedule's prompt or by a project agent's
  `start_session` is marked `background` (kept with the session, so it survives a restart). In the
  local runtime its calls rank below yours and never swap out a model you used in the last
  `load.interactive_hold_min` minutes, and they wait while the view is `saturated`
  ([04](04-models-and-routing.md#runtimes)). Sessions from auto-fix and ones you start are not
  background.
- **Deferral.** A project agent's `start_session` creates the session at once, but with
  `prefer_quiet` (default `true` for `[[projects]]`) it gets its brief only when the view is
  `quiet`, or after `max_defer`. Until then it is listed as `paused` with a `deferred` hold.
  Sessions waiting still count against `max_sessions`. A session released by `max_defer` or by
  *run now* (also a schedule prompt run that waited out its `max_defer`, or one a person started
  with `schedules.run`) does not give way again for the rest of its life: "then run anyway"
  means it runs to the end.
- **Yielding mid-run.** A running background session checks the view before each model call. If
  it is `busy` or `saturated`, the call in flight has already finished, so it pauses there: no
  slot held, no tokens spent, like `await_tasks`. It resumes when the view is `quiet`. The pause
  reuses the usage-limit pause (a `paused` event with `cause = "load"` and `until_unix_ms = 0`,
  then `resumed`). Subagents of a background session yield the same way, and a paused agent is
  detached from its local model, so a model you ask for can swap it out without waiting for it.
- **Visibility and run now.** Session summaries carry `background` and a `hold`
  (`cause` `deferred` or `load`, and the reason); the desktop shows `Paused · waiting for a quiet
  machine` or `giving way to you`, a `· background` suffix, and the existing **Resume now**
  button; `sessions.control` with `resume_now` does the same. Running now ends the wait and stops
  that session giving way for the rest of its life. The CLI shows deferred schedules and prints a
  line when a session it watches waits. The desktop has no Projects pane or Schedules pane yet,
  so deferred scheduled runs are visible through `imperium schedule list` only.

### Triggers

| Trigger     | Example                                       |
| ----------- | --------------------------------------------- |
| Cron        | `0 8 * * MON-FRI` in a chosen time zone        |
| Interval    | every 30 minutes, with jitter                 |
| One-shot    | at 2026-11-08 15:00                           |
| File watch  | changes under `src/**` (debounced)            |
| Git         | new commits on a branch, new tag, PR comment (via polling or webhook) |
| Webhook     | `POST` to a local, token-protected URL        |
| Event       | any hook event, e.g. `session.end` in another project |

### Actions

| Action           | What runs                                                         |
| ---------------- | ----------------------------------------------------------------- |
| Prompt           | A new session with a stored prompt, model/routing overrides, and budget |
| Project agent    | Delivers the trigger as an event to the project agent             |
| Command          | A shell command inside the project's isolation tier               |
| Hook             | Fires the `schedule.fire` hook event with the schedule payload     |

### Behaviour

- **Missed runs** (computer off or asleep): per schedule, `skip`, `run_once_on_wake` (default),
  or `run_all` (capped).
- **Overlap**: `skip`, `queue`, or `allow` when the previous run is still going.
- **Conditions**: only on AC power, only when a given GPU is idle, only within a time window,
  only if the previous run succeeded, only when the machine is quiet (`when_quiet`, implemented;
  see [quiet periods](#quiet-periods)).
- **Outputs**: each run is a normal session (reviewable later), plus optional notifications to
  the desktop, the phone app, or a webhook.
- Defined in project config (shareable) or user config, editable in a Schedules pane and via
  `imperium schedule add|list|run|pause`.

```toml
[[schedules]]
id = "morning-triage"
trigger = { cron = "0 8 * * MON-FRI", tz = "Europe/London" }
action = { prompt = "Triage new GitHub issues and draft replies; do not post.", budget_usd = 0.25 }
missed = "run_once_on_wake"
notify = ["desktop", "phone"]
```

### Hooks integration

Schedules emit `schedule.fire`, `schedule.complete`, and `schedule.failed` hook events, and
hooks can create, pause, or trigger schedules through the CLI or protocol. A plugin can ship
schedules and hooks together (for example, a "nightly dependency audit" plugin).

## GitHub auto-fix

Projects linked to a GitHub repository can react to repository activity automatically: fix a
pull request whose checks fail, answer review comments, triage new issues, and investigate
failed workflow runs.

**Implemented (phase 6, `imperium-github` and the daemon's auto-fix):** with
`github.autofix.enabled`, the daemon polls each `[[github.watch]]` repository (every `poll_s`,
default 120 s) through the `gh` CLI for the signed-in user's open pull requests whose latest
commit has a failed check. For each new failing commit it runs **Fix CI**: it prepares a
worktree of the pull request's branch from the watch's local clone
(`<data>/worktrees/<owner>__<repo>/pr-<n>`, on a local `imperium/fix-pr-<n>` branch), gives a
new session the failed checks and the end of the failed job's log (`gh run view --job
--log-failed`), with file writes, edits, and commands pre-approved inside that worktree,
commits whatever it changed with an `Imperium-Auto-Fix: session <id>` trailer, and, with
`push = "ask"` (the default), queues an approval request; only on approval does it push to the
pull request's branch, as a plain push. It never force-pushes, merges, closes, or approves.
After `max_attempts_per_pr` (default 3) failing commits on one pull request it stops and
says so. Progress goes to every client as `autofix.event` notifications (`started`,
`awaiting_approval`, `pushed`, `declined`, `nothing`, `failed`, `stopped`); approvals are
answered in the desktop app's Approvals panel or with `imperium approvals list|approve|deny`
(`approvals.list` and `approvals.respond`, with `approval.requested` notifications). Not yet:
review, conflict, triage, and workflow-failure responders, `github.*` hook events, webhooks,
labelled or all pull requests, budgets, `comment`, and persisting attempt counts across
restarts.

### Events

Delivered by polling the GitHub API (default, works behind NAT) or by a webhook through the
optional relay ([15](15-mobile-and-remote.md#connectivity)). Each becomes a hook event, so
plugins and user hooks can react too.

| Hook event              | Raised when                                                         |
| ----------------------- | ------------------------------------------------------------------- |
| `github.check.failed`   | A check run or workflow fails on a watched pull request or branch   |
| `github.pr.review`      | A review or review comment arrives on a watched pull request        |
| `github.pr.conflict`    | A watched pull request can no longer merge cleanly                  |
| `github.issue.opened`   | An issue is opened (optionally filtered by label)                   |
| `github.issue.comment`  | Someone comments on a watched issue or mentions the bot handle      |
| `github.workflow.failed` | A workflow run fails on a watched branch (for example `main` or a schedule) |

### Built-in responders

| Responder        | Does                                                                                   |
| ---------------- | -------------------------------------------------------------------------------------- |
| **Fix CI**       | Reads the failed job's logs, reproduces locally where possible, commits a fix to the PR branch in its own worktree, and pushes |
| **Address review** | Applies requested changes, replies to each comment with what changed, and resolves threads it addressed |
| **Resolve conflicts** | Merges the base branch in a worktree, resolves conflicts, runs checks, and pushes |
| **Triage issue** | Labels, asks for missing details, links duplicates, and optionally opens a draft PR with a fix |
| **Workflow failure** | Opens or updates an issue with the failure's cause, or starts Fix CI when it maps to an open PR |

Each responder runs as a normal session (reviewable later, journaled, routed and budgeted like
any other) started by the project agent or directly by the event.

### Configuration and guardrails

```toml
[[github.watch]]
repo = "Densetsu152637/Imperium-AI"
branches = ["main"]                 # workflow failures on these branches
pull_requests = "mine"              # mine | labelled | all
issues = { labels = ["auto-triage"] }

[github.autofix]
enabled = true
responders = ["fix_ci", "address_review", "resolve_conflicts"]
max_attempts_per_pr = 3             # stop and notify after this many failed fixes
budget_usd_per_day = 2.0
push = "ask"                        # ask | auto: ask queues an approval before each push
comment = "auto"                    # replies and issue comments
```

- **Never** force-pushes, merges, closes, or approves; those stay with people. Pushes go to the
  pull request's own branch only, never to protected branches.
- Every push and comment is attributed (commit trailer and a comment footer) and recorded in the
  session journal.
- With `push = "ask"`, the fix waits as an approval request in the desktop and phone apps.
- Repeated failures with the same cause stop after `max_attempts_per_pr` and notify instead of
  looping.
- Authentication uses the `gh` CLI's login or a GitHub App / fine-grained token stored in the
  keychain, scoped to the watched repositories.
