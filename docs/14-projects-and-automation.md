# 14 — Projects and automation

## Environments (implemented)

An **environment** is the unit users work in: a name, an optional icon and colour, and one or
more folders. The folders are the sandbox the agent works in: its file tools accept any path
inside any of them, and `run_command` runs in the first folder (the working directory) under the
session's isolation tier, which can write to every folder
([isolation](05-tools-and-isolation.md#isolation-tiers)). A session belongs to one environment
and records it (`environment_id` and `folders` in `session.list`); folders added later apply to
sessions started from then on.

The daemon keeps environments in `environments.json` in its data folder and serves them over
RPC:

| Method | Params | Result |
| --- | --- | --- |
| `environments.list` | `{}` | `{ environments: EnvironmentInfo[], git: string[] }` (`git`: ids of local environments with a folder inside a git repository, which can start sessions on worktrees; also sent with `environments.changed`) |
| `environments.create` | `{ name, icon?, color?, folders, runtime?, persistent_agent?, pet? }` | `EnvironmentInfo` |
| `environments.update` | `{ id, name?, icon?, color?, pinned?, runtime?, persistent_agent?, pet? }` (empty icon/colour/pet clears it) | `EnvironmentInfo` |
| `environments.test_runtime` | `{ id }` | `{ ok, message }` |
| `environments.delete` | `{ id }` | `{}` (folders and running sessions are left alone) |
| `environments.add_folder` | `{ id, folder }` (an existing absolute folder) | `EnvironmentInfo` |
| `environments.remove_folder` | `{ id, folder }` | `EnvironmentInfo` |

`EnvironmentInfo` is `{ id, name, icon, color, folders, pinned, runtime, persistent_agent,
agent_session_id, pet }`, where `pet` is the environment's default pet (an installed pet's id) for the
[nursery](16-pets.md#imperium-nursery), or none to let the nursery decide; folders first-to-last with the working directory first; pinned environments are listed first by clients. Every change is announced to all clients as `environments.changed`
(the full list). `session.create` takes `environment_id`; `project_dir` still works and maps to
the environment whose first folder it is, created (named after the folder) when there is none.
Sessions from before environments are moved the same way when the daemon resumes them, and the
desktop app turns each folder it had opened into an environment the first time it connects.

From the command line: `imperium env list`, `imperium env create <name> [folder...]`,
`imperium env add-folder <env> <folder>`, `imperium env remove-folder <env> <folder>`,
`imperium env rename <env> <name>`, `imperium env delete <env>` (an environment by name or id),
`imperium env set-runtime <env> local|docker|wsl|ssh [--image I] [--container C] [--distro D]
[--host H]`, `imperium env test <env>`, `imperium env agent <env> on|off`, and
`imperium chat --env <name|id> [--worktree [BRANCH]]`.

### Runtimes (implemented)

An environment's `runtime` says where its sessions' file and command tools run. It is an
object tagged by `kind`:

| `kind` | Fields | Tools run | Folders are |
| --- | --- | --- | --- |
| `local` (default) | none | on this machine, under `isolation.tier` | local folders |
| `docker` | `image` | each command in a fresh container from `image` (the container tier, network on) | local folders, mounted at `/workspace`, `/folders/2`, … |
| `docker` | `image`, `container` | `docker exec` into that running container | paths inside the container |
| `wsl` | `distro` | `wsl.exe -d <distro> --cd <folder> --exec sh -c …` (Windows only) | Linux paths; Windows paths are translated to `/mnt/<drive>/…` |
| `ssh` | `host` | the system `ssh` client (`BatchMode`, so keys and agents from your SSH config; never passwords) | paths on the host |

For a remote runtime (a container, WSL, or SSH) the folders are not checked on this machine; a
missing one fails the tool that uses it, and `environments.test_runtime` (the **Test** button,
`imperium env test`) checks that the runtime can be reached: Docker's engine runs and has the
image (or the container is running), the WSL distribution is installed, or SSH connects without
a prompt. Details of how tools run there are in
[isolation](05-tools-and-isolation.md#environment-runtimes-implemented). The approval layer is
unchanged. Changing an environment's runtime keeps its folders (translating Windows paths for
WSL) and applies to sessions started afterwards. Project files on a remote runtime
(instructions, skills, project memory) are not read yet.

### Persistent agent (implemented)

An environment can keep a **persistent agent**: one session that is always available, listed
first among that environment's sessions with a **Persistent** badge (`persistent: true` in
`session.list`). It is off by default; turn it on with the switch in the environment's detail,
`imperium env agent <env> on`, or `environments.update { persistent_agent: true }`. It starts
at once (or as soon as the environment has a folder) and with every daemon start, and it resumes
the same conversation each time, however long ago it was used: its session id is kept as the
environment's `agent_session_id`, outside the `agents.resume_days` window that applies to
ordinary sessions. Like a project agent (below) it has `start_session`, which starts sessions in
the same environment (at most 2 at once and 20 a day) and reports back to it. When a turn starts
with the conversation over the model's context budget, older turns are summarised first (the
same compaction as `/compact`), so it can run for days. Turning it off stops the session and
archives it, so its history is listed with the other archived sessions; the environment keeps its
id, so turning it on again takes it out of the archive and continues the conversation (unless it
was unarchived by hand in the meantime, when a new one starts). Deleting the environment stops it
too. It has no pet in the nursery.

Sessions that a schedule, a project agent, or auto-fix start still work in a single folder and
carry no environment.

### Planned

An environment will also define:

- **Its configuration layer** (`.imperium/config.toml`), instructions, policy overrides,
  plugins, MCP servers, routing rules, and budgets. Today these come from the working directory
  (the first folder).
- **Narrowed sessions:** a session limited to a subfolder or a read-only mount.
- **Schedules** (below), which are still configured per folder.

## Worktrees

**Implemented: sessions on a worktree.** `session.create { environment_id, worktree: true,
worktree_branch? }` (the **Start in a worktree** switch in the desktop's
[new-session composer](07-desktop-ui.md#environments-and-sessions-implemented), `imperium chat
--worktree [BRANCH]`) starts a session on git worktrees: every folder of the environment inside
a git repository is replaced by a new worktree of that repository on a new branch,
`imperium/<first 8 characters of the session id>` unless a branch is named, made from the
repository's current `HEAD`. Folders outside git are used as they are; a subfolder of a
repository becomes the same subfolder of its worktree. Worktrees go in
`<sessions.worktree_dir>/<repo name>/<branch>` when that setting is set (Settings → Sessions),
and otherwise inside the repository in `.worktrees/<branch>` (slashes in the branch become
`-`), which is added to the repository's `.git/info/exclude` so it is never committed. The
session records its worktrees (`worktrees: [{ folder, path, branch, repo }]` in
`session.list` and `session.create`, a **Worktree** badge in the app), keeps them when it
resumes after a restart, and `session.delete { session_id, remove_worktrees }` (`imperium
session delete <id> --remove-worktrees`) stops and forgets the session and removes its
worktrees, keeping their branches. Worktrees are made only for local folders (not remote
runtimes).

**Planned:** parallel writing subagents can each work in their own Git worktree, so concurrent
edits never collide. Worktree behaviour is explicit configuration, set globally and overridable
per project:

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
An environment's [persistent agent](#persistent-agent-implemented) uses the same machinery,
keyed by environment instead of folder.

**Implemented (phase 6):** `[[projects]]` entries with `agent = true` get an agent when the
daemon starts: an ordinary session (it appears in the session list and can be talked to) with
coordinator instructions and a `start_session` tool. It is woken by messages only: a schedule
with `action.agent`, `imperium project tell "<text>"` (`projects.message`), or a report from a
session it started, delivered as `Session <id> finished. Its report: ...` when that session's
turn ends. It uses no tokens between events. `max_sessions` (default 2) caps sessions running
at once and `max_sessions_per_day` (default 20) per day. Permission requests from the agent and
its sessions, and from scheduled prompt runs, become approval requests (the Approvals panel,
`imperium approvals`). `imperium daemon autostart enable|disable|status` starts `imperium-core` at
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
starts it at once; a person's run never waits. While a run waits, ticks that come due join it
instead of starting another, also with `overlap = "allow"` (the daemon logs them as skipped). A
waiting run is not kept across a restart: it was never recorded, so it counts as missed
(`missed`). The other conditions listed under
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
  Sessions waiting still count against `max_sessions`. The waiting brief is kept with the
  session (`deferred_brief` in its `.meta.json`, with the time `max_defer` ends), so after a
  restart the session waits again for what is left of `max_defer`, then gets its brief and
  reports to the project's agent; if the project no longer has an agent, it still runs, with
  nobody to report to. A session released by `max_defer` or by
  *run now* (also a schedule prompt run that waited out its `max_defer`, or one a person started
  with `schedules.run`) does not give way again for the rest of its life: "then run anyway"
  means it runs to the end. This is kept with the session (`run_now`), so a restart does not
  undo it. A message you write to a background session (`session.send`) releases it the same
  way; messages from the daemon (a schedule, a project agent) do not.
- **Yielding mid-run.** A running background session checks the view before each model call. If
  it is `busy` or `saturated`, the call in flight has already finished, so it pauses there: no
  slot held, no tokens spent, like `await_tasks`. It resumes when the view is `quiet`. The pause
  reuses the usage-limit pause (a `paused` event with `cause = "load"` and `until_unix_ms = 0`,
  then `resumed`). Subagents of a background session yield the same way, and a paused agent is
  detached from its local model, so a model you ask for can swap it out without waiting for it; if the model is swapped out
  meanwhile, its slot is not saved and it re-reads its context on resume.
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

## Archiving sessions

**Implemented:** a session can be archived, the way Claude's desktop app keeps its list tidy.
An archived session stops running and is not resumed when the daemon restarts, but its journal
and record are kept: `session.list` still returns it, with `archived: true` and `archived_at`
(milliseconds since the Unix epoch), and `session.subscribe` returns its history with nothing
live to follow. It takes no messages. `session.control` `archive` archives it and `unarchive`
resumes it where it left off; both are announced as `session.updated` (unarchiving also as
`session.created`), and the state is kept in the session's meta file, so it survives restarts.
`session.delete` deletes an archived session as well. From the CLI: `imperium session archive
<id>`, `imperium session unarchive <id>`, and `imperium session list [--archived]`.

Sessions are also archived on their own:

- **When its pull request closes** (`sessions.auto_archive_on_pr_close`, on by default): the
  watcher that follows the pull requests sessions [watch](#github-auto-fix) archives the
  session once the pull request is merged or closed. A merged session still busy is archived
  at a later check, once it is idle; it keeps its `merged` status in the list.
- **After inactivity** (`sessions.auto_archive_after_days`, `0` = off, the default): the daemon
  looks every hour (first a minute after it starts) for sessions whose last journalled event is
  that many days old.

Neither rule archives a session that is working, waiting on the user (an approval, a usage
limit, a failed turn, a model), has queued messages, is pinned, or is an environment's
[persistent agent](#persistent-agent-implemented) or a [project agent](#project-agent); those
two cannot be archived by hand either.

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
