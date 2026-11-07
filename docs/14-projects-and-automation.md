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

## Project agent

A long-lived agent per project that watches for work and **dispatches it into new sessions**.

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
  only if the previous run succeeded.
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
