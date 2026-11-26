# 02 — Architecture

## Process model

```text
 ┌──────────────┐   ┌──────────────┐   ┌──────────────────┐
 │ Desktop app  │   │  imperium    │   │ Scripts / mods / │
 │ (Tauri+React)│   │  (CLI/TUI)   │   │ other clients    │
 └──────┬───────┘   └──────┬───────┘   └────────┬─────────┘
        │  JSON-RPC 2.0 over WebSocket (local) or SSH-forwarded socket
        └──────────────────┼────────────────────┘
                    ┌──────┴───────┐
                    │ imperium-core│  sessions · agent engine · scheduler · telemetry
                    └──┬───┬───┬───┘
        ┌──────────────┘   │   └───────────────┐
 ┌──────┴──────┐  ┌────────┴───────┐  ┌────────┴────────┐
 │ Model       │  │ Tool hosts     │  │ Sidecars        │
 │ runtimes    │  │ (fs, exec, web,│  │ (media: Python, │
 │ llama-server│  │  browser, MCP) │  │  ComfyUI)       │
 │ vLLM, HTTP  │  └────────────────┘  └─────────────────┘
 └─────────────┘
```

- **`imperium-core`** is the only process that holds state. It owns sessions, the agent engine,
  the model scheduler, tool execution, policy resolution, configuration, and telemetry.
- **Clients** are stateless views. The desktop app and the `imperium` command each attach to the
  running daemon, start one when none runs, and hold a lease on it while they run, so the daemon
  lives as long as anything uses it ([shared daemon lifetime](#shared-daemon-lifetime-implemented)).
  `imperium run --headless` runs one task through the agent loop with no daemon at all, for
  evaluations ([03](03-agent-engine.md#running-evals-through-the-harness-implemented)).
- **Remote hosts** run their own `imperium-core`. The desktop connects through an SSH tunnel to the
  remote daemon's Unix socket, so remote sessions use the remote machine's files, models, and
  GPUs with no special-case code paths (see [isolation](05-tools-and-isolation.md#ssh-hosts)).
- **Model runtimes and sidecars** are child processes supervised by the daemon (restart policy,
  health checks, log capture, memory accounting).

### Shared daemon lifetime (implemented)

One daemon runs per user, and its lifetime is a reference count: a daemon a client started for
itself exits once **no client holds a lease and no work keeps it up**. A daemon started on its
own (`imperium-core` run by hand, `imperium daemon start`, `imperium daemon autostart`, a
service) is **persistent** and never exits for want of clients.

- **Starting.** Every client that needs the daemon (the desktop app, every `imperium` command
  that talks to it, `imperium mcp-serve`) reads `<data>/daemon.json` and connects. When no daemon
  answers (no file, a stale file left by a crash, a refused connection, or one that is exiting),
  it starts `imperium-core --exit-when-idle` and waits up to 20 seconds for it to listen. The
  CLI uses the `imperium-core` beside the `imperium` executable, then the `PATH`; the app uses
  the bundled one. The daemon is started detached from the caller's console and process group
  (`CREATE_NO_WINDOW` and a new process group on Windows, which gives the daemon a hidden console its children share instead of each opening a window; a new process group on Unix), so
  closing the terminal or pressing Ctrl+C does not end it; its output goes to
  `<data>/imperium-core.log` (the app's log folder for the app). Several clients may start one
  at the same moment: the single-instance lock on `<data>/daemon.lock` is the arbiter. The
  daemon takes it before it listens or writes `daemon.json`, so one wins and the others exit at
  once ("already running"); every client keeps polling `daemon.json` and attaches to the winner.
  A client whose daemon is still missing 3 seconds after it started one starts it again.
- **Leases.** `system.lease` (protocol 0.44) registers the caller as a user of the daemon for as
  long as that WebSocket stays open: the client sends its kind (`cli`, `desktop`, `tool`), a
  label (the subcommand, such as `imperium chat`, never its arguments) and its process id. The
  lease belongs to the connection, so a normal exit, a kill and a lost socket all end it. The
  process id covers what a socket cannot: a hung or crashed client can leave a connection open,
  so once a second the daemon checks that each lease's process is still in the process table
  (start times are not compared: on Linux they move when the clock is stepped, and a process
  that ended has closed its connection anyway); a lease whose process is gone is dropped. A
  command that the daemon's own agents run (`run_command`, background commands, hooks; they
  carry `IMPERIUM_SPAWNED_BY_DAEMON`) takes no lease and starts no daemon, since a lease would
  keep the daemon up for as long as the agent waits on it (`imperium mcp-serve` refuses to run
  as one of a session's own MCP servers, which carry `IMPERIUM_MCP_CHILD`). A client over an SSH forward sends no process id, since the id
  means nothing on the daemon's machine, and a lease is then only its connection. Each running
  `imperium` command holds one, so commands run in parallel each hold one; a daemon of an
  earlier protocol answers "method not found" and is simply used (it is persistent).
- **Work.** Some things the daemon does on its own keep it up with no client: a running turn (a
  session that is working, or paused for a quiet machine or a usage limit; one waiting only on a
  permission or a question is not counted, since with no client nothing can answer it and it is
  resumed from its journal when a daemon starts), an enabled schedule with a run to come or
  running, a persistent agent whose initiative is on (`suggest` or `act`), and GitHub
  responders (auto-fix with watched repositories, the pull requests sessions follow, and the review comments of pull requests whose auto-fix switch is on).
  long jobs the daemon runs on its own (a running model download, benchmark, llama.cpp runtime
  install or update, or a move of the working folders, so an exit never cuts them short), and,
  while the phone listener is on and a paired device may approve, a session or request waiting
  for an approval (the phone can answer it). `system.lifetime` lists them. The phone listener
  and the OpenAI-compatible API themselves are not work: a daemon that serves other devices is
  meant to be a persistent one (`imperium daemon autostart enable`, or **Keep running** in the
  desktop app).
- **Children.** What the daemon starts ends with it, however it dies. On Windows it puts itself
  in a kill-on-close job at start (jobs nest, so the jobs background commands make for
  themselves still work), and the installer's `taskkill` ends the whole tree. The job allows
  breakaway, and what the daemon launches for the user rather than for an agent (`workspaces.open`
  showing a folder or `Memory.md` in Explorer or an editor) starts with
  `CREATE_BREAKAWAY_FROM_JOB`, so it stays open when the daemon exits; model servers, MCP
  servers, the speech server, the browser and agents' commands stay in the job. Elsewhere a
  killed daemon cannot take its children down without unsafe code, so every `llama-server` or
  `vllm` it starts, and the local speech server (`whisper-server`), is written to
  `<data>/logs/children.txt` with its start time and name, and the next daemon ends what is
  still running under that id, start time and name before it loads anything.
- **Exiting.** When there is no lease and no work for the grace period (10 seconds, long enough
  that one command after another reuses the daemon; `--exit-when-idle=SECONDS` on the daemon,
  `IMPERIUM_DAEMON_IDLE_SECS` for the daemon an `imperium` command starts) the daemon exits
  through the path `system.shutdown` takes: background commands are stopped, the speech server
  stops, connections close, `daemon.json` is removed, and sessions resume from their journals the
  next time. The decision is made under the lock that grants leases: from then on `system.lease`
  answers `-32009` (daemon stopping), and a client that gets it waits for the old daemon to go
  and starts a new one, so no client is left attached to a daemon that is leaving.
- **Control.** `system.lifetime` returns the mode (`shared` or `persistent`), the grace period,
  the leases (id, kind, process id, label, how long held), the work, and how long the daemon has
  been idle and when it will exit; with `persistent` it chooses the mode while the daemon runs
  (a daemon that has decided to exit stays decided). `imperium daemon status` prints it. The
  desktop's **Keep running after Imperium closes** switch uses it. `system.shutdown`
  (`imperium daemon stop`, the app's Stop) still stops the daemon at once whatever holds it,
  and from the moment it is asked, `system.lease` answers `-32009` too.
- **Opting out.** `IMPERIUM_NO_DAEMON_SPAWN=1` makes `imperium` commands attach to a running
  daemon but never start one (for CI and tests); `imperium ping`, `imperium daemon stop` and
  `imperium daemon status` never start one either, since they ask about a daemon. A `--remote`
  command never starts anything locally: a remote host's daemon is started over SSH with
  `imperium daemon start` (by `imperium remote connect`), so it is persistent, and the command's
  lease on it carries no process id.
- **Signals.** On Unix SIGTERM (`docker stop`, a service manager) runs the same shutdown as
  Ctrl+C: background commands are stopped, connections close, and `daemon.json` and the local
  socket are removed. The handler is installed before the endpoint file is written, so a SIGTERM
  that arrives while the daemon starts waits for the shutdown path.

### Server mode (implemented)

`daemon.server` (default `false`) makes the daemon a server
([plan](plans/server-package.md), [D5](plans/server-package.md#d5-features-unavailable-on-a-server)):
the server image sets it with `IMPERIUM_DAEMON__SERVER=true`, and environment variables are the
last configuration layer, so a `config.toml` on the data volume cannot unset it. It only ever
removes capabilities. The handshake reports it (`HandshakeResult.server`), so clients hide the
desktop-only controls, and each refused call answers `-32010` (`UNAVAILABLE_ON_SERVER`) with
`data: {feature}`, checked at the top of the method, before any peer is looked at or any
challenge is issued, so nothing is recorded:

| Refused on a server | `feature` |
| --- | --- |
| `plugins.approval_challenge`, `plugins.approve` | `plugin_approval` |
| `pull_requests.auto_merge_challenge`, `pull_requests.set_auto_merge` with `enabled: true` (turning it off works from any client) | `auto_merge_enable` |
| `mods.install` with `trusted` (an error whose message starts with `trust was not granted`, so the CLI installs the mod in its sandbox instead); `interactive` is dropped with a note | `terminal_grant` |
| `isolation.tier = "container"`: refused by configuration validation (the daemon does not start with it), by `config.set`, and at session start, where a Docker image runtime is refused too (no Docker socket on a server) | `container_tier` |

`imperium runtime vllm setup --docker` fails with a message on a server, and a server daemon whose
data folder records a Docker setup of vLLM starts `vllm` from `PATH` instead of a container.
Approvals recorded earlier (a data folder carried over from a desktop) are verified as on any
daemon and still count: the refusals apply to making approvals, not to reading them. The
remaining features of the plan's table (`secret_write`, `mcp_login`, `vllm_docker` as an error
code) belong to later stages; their names are already in
`imperium_protocol::jsonrpc::server_features`.

Underneath server mode, a structural rule holds whatever any setting says: a daemon in a nested
PID namespace refuses every grant and app-identity approval
([06](06-extensibility.md#who-may-grant-trust-and-approval-implemented)). A server-mode daemon
whose `NSpid` line has a single entry (a container run with `pid: host`, or no container) logs
once at start that only server mode refuses grants there.

## Protocol

- JSON-RPC 2.0 requests and responses, plus server-pushed **event streams**
  (`session.subscribe`, and the topic choice below).
- **Transport (implemented):** a WebSocket at `ws://127.0.0.1:<port>/rpc`, text frames only.
  The port is `daemon.port` (default `0`: a free port chosen at each start). The upgrade must
  carry `Authorization: Bearer <token>`; anything else gets HTTP 401. The token is 32 random
  bytes generated at every launch. It is never bound to a public interface. Browser-based
  clients (the desktop webview), which cannot set headers, instead offer the subprotocols
  `imperium.v0` and `imperium.token.<token>`; the daemon selects `imperium.v0`.
- **Local socket (implemented, on by default):** with `daemon.socket` on (the default) the
  daemon also serves the same HTTP and WebSocket server, with the same `/rpc` path and the same
  token check, on a Unix domain socket or a Windows named pipe. A local connection is not
  trusted for being local: without the token the upgrade gets 401 there too. It is on by
  default because the `imperium` command needs it for evaluation runs (`imperium run
  --headless`, which the daemon refuses over loopback TCP, since a page in the app's window
  reaches the daemon there) and the desktop app for plugin approvals. If it cannot be opened the
  daemon logs a warning and serves loopback TCP alone. The address goes in `daemon.json` as
  `socket`.
  - Unix: `<data>/ipc/daemon.sock`. The `ipc` folder is created mode 0700 and the socket set to
    0600, so only the current user can open it, even in the instant between creating and
    changing the socket. At start a socket nobody answers on (left by a crash) is replaced; a
    socket another process still answers on, or a file that is not a socket, stops the start
    with an error rather than being removed. The socket is removed on a clean shutdown.
  - Windows: a named pipe `\\.\pipe\imperium-core-<random>`, new at every start, created with
    a protected DACL that grants only the current user's SID, local clients only, and as the
    first instance of its name so another process cannot take the name before the daemon. The
    daemon always holds an instance (it creates the replacement before handing over a connected
    one, and keeps the old one if that fails), so the name is never vacant for another local
    user to take. A pipe name is not a secret and `daemon.json` can outlive the daemon, so a
    client that finds a pipe in it checks that the process serving the pipe is the `pid` in the
    same file (`GetNamedPipeServerProcessId`) before sending the token, and refuses otherwise.
    Pipes disappear with the process, so there is nothing stale to clean up.
  - Remote use: forward the socket with OpenSSH, for example `ssh -N -L
    /tmp/imperium.sock:<remote data>/ipc/daemon.sock host`, or let `imperium remote connect
    <host> --socket` do it (with `StreamLocalBindMask=0177`, so the local end is also mode
    0600). The remote token is in the host's `daemon.json`; the forwarded endpoint file the
    command writes holds it. `-R` works the other way, to let a remote machine reach a daemon
    behind your firewall. SSH cannot forward a Windows named pipe, so a Windows host is
    reached through its TCP port as before; the pipe serves clients on the same machine.
- **Event subscriptions (implemented):** a new connection receives every notification it may,
  as before. `events.subscribe` with `topics` narrows it to those topics (a notification method
  such as `session.event`, a prefix such as `session.*`, or `*` for everything again) and
  later calls add to them; `events.unsubscribe` removes topics, or, on a connection still
  receiving everything, mutes them. Both return the current choice (`topics`, which is `null`
  while everything is received, and `muted`). The choice is applied after the existing rules
  (a restricted connection's allowed list, benchmark progress withheld from a paired phone,
  resource snapshots only for `system.resources_subscribe`), so it can only narrow: naming a
  topic the connection could not receive changes nothing. A connection may name at most 128
  topics; a malformed topic gets `-32602`. Paired phones may not call these methods. Replies
  to calls are never filtered, and `session.subscribe` still returns a session's history.
- **Discovery (implemented):** on start the daemon writes `<data>/daemon.json` (URL, token,
  PID, protocol version, and the local socket's address when `daemon.socket` is on, as it is by default), readable
  only by the user (mode 0600 on Unix; per-user directory ACL on Windows), and removes it on
  shutdown. Clients that find a `socket` there try it before the URL. It holds a lock on `<data>/daemon.lock`, so only one
  daemon runs per user; a second start exits with "already running". A daemon killed without
  cleanup leaves a stale `daemon.json`, which the next start overwrites. A client that finds no
  daemon behind the file starts one ([shared daemon lifetime](#shared-daemon-lifetime-implemented)).
- **OpenAI-compatible API (implemented):** a second, optional listener for other tools, off
  until `api.openai_compatible.enabled`. It serves `/v1/models` and `/v1/chat/completions` over
  HTTP on `api.openai_compatible.bind` (loopback only unless `allow_lan` is set) and requires its
  own bearer token, which lives in the secret store as `api.openai-token` and is neither the
  daemon's token nor ever written to the config or logs. Its calls go through the residency
  controller like any other and count toward budgets as the `api` source
  ([04](04-models-and-routing.md#openai-compatible-local-api-implemented)). `api.get`,
  `api.set` (which saves the settings and starts or stops the listener at once), and `api.token`
  (show or rotate) manage it over the JSON-RPC socket; a paired phone may not call them.
- **Handshake (implemented):** every connection's first call must be `system.handshake`
  (client name/version and protocol version). Other calls before it fail with `-32002`. The
  daemon refuses a different protocol major with `-32001`. `system.ping` returns uptime.
- **Sessions (implemented):** `session.create` (the caller is subscribed), `session.list`
  (each summary carries `created_at`, from when the session's journal was started),
  `session.subscribe` (returns the session's full event history so far, then streams new
  events with no gap or duplicate), `session.search` (searches running and archived sessions'
  content: the best match per session with an excerpt, highlights, and the history event to
  open at; see [17](17-memory.md#session-search-implemented)), `session.export` (a session's
  transcript as Markdown or JSON, from the journal, so archived sessions work too; the CLI's
  `imperium session export`, also spelled `sessions`; exports are verbatim and may hold
  secrets an agent read, which is why the result carries a `warning` when one looks like a
  credential), `session.send`, and
  `session.respond`. Events arrive as
  `session.event` notifications. `session.send` takes an optional `mode` (`queue`, the
  default: run now, or after the current turn; `steer`: join the running turn) and `agent_id`
  (a running subagent), and `attachments` (files staged with `session.attach`, which references
  a file inside the session's folders or copies it into the session's attachments folder), and
  returns the message's `id`; `session.queue` lists, edits, or
  removes queued messages ([03](03-agent-engine.md#queueing-and-steering)). `voice.transcribe`
  turns recorded speech into text through `[media.transcription]`
  ([07](07-desktop-ui.md#attachments-and-voice-input-implemented)). `session.agents`
  lists a session's agents with their kept state and `session.agent_resume` gives a finished
  subagent a follow-up ([03](03-agent-engine.md#agent-state)). `session.control` changes a running session between turns:
  its usage-limit policy, what it watches, its model (without loading it; the next turn
  compacts the conversation to fit, [03](03-agent-engine.md#switching-models-and-compaction)),
  or compaction now, pins it (`set_pinned`) or renames it (`rename`; both kept across restarts
  and announced as `session.updated`); model switches and compaction are refused with `-32006` while a turn runs,
  and compaction with `-32007` when the conversation cannot fit.
- Schema defined in Rust (`imperium-protocol`) and exported to TypeScript with `ts-rs` into
  `packages/protocol/src/generated/` whenever `cargo test` runs. `scripts/check-rust.sh` (run
  by CI) fails if the checked-in files differ. The pnpm package `@imperium/protocol`
  (`packages/protocol`) re-exports them; its `src/index.ts` is generated by
  `scripts/generate-index.mjs` on every typecheck and is not checked in.
- Versioned: `ProtocolVersion { major, minor }` in the handshake (currently 0.66: 0.66 added `HandshakeResult.server` (the daemon runs in [server mode](#server-mode-implemented); `false` from an older daemon) and the error code `-32010` (`UNAVAILABLE_ON_SERVER`, with `data: {feature}`, `feature` one of `plugin_approval`, `auto_merge_enable`, `terminal_grant`, `secret_write`, `mcp_login`, `container_tier` and `vllm_docker`); no method was added or removed; 0.64 added the optional `text` of `ApprovalInfo` (the exact text of a request whose text may be edited, a `reply` to a review thread) and of `approvals.respond` (`ApprovalRespondParams`: the user's own text, defanged and used instead of the proposed one; refused for any other request and from a paired phone), and the `replied`, `reply_declined` and `held` statuses of `autofix.event`, see [14](14-projects-and-automation.md#replies-fix-ci-on-the-wake-path-and-the-budget-implemented-stage-8-protocol-064); 0.63 added `pull_requests.set_monitor` (`PullRequestsSetMonitorParams`: a session's `auto_fix` switch for its pull request, which lets new reviews and comments from people with write access wake it, refused for a pull request the signed-in account did not open; `auto_archive_on_close`; `ack_untrusted`, which clears the card's notice that the session handled review text written by others; not for a paired phone), the `trigger` (`ci`, `review`, `conflict`) and `comment_ids` fields of `autofix.event` (`AutofixEventParams`) and `PrStatus.ignored_comments` (comments from people without write access that woke nothing), with `PrStatus.auto_fix` and `untrusted_handled` now acting, see [14](14-projects-and-automation.md#review-comment-wake-implemented-stage-7-protocol-063); 0.62 added `plugins.inspect_folder` (`PluginsInspectFolderParams`: the installed folder's name; the answer is the same `PluginPreview` as `plugins.inspect`, read from the folder itself, with the content hash and run-summary digest the load-time gate computes, so the trust view can list what a folder that did not come from a marketplace would run and `plugins.approval_challenge` and `plugins.approve` bind that hash and digest; a paired phone cannot call it), see [06](06-extensibility.md#plugin-marketplaces); 0.61 added `worktree_dir` to `EnvironmentInfo`, `EnvironmentCreateParams` and `EnvironmentUpdateParams`, the environment's own git worktrees folder, which replaces `sessions.worktree_dir` for its sessions and writer subagents (an empty value on update clears it), see [14](14-projects-and-automation.md#worktrees); 0.60 added `pull_requests.auto_merge_challenge` and `pull_requests.set_auto_merge` (turn GitHub's auto-merge on for a session's pull request, with `PullRequestsAutoMergeChallengeParams`, `PullRequestsAutoMergeChallengeResult` and `PullRequestsSetAutoMergeParams`: turning it on needs a single-use challenge bound to the connection, the pull request's address, the head commit and the merge method, and is answered only to the desktop app installed beside `imperium-core` on the local socket; turning it off needs no challenge and any local caller may send it; a paired phone may send neither), see [05](05-tools-and-isolation.md#auto-merge-the-desktop-app-alone-implemented) and [14](14-projects-and-automation.md#github-auto-fix); 0.59 added the `tool_search` and `call_tool` tool definitions (reactive tool loading, [roadmap item 61](12-roadmap.md#next-in-order); the loader is off by default and not yet a setting) and the `tools_loaded` session event (`ToolsLoaded`, with `ToolLoadVia`: the names of tools whose parameters a `tool_search` showed, or that ran through `call_tool`); 0.58 added `SessionSummary.pull_request_url`, the link of the pull request a session is bound to (the binding itself is kept in the session's record across restarts, and `session.review_pull_request` binds the pull request it opens), see [14](14-projects-and-automation.md#github-auto-fix); 0.57 added `pull_requests.bind`, `pull_requests.unbind` and `pull_requests.status` (a session's bound pull request and the daemon's cached `PrStatus` of it, with `PullRequestsBindParams`, `PullRequestsUnbindParams`, `PullRequestsUnbindResult`, `PullRequestsStatusParams`, `PrStatus`, `PrState`, `PrChecks`, `PrReview`, `PrConflictState`, `PrMergeMethod` and `PrAutoMerge`) and the `pull_requests.changed` notification (`PullRequestsChangedParams`: the session and its new status, or none after an unbind); a paired phone may call `pull_requests.status`, and `session.control` `watch` and `unwatch` are aliases for bind and unbind, see [14](14-projects-and-automation.md#github-auto-fix); 0.56 added `plugins.approval_challenge` and `plugins.approve` (a single-use challenge bound to the connection, the plugin's hash and run-summary digest, and the approval that answers it, with `PluginsApprovalChallengeParams`, `PluginsApprovalChallengeResult`, `PluginsApproveParams` and `PluginsApproveResult`; taken only from the desktop app installed beside `imperium-core` on the local socket, and no approval counts yet), see [06](06-extensibility.md#plugin-marketplaces); 0.55 added `plugins.suggestions` and `plugins.dismiss_suggestion` (the marketplaces and plugins a trusted project's `.imperium/config.toml` offers, and dismissing one offer or muting a project, with `PluginsSuggestionsResult`, `SuggestedMarketplace`, `SuggestedPlugin` and `PluginsDismissSuggestionParams`; offers add, fetch, install and enable nothing), see [06](06-extensibility.md#plugin-marketplaces); 0.54 added `plugins.set_secret` and `plugins.delete_secret` (saved secrets for a marketplace plugin's `${VAR}`, with `PluginsSetSecretParams`, `PluginsDeleteSecretParams` and `PluginsSecretResult`), `delete_secrets` on `plugins.uninstall`, and `required_secrets` and `missing_secrets` on `plugins.list` rows, see [06](06-extensibility.md#plugin-marketplaces); 0.53 added `plugins.set_enabled` (turn a plugin on or off for the user or one environment) and `plugins.update` (replace a marketplace plugin with its source's current content, withheld when it has code), `EnvironmentInfo.plugins` (the `EnvironmentPlugins` lists), the `environment` param of `plugins.list` and its `decided_by` field, see [06](06-extensibility.md#plugin-marketplaces) and [14](14-projects-and-automation.md); 0.52 added `plugins.list` and `plugins.uninstall` (list every plugin folder with its origin, approval kind and gate verdict; uninstall one to `config-trash` with its record, `config.undo` restoring both), the `id`, `origin`, `withheld` and `withheld_reason` fields of `mcp.list` plugins, and the `sha256` and `record` fields of `config.changes` entries, see [06](06-extensibility.md#plugin-marketplaces); 0.51 added `plugins.install` (install a marketplace plugin, withheld when it has code, with `PluginsInstallParams` and `PluginsInstallResult`), see [06](06-extensibility.md#plugin-marketplaces); 0.50 added `plugins.inspect` (inspect a marketplace plugin, with the `PluginPreview`, `PluginHookRun`, `PluginServerRun` and `PluginInstalled` types), see [06](06-extensibility.md#plugin-marketplaces); 0.49 added `plugins.marketplace_add`, `plugins.marketplace_list`, `plugins.marketplace_refresh`, `plugins.marketplace_remove` and `plugins.browse` (plugin marketplaces, with the `MarketplaceInfo`, `MarketplaceKind`, `MarketplaceRefresh`, `PluginListing` and `PluginComponents` types), see [06](06-extensibility.md#plugin-marketplaces); 0.48 added `session.builtin` (the built-in slash commands `init`, `code_review` and `security_review`, with the `BuiltinCommand` type; `session.init` stays as the same call for `init`), see [07](07-desktop-ui.md#file-mentions-and-init-implemented); 0.47 added `session.files` (the file search behind `@` mentions, with the `SessionFile` type), `session.init` (`/init`) and the `mention` option of `session.attach`, see [07](07-desktop-ui.md#file-mentions-and-init-implemented); 0.46 added `pool_max` to `LocalLaunchEffective` (the KV pool's ceiling, which the `kv_pool_bytes`, `required_bytes` and placement of `models.config` local rows are now worked out for, see [07](07-desktop-ui.md#model-configuration-dialog-implemented)); 0.45 added `session.eval_create`, `session.eval_result` and `session.prompt_preview`, the CLI-only methods of `imperium run`, which start an evaluation session (a harness variant, subagent recordings, a root model of its own, unattended permission handling) in the daemon, with no generated TypeScript types and no access for a paired phone or any client but the `imperium` command, see [03](03-agent-engine.md#running-evals-through-the-harness-implemented); 0.44 added `system.lease` and `system.lifetime`, the error code `-32009` (the daemon is stopping), and the types `LeaseKind`, `LeaseInfo`, `LifetimeMode`, `LifetimeStatus`, `WorkKind` and `WorkInfo`, see [shared daemon lifetime](#shared-daemon-lifetime-implemented); 0.43 added `auto` to the permission-mode values (`session.control` `set_permission_mode`, `permission_mode` on session summaries and control results, and `permissions.mode`; no method or type changed, see [05](05-tools-and-isolation.md#auto-mode-implemented)); 0.42 added `agents` (`AgentFileInfo`) to `mcp.list` results and the `agent` argument of the `spawn_agent` tool, the named agents of agent files, see [06](06-extensibility.md#named-agents-implemented); 0.41 added `session.tools`, `session.run_tool` and `session.tool_result`, the daemon side of the MCP server bridge `imperium mcp-serve --tools` (CLI methods, with no generated TypeScript types; a paired phone cannot call them), see [20](20-claude-harness.md#exposing-imperiums-own-tools-implemented); 0.40 added the `interactive`, `trusted` and `notes` fields of the `mods.install` result (what the daemon counted of an approval, see [06](06-extensibility.md#who-may-grant-trust-and-approval-implemented)); 0.39 added `session.agent_ask`, with which the user asks a knowledge agent a question (see [21](21-knowledge-agents.md)); 0.38 added knowledge agents: `AgentSummary.knowledge` (the `KnowledgeCard` and its parts), the `knowledge_changed`, `question_asked` and `question_answered` session events, `session.agent_knowledge`, and the `knowledge` argument of the `spawn_agent` tool and the `ask_agent`, `list_knowledge_agents` and `keep_as_knowledge` tools, see [21](21-knowledge-agents.md); 0.37 added `mcp.login` and `mcp.logout` and the `sign_in` field of the status of `mcp.list` servers (OAuth sign-in for remote MCP servers, see [05](05-tools-and-isolation.md#oauth-for-remote-servers-implemented)); 0.36 added `models.disabled` (the specs of models the user switched off) as `disabled` on `models.config` results and `models.set_config` params, `default_model` on `models.config` results, `enabled` on local model rows (`models.local`) and on `models.config` local rows, and the `weights_bytes`, `layers`, `gpu_layers`, `vram_bytes` and `ram_bytes` memory-estimate fields of `models.config` local rows, see [04](04-models-and-routing.md#disabled-models-implemented); 0.35 added browser tabs: the `tab` and `tabs` fields of the `browser_view` session event and of `BrowserPage` (the result of `session.browser_control` and `session.browser_act`), the `BrowserTab` type, and the `new_tab`, `switch_tab` and `close_tab` actions of `session.browser_act`, see [05](05-tools-and-isolation.md#fetch-cache-and-robotstxt-implemented); 0.34 added the `main_context` and `trusted` fields of `mods.list` rows, and `main_context` on `mods.inspect` results, `trusted` on its installed mod and on `mods.install` params (a mod that asks to run in the app's own window, and the user's confirmation of it), see [06](06-extensibility.md#trusted-main-context-mods-trust-implemented-loader-not-built); hooks may answer `tool.before` and `prompt.submit` with a rewrite, which changes no protocol type, see [06](06-extensibility.md#rewriting-hooks-implemented); 0.33 added `mods.inspect` and `mods.install`, the daemon side of `imperium mods install` and `imperium mods update` (CLI methods, with no generated TypeScript types; a paired phone cannot call them), the `opt_in` and `modified` fields of `mods.list` rows, and the `url` and `headers` fields of `[[hooks]]` and the `session.end`, `turn.start`, `turn.end` and `permission.requested` hook events (no session event or desktop type changed), see [06](06-extensibility.md#hooks); 0.32 added `permissions` (an agent permission profile: MCP server and plugin allowlists and a local-files-only lock) to environment agents and to `environments.agent_create` and `environments.agent_update` (0.28 and 0.30 are not used); 0.31 folded the project-private and project-shared memory scopes into the environment scope of the memory tools (`memory_remember`, `memory_search`, `memory_read`) and the `memory.write` settings, and accepts `project`, `project-private` and `private` as names for `environment` in this version only, see [17](17-memory.md#names-kept-for-one-protocol-version); no method or type changed; 0.29 added `environments.config`, the `subfolder` and `read_only` options of `session.create` and the `read_only` field of session summaries, and the `environment` field of schedule rows; 0.27 added the `initiative.*` methods, `initiative.changed` and `initiative.notice` notifications, `initiative` on environment agents (off by default), and `started_by` on session summaries; 0.26 added `events.subscribe` and `events.unsubscribe` and the optional `socket` field of the discovery file; 0.25 added the `access` argument of `spawn_agent` (`write` gives a subagent git worktrees of its own); 0.24 added the `draft_model`, `draft_max`, `draft_min` and `draft_gpu_layers` settings of local models (`LocalLaunchSettings` and `LocalLaunchEffective`), `draft_in_use` and `draft_notice` on `models.config` rows, and `draft_model` and `draft_notice` on the active local models of `system.resources`; 0.23 added the `builtin` field of environments (the Home environment, and `session.create` with neither environment nor project starting there); 0.22 added the `environments.agent_*` methods, `agents` on environments, and made `persistent_agent` and `agent_session_id` deprecated aliases that a later version removes; 0.21 added `models.bench`, `models.bench_results`, `models.bench_cancel` and the `models.bench_progress` notification for the local model benchmark; 0.20 added `workspaces.where`, `workspaces.move`, `workspaces.open`, and the `workspaces.progress` notification; 0.19 added `api.get`, `api.set`, `api.token` and the `source` budget scope for the OpenAI-compatible API; 0.18 added `session.browser_control`, `session.browser_act`, and the `browser_control` and `preview_changed` session events; 0.17 added `session.control` `set_permission_mode` and the `permission_mode` fields of session summaries and control results; 0.16 added `session.branch`, `session.compare` and `session.compare_pick` and the `branched_from` and `comparison_id` fields of session summaries; 0.15 added `session.export` and `kept_context` on `context_compacted`; 0.14 added `session.review`, `session.review_hunk`, `session.review_commit`, and `session.review_pull_request`; 0.13 added `personalities.list`; 0.12 added `session.checkpoints`, `session.rewind`, and the `session.checkpoint` notification; 0.11 added the `model.residency` notification; 0.9 added `session.control` `set_effort`, the `effort` and `reasoning` fields of session summaries and control results, `reasoning` on model rows, and `effort` on `spawn_agent`; 0.8 added `session.attach`, `attachments` on `session.send`, queued messages, `turn_started`, and `steered`, and `voice.transcribe`; 0.7 added the `local_model_waiting` session event and the local model rows' context, layer and memory fields; 0.6 added the `budgets.*` methods and notifications, `confirm_budget` on `session.send`, and the `-32008` budget error; 0.5 added the `permissions.*` methods, environment permissions, and suggested rules on approvals); additive
  changes bump minor, breaking changes bump major and keep the previous major for one release.

Core method groups: `session.*`, `agent.*`, `task.*`, `model.*`, `runtime.*`, `tool.*`,
`mcp.*`, `plugin.*`, `policy.*`, `config.*`, `secret.*` (write-only), `telemetry.*`, `fs.*`.

## Rust workspace

| Crate                 | Responsibility                                                         |
| --------------------- | ---------------------------------------------------------------------- |
| `imperium-api`        | Shared types and traits: chat messages, tool specs, usage and pricing, the local-model and tool-provider interfaces, Markdown front matter (implemented) |
| `imperium-client`     | The client of the daemon protocol, used by the CLI and the tests (implemented) |
| `imperium-ipc`        | Same-user local sockets: Unix domain socket, Windows named pipe (the one crate that allows `unsafe`, for the pipe's ACL and the pipe client's process handle) |
| `imperium-protocol`   | Wire types, method/event definitions, TS export                        |
| `imperium-engine`     | Agent engine library: sessions, agent loop, delegation, task graph, event bus |
| `imperium-models`     | Endpoint clients behind `ChatClient`, model registry, router, cost accounting |
| `imperium-local`      | GGUF discovery, hardware probe, memory planner, runtime packs, runtime supervision (llama.cpp, vLLM); carries out residency decisions, decides none on its own |
| `imperium-tools`      | Built-in tools: files, commands, isolation tiers; confines paths to a workspace and asks `imperium-policy` before it acts (implemented) |
| `imperium-web`        | Web search providers, page fetching, and a CDP browser (implemented)   |
| `imperium-mcp`        | MCP client (stdio, streamable HTTP; implemented) and the Imperium MCP server (`imperium mcp-serve`, the session-level [Claude harness](20-claude-harness.md) and its `--tools` bridge for Imperium's own tools) lives in `imperium-cli`; the engine side of the bridge is `imperium-engine`'s `bridge` module |
| `imperium-extensions` | Plugin, skill, hook, and slash-command loading; Claude Code plugin compatibility (implemented) |
| `imperium-workflow`   | Workflow packs, context retrieval, load manifests, documentation pointers (implemented) |
| `imperium-policy`     | What agents may do: permission rules and modes, the safety floor, agent profiles, and the layering of workflow packs (implemented); prompt assembly stays in `imperium-engine` |
| `imperium-memory`     | Global memory and environment `Memory.md`: entries, index, search, redaction, migration of older project memory (implemented) |
| `imperium-config`     | Layered config, schema validation, secret store (keychain, encrypted file, in-memory)  |
| `imperium-automation` | Schedule triggers (cron, interval, one-shot), missed runs, run history (implemented); project agents (planned) |
| `imperium-github`     | Failing pull request checks through `gh`, auto-fix worktrees, commit and push (implemented); `HttpGitHub`, a GraphQL and REST client pinned to `api.github.com` with a `Secret` token, scrubbed errors and rate-limit handling (implemented; the daemon's call sites use it and `pull_requests.*` reads its status: [pull-request card plan](plans/pr-card.md), stages 1 to 3) |
| `imperium-github-fake` | A fake GitHub API server for tests (dev-only: only `[dev-dependencies]` may name it, never in a release build; [plan](plans/pr-card.md#stage-1-github-client-and-fake-github-server)) |
| `imperium-media`      | Image (OpenAI-compatible, ComfyUI), speech, and transcription tools (implemented) |
| `imperium-pets`       | Pixel Pets v3 packages and the behaviour arbiter (implemented); WASM behaviours run in the desktop's mod frames, not here ([16](16-pets.md)) |
| `imperium-pty`        | A shell on a pseudo terminal on all three OSes (`portable-pty`), a 1 MiB output ring and, on Linux, the per-terminal supervisor the daemon re-runs as its hidden `pty-supervisor` subcommand; no daemon wiring yet ([terminal and file panes plan](plans/terminal-file-panes.md), stage 1) |
| `imperium-core`       | The daemon (`imperium-core` binary, formerly `imperiumd`), also a library for tests |
| `imperium-cli`        | `imperium` binary: CLI and TUI client, `imperium mcp-serve` ([20](20-claude-harness.md)), and the headless `imperium run` for evaluations ([03](03-agent-engine.md#running-evals-through-the-harness-implemented)) |

Ratio, the agent-harness language, lives in its own repository,
[Ratio](https://github.com/Densetsu152637/Ratio); Imperium will depend on its engine and
implement the host side ([roadmap item 58](12-roadmap.md#next-in-order)).

`imperium-api` depends on no other Imperium crate, and crates that only share vocabulary
depend on it rather than on each other: `imperium-tools`, `imperium-mcp`, `imperium-web`, and
`imperium-media` take `ToolSpec` from it instead of `imperium-models`; `imperium-config` takes
`Pricing`; `imperium-memory` takes the front-matter parser instead of `imperium-extensions`;
and `imperium-local` implements its `LocalModels` trait without depending on `imperium-engine`.
Likewise `imperium-web`, `imperium-media`, and `imperium-mcp` implement its `ToolProvider` trait,
so `imperium-engine` does not depend on `imperium-web` or `imperium-mcp` (it reaches `imperium-mcp` only
through `imperium-extensions`, for plugins' server definitions); the daemon builds the providers.
`imperium-engine` does depend on `imperium-media`, for the attachments it stages and describes. There is
no separate sandbox, telemetry or remote-transport crate: isolation tiers live in `imperium-tools`, usage
and cost are recorded by the engine and the daemon, and remote hosts are reached over an SSH tunnel.
The moved items are imported from `imperium-api` directly; no crate re-exports them at an old
path. `imperium-policy` depends on `imperium-api` and `imperium-workflow` (and on no other
Imperium crate, `imperium-config` included: it takes plain inputs); `imperium-tools`,
`imperium-engine`, `imperium-core`, `imperium-web` and `imperium-cli` depend on it.

Secrets go through the `SecretStore` trait in `imperium-config` (`load`, `store`, `delete`):
`KeychainSecrets` uses the OS keychain, `FileSecrets` keeps them in an encrypted file,
`AutoSecrets` uses the keychain and falls back to that file where there is none
([10](10-configuration.md#encrypted-file-fallback-implemented)), and `MemorySecrets` keeps them
in memory for tests. The daemon builds one `Arc<dyn SecretStore>` at start and every key lookup
(endpoint and media keys, web search keys, MCP tokens, the Hugging Face token) reads through it;
environment variables such as `HF_TOKEN` still take precedence where they did before.

### Swappable implementations

Each replaceable subsystem is a trait with built-in implementations chosen by name, so a new
one is added without editing its callers:

| Seam | Trait | Built-ins | Chosen by |
| ---- | ----- | --------- | --------- |
| Secrets | `SecretStore` (`imperium-config`) | `AutoSecrets` (`KeychainSecrets`, `FileSecrets`), `MemorySecrets` | The daemon at start |
| Local runtimes | `RuntimeBackend` (`imperium-local`) | `LlamaCpp`, `Vllm` | First backend that accepts the model format and is available; `Supervisor::with_backends` sets the list |
| Web search | `SearchProvider` (`imperium-web`) | `Searxng`, `Brave`, `Tavily`, `Kagi`, `Exa` | `web.search` through `SearchProviders::register` / `build` |
| Image generation | `ImageGenerator` (`imperium-media`) | `OpenAiImages`, `ComfyUi` | `media.image.backend` through `ImageGenerators::register` / `build` |
| Local model leasing | `LocalModels` (`imperium-api`) | The `imperium-local` supervisor | The daemon at start |
| Local memory residency | `ResidencyPolicy` (`imperium-local::residency`) | `Standard` (the arbiter's rules), `CorePolicy` (`imperium-core::residency`) | The daemon's residency controller installs `CorePolicy` with `Supervisor::set_policy` |
| GitHub | `GitHub` (`imperium-github`) | `GhCli` (auto-fix today), `HttpGitHub` (the card, from stage 2) | The daemon at start |
| Model clients | `ChatClient` (`imperium-models`), with an optional `Tokenizer` | `OpenAiCompatible`, `LlamaCpp` | `endpoints.<id>.kind` (default `openai`) through `ChatClients::register` / `build`, given to each session as `SessionConfig::clients` |
| Agent tools beyond the agent's own | `ToolProvider` (`imperium-api`) | `WebTools` (`imperium-web`, with the browser), `MediaTools` (`imperium-media`), `McpTools` (`imperium-mcp`) | The daemon for each session, as `SessionConfig::tools` |

### Local memory residency

`imperium-core` owns every decision about which local models and KV caches are in memory
(roadmap item 16; [04](04-models-and-routing.md#residency-controller-implemented)). Its
`ResidencyController` (`crates/imperium-core/src/residency.rs`) runs one plan loop per local
runtime and is the policy calls consult; the `imperium-local` supervisor executes:

```text
            signals (slot freed, agent waiting, settings changed, every 15 s)
 supervisor ---------------------------------------------> ResidencyController (core)
     ^                                                        | plan(loaded, idle_unload_s)
     | execute(Action, Reason): Load, Unload, SaveSlot,       | refreshed_budget(probe)
     |   RestoreSlot, ResizePool, SetBudget                   | CorePolicy::decide (calls)
     +--------------------------------------------------------+
 every move --> Supervisor::moves --> model.residency (clients), model.swapped_out/in
```

The supervisor has no timer and spawns nothing that unloads; what it still does without being
told is what a call in flight needs (load its model, evict what the policy says, restore the
caller's saved context, restart a pool that must grow or a model the GPU rule moves once it
has drained), each reported with its reason.

Key dependencies (to confirm during phase 0): `tokio`, `axum` / `tokio-tungstenite`,
`serde`, `reqwest`, `rmcp` (official Rust MCP SDK), `chromiumoxide`, `keyring`, `sysinfo`,
`nvml-wrapper`, `russh`, `gguf` metadata parsing, `tracing` + `opentelemetry`.

## Repository layout

```text
Cargo.toml              Rust workspace
crates/                 Rust crates listed above
apps/desktop/           Tauri 2 shell (src-tauri/) and React app (src/, in feature folders)
pets/                   Bundled default pet packs
mods/                   Bundled UI mods (opt-in), such as imperium-nursery
packages/protocol/      Generated TypeScript protocol types
packages/mod-sdk/       UI mod SDK (@imperium/mod-sdk)
examples/mods/          Example UI mods
runtimes/               Pinned runtime versions (llama.cpp ref, vLLM version)
policy/                 Bundled default policy pack (pinned snapshot)
scripts/                Check, build, and CI helper scripts
docs/                   These documents
```

JavaScript uses a pnpm workspace (`package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` at
the root). Python uses `uv` with a committed `uv.lock`.

## Data locations

Resolved with platform conventions (`directories` crate):

| Kind      | Linux                         | Windows                          | macOS                                     |
| --------- | ----------------------------- | -------------------------------- | ----------------------------------------- |
| Config    | `~/.config/imperium`          | `%APPDATA%\Imperium\config`      | `~/Library/Application Support/Imperium` |
| Data      | `~/.local/share/imperium`     | `%LOCALAPPDATA%\Imperium\data`   | `~/Library/Application Support/Imperium`  |
| Cache     | `~/.cache/imperium`           | `%LOCALAPPDATA%\Imperium\cache`  | `~/Library/Caches/Imperium`               |

Data holds the daemon's endpoint and lock files and, later, session transcripts (SQLite +
append-only JSONL), the cost ledger, installed plugins, runtime packs, and policy snapshots.
Setting `IMPERIUM_HOME` puts all three under one root as `config/`, `data/`, and `cache/`.

## Concurrency and failure

- **One process.** `imperium-core` hosts everything; model runtimes and sidecars are the only
  child processes.
- **Task per agent (implemented).** The daemon runs one multi-threaded Tokio runtime with a
  worker per core. Every agent (a session's root and each subagent) is its own task on it, so
  agents run in parallel across cores, a busy agent cannot block the transport, and an idle
  agent waiting on the model, the user, or its subagents uses no CPU. Agents in a session
  share state through `Arc` and short-held locks; the transport talks to sessions over
  channels.
- Within a session, each agent turn, tool call, and runtime request is a task with a
  cancellation token. Cancelling a parent cancels its subtree unless a child is marked
  `detached`.
- Runtime crashes surface as events; in-flight requests on that runtime fail with a retryable
  error and the router may re-route them.
- Session state is journaled before each model call, so a daemon restart resumes sessions at
  the last completed step.
