# Claude harness

**Implemented (`imperium mcp-serve`).** An MCP server on standard input and output that lets an
external agent, Claude Code in particular, use a running Imperium daemon: hand it tasks to
orchestrate, and exercise its agents to find bugs and test features. It is the first piece of
the MCP server bridge planned in [02](02-architecture.md#rust-workspace) and
[12](12-roadmap.md) (item 31), which also covers exposing Imperium's own tools; nothing here
exposes those.

## How it works

The harness is a client of the daemon, like `imperium chat`: it reads the endpoint file
(`daemon.json` in the data folder, or a remote's with `--remote`), connects with the token in it over the local
socket or loopback WebSocket, and calls the ordinary protocol methods
([02](02-architecture.md#protocol)). It adds no protocol methods and changed no protocol
version. The MCP side is newline-delimited JSON-RPC 2.0 on stdio (revisions `2025-06-18`,
`2025-03-26`, `2024-11-05`; `initialize`, `ping`, `tools/list`, `tools/call`), written by hand in
`imperium-cli` (`crates/imperium-cli/src/mcp_serve.rs`); only protocol messages go to standard
output.

What it does not do:

- **It grants nothing.** Every session it starts is an ordinary session: the environment's
  folders and path grants, the permission mode (`permissions.mode`) and rules, the safety floor,
  budgets, isolation tier and hooks all apply as for a session started from the terminal.
- **It answers nothing on its own.** A permission request an agent raises stays pending until
  the caller sends `respond_permission` with an explicit decision; there is no auto-approve
  option, and the harness never calls `session.control` `set_permission_mode`.
- **It never shows the token.** The token stays in the harness process; no tool result or error
  includes it (the integration test checks every reply for it). The harness is as trusted as
  the user who can read the endpoint file: anything that can run it can drive the daemon, so
  register it only in configuration you trust.

The caller sees the daemon's own events: a session's replies, tool calls and permission
requests are read from the session's event history (`session.subscribe`), and a transcript from
`session.export`.

## Tools

| Tool | What it does |
| ---- | ------------ |
| `list_environments` | The environments (id, name, folders, runtime). |
| `list_sessions` | Sessions with status, model and environment; filter by `environment`, `include_archived`. |
| `start_session` | Creates a session (`environment` name or id, or `project_dir`; optional `model`, `subfolder`, `read_only`, `worktree`) and, with `prompt`, sends the first message. Returns `session_id`, the model, the folders and a `cursor`. With `wait_timeout_s` it also waits and returns the turn as `turn`. The runtime is the environment's. |
| `send_message` | Sends `text` to a session: it starts a turn when idle and queues otherwise (`steer` joins the running turn). Returns a `cursor`; `wait_timeout_s` waits as above. |
| `wait_for_turn` | Waits (`timeout_s`, default 60, at most 1800) for the turn that began at `since`, or the latest turn. Returns an `outcome`: `completed`, `failed`, `stopped`, `permission_required`, `questions_asked`, or `timeout` (still running: call again). It also returns `assistant_text`, the `tool_log` of the window, `pending_permissions`, `pending_questions`, token `usage` and a `cursor` for the next call. |
| `get_session` | Status, pending permission requests and questions, and the latest turn's outcome. |
| `get_tool_log` | Tool calls with arguments, results (clipped) and permission requests, oldest first (`since`, `limit`). |
| `get_transcript` | The daemon's export of the whole session (`markdown` or `json`), including subagents. Verbatim: it may hold secrets the agents read. A long one loses its start (`max_chars`). |
| `respond_permission` | `allow`, `allow_for_session` or `deny` for a `request_id` from `pending_permissions`. |
| `answer_questions` | Answers an `ask_user` batch, one string per question. |
| `stop_session` | Stops a session's work (`session.control` `stop`); it can be messaged again. |

A **cursor** is a session's event count. `send_message` and `start_session` return the count from
before the message, so `wait_for_turn` with that `since` looks only at the turn the message
starts. A turn that was already ending when the message was sent is not mistaken for it.

## Setting it up

The daemon must be running (`imperium daemon start`, or the desktop app). Register the server in
a project's `.mcp.json` (or with `claude mcp add imperium -- imperium mcp-serve`):

```json
{
  "mcpServers": {
    "imperium": { "command": "imperium", "args": ["mcp-serve"] }
  }
}
```

Use the full path to the `imperium` binary if it is not on `PATH`. For a daemon on another host
connected with `imperium remote connect`, use `"args": ["--remote", "<name>", "mcp-serve"]`. If
the daemon restarts, the connection closes and every tool says so; reconnect the server
(`/mcp` in Claude Code). Claude Code lists the tools as `mcp__imperium__<tool>`.

To use a model without a cloud key, point the environment at a local or fake endpoint in the
configuration as usual; the harness chooses nothing beyond the optional `model` argument.

## Testing guide

Both flows are driven by Claude Code calling the tools; the results are JSON.

### Orchestrate a task

1. `list_environments`, and pick the environment the work belongs in.
2. `start_session` with `environment` and a `prompt` that says the goal and what "done" means,
   and `wait_timeout_s: 120` if a short task is expected.
3. If the outcome is `timeout`, call `wait_for_turn` again with the returned `cursor` as
   `since`. Long tasks are polled; a timeout is not a failure.
4. On `permission_required`, read `pending_permissions[].summary` and `tool`, decide, and call
   `respond_permission`. Check what the agent actually asked to do: the summary is the same text
   the user would see. Then `wait_for_turn` again.
5. On `completed`, read `assistant_text`, then `get_tool_log` and the files the agent changed
   (in the environment's folders) before trusting the result. `send_message` continues the same
   session with feedback.
6. `stop_session` ends work that has gone wrong.

### Smoke-test an agent capability

To check that a tool works, for example `read_file`:

1. Put a small known file in a throw-away folder, and `start_session` with
   `project_dir` set to it and `prompt: "Read notes.txt and tell me its first line."`, with
   `read_only: true` when the check must not write.
2. `wait_for_turn`. The outcome should be `completed`; the `tool_log` should hold a
   `tool_started` and a `tool_finished` entry with `tool: "read_file"`, `ok: true` and the file's
   text in `output`; `assistant_text` should repeat the first line.
3. For a tool that needs approval (`write_file` in the `ask` or `default` mode, `run_command`),
   expect `permission_required`, answer it with `respond_permission`, and check both the
   `deny` path (the agent carries on and nothing changes) and the `allow` path (the file
   exists).
4. A failing capability shows as `ok: false` with the tool's error in `output`, an `outcome` of
   `failed` with `message`, or `notices` (such as an MCP server that did not start). Use
   `get_transcript` for the whole conversation when the log is not enough.

Everything the harness starts appears in the desktop app and `imperium session list`, and can be
archived or exported there like any session.

## Tests

`crates/imperium-cli/tests/mcp_serve.rs` runs the harness over an in-memory stdio pipe against
a real daemon whose model is a fake HTTP endpoint (no model runtime, no network): tool listing,
orchestrating a task, the `read_file` smoke test, a permission request left pending until
denied and then allowed (with the file written only after `allow`), timeouts, errors, and the
token never appearing in a reply. The event-reading logic has unit tests of its own.

## Not built

- Exposing Imperium's own tools (file, search, memory and so on) as MCP tools to other agents,
  and MCP resources or prompts for sessions.
- Automatic reconnection to a restarted daemon, and the streamable HTTP transport (stdio only).
- Streaming a turn's text to the caller as it is written (the result arrives when the turn
  ends or the wait times out); MCP progress notifications.
- Creating or configuring environments, changing models or permission modes, archiving, the
  approval queue of unattended work (`imperium approvals`), budgets confirmations
  (`confirm_budget`) and attachments.
- Cancelling a running wait when the caller sends `notifications/cancelled` (the wait runs to
  its timeout).
