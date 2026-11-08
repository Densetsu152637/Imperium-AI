# 05 — Tools and isolation

## Built-in tools

| Tool group  | Tools                                                     | Notes                                            |
| ----------- | --------------------------------------------------------- | ------------------------------------------------ |
| Files       | `read_file`, `write_file`, `edit_file`, `list_dir` (implemented); glob and grep planned | Exact-match unique edits; reads numbered and paged with `offset`/`limit` |
| Execution   | `run_command` (implemented); background processes planned | Runs in the project root via `cmd /C` (the line passed verbatim) or `sh -c`; default timeout 120 s; returns exit code, stdout, stderr |
| Web         | `web.search`, `web.fetch`                                 | Search providers: SearXNG (self-hosted), Brave, Tavily, Kagi; pluggable |
| Browser     | `browser.open`, `browser.read`, `browser.act`, `browser.screenshot` | Chromium over CDP; visible or headless |
| Agents      | `spawn_agent`, `await_tasks`, `send_to_task`, `cancel_task` | See [agent engine](03-agent-engine.md)        |
| Media       | `media.generate`, `media.transcribe`                      | See [media](09-media-generation.md)              |
| Interaction | `ask_user`                                                | Pauses the agent until the user answers          |

Implemented in phase 8 (`imperium-web`): `web_fetch` reads an http(s) page as text (title,
final URL, scripts and markup removed, long pages in chunks via `offset` and `limit`; non-text
content is refused) and `web_search` queries the configured provider (`web.search` =
`searxng` with `web.searxng_url`, or `brave` or `tavily` with `web.api_key` from the keychain)
and returns numbered results. Each provider implements the `SearchProvider` trait and is built
from its `web.search` name by `SearchProviders`, so adding one is an implementation and a
`register` call; unknown names leave search off. Agents are told to cite the sources they use
with their URLs and to treat web content as data. Searching runs without asking; `web_fetch` asks unless the host
is in `web.allow_domains` (subdomains included) or the user allowed fetching for the session.
Kagi and other providers, caching, and robots handling are later work.

Browser tools (`browser_open`, `browser_read`, `browser_click`, `browser_type`) drive Chrome,
Chromium, or Edge over the DevTools Protocol, found in the usual install locations or set with
`web.browser_path`. The browser runs with its own profile in `<data>/browser` (never the user's
profile, so no saved logins), headless by default (`web.headless`), started on first use and
shared by a session's agents. Opening a page follows the `web_fetch` domain rule; clicking and
typing always ask unless allowed for the session. After every action the session emits a
`browser_view` event with the page's URL, title, and a JPEG screenshot, which the desktop's
Browser panel shows, so the user sees what the agent sees. Screenshots for vision models,
tabs, downloads, and a visible pane the user can take over are later work.

Implemented in phase 1 (`imperium-tools`): paths resolve inside the project folder and may not
escape it through `..`, absolute paths, or symbolic links (tier 0). Reads and listings run
without asking; writes, edits, and commands ask the user, who can allow once, allow that tool
for the rest of the session, or deny. On timeout the shell is killed, but processes it started
may keep running until the OS sandbox tier adds process-tree cleanup.

All tools are described by JSON Schema and are also exposed over an MCP server endpoint, so
external agents can use Imperium's tools.

## MCP

**Implemented (phase 5, `imperium-mcp`):** each session starts its MCP servers before its
first turn: stdio processes (newline-delimited JSON-RPC, stopped with the session) and
streamable-HTTP endpoints (JSON or SSE responses, `Mcp-Session-Id` kept), using MCP revision
`2025-06-18`. Tools are listed (following pagination) and offered to the root and subagents as
`mcp__<server>__<tool>`. A call asks for approval unless the server's `allow` list names the
tool (or `"*"`), or the user allowed it for the session; the result's text, resource text, and
descriptions of other content are returned to the model, and a tool-reported error fails the
call. Servers that fail to start are reported as session notices; the session carries on
without them. Server pings are answered; other server requests (sampling, roots,
elicitation) are refused for now.

Servers come from `[mcp.servers.<name>]` in the user configuration (a `command` with `args`
and `env`, or a `url` with `headers` and an optional `bearer_token = { secret = "<name>" }`),
and from a project's `.mcp.json` (the Claude Code format, with `${VAR}` and `${VAR:-default}`
expansion) only when the project folder is listed in `mcp.trusted_projects`, since a cloned
repository could otherwise run any command. A configured server overrides a project server of
the same name.

```toml
[mcp.servers.github]
url = "https://api.githubcopilot.com/mcp/"
bearer_token = { secret = "github" }
allow = ["get_issue", "list_pull_requests"]

[mcp.servers.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "D:/Notes"]

[mcp]
trusted_projects = ["D:/Git Repositories/Imperium-AI"]
```

A stdio server's tokens go in `secret_env`, environment variables read from the keychain when
the server starts, so the config file never holds them:
`secret_env = { SLACK_BOT_TOKEN = { secret = "mcp.slack.bot-token" } }`. A server may also
record `catalog = "<id>"`, the connector it was added from, and `enabled = false` keeps it
configured without starting it. Plugins found in the plugin folders can be turned off by name
with `extensions.disabled_plugins`.

### Connectors: managing MCP servers from the app (implemented)

The desktop app's **Connectors** window (title bar, or Settings → Connectors) lists installed
MCP servers, plugins, and skills, and offers a directory of popular servers to add. It works
through these daemon methods, which write `[mcp.servers.<name>]` in the user config with
`toml_edit` the way `config.set` does: comments, formatting, and unknown keys are kept, the
whole configuration is validated after the edit, and a change that would make it invalid is
undone. Server names are 1–40 lowercase letters, digits, `_` or `-`.

| Method | Does |
|---|---|
| `mcp.list` | The servers in the user config with their last known status (`untested`, `connected` with the tool names, `error` with the reason, or `disabled`) and any secrets missing from the keychain; the plugins in `<config>/plugins` and `extensions.plugin_dirs` (with skill, command, hook, and server counts and whether each is enabled); and the user's and plugins' skills. |
| `mcp.catalog` | The connector directory. |
| `mcp.install` | Adds a directory entry under a name, filling its settings from the form's values. |
| `mcp.save` | Adds or replaces a server from its full settings (custom servers and *Configure*). |
| `mcp.set_enabled` | Turns a server on or off (`enabled = false`). |
| `mcp.remove` | Removes a server; optionally deletes the secrets it referred to. |
| `mcp.test` | Starts the server as a session would, lists its tools, and stops it; the result is what `mcp.list` shows until the next test. |

Secret values (tokens, keys, connection strings with passwords) are sent once with
`mcp.install` or `mcp.save`, stored in the keychain (named `mcp.<server>.<field>`), and written
to the config only as `{ secret = "<name>" }`; no method returns them. New sessions start the
servers as saved, without restarting the daemon; running sessions keep the servers they
started with. Turning a plugin on or off goes through `config.set` and applies after a restart.

The directory is data, not code: `crates/imperiumd/catalog/connectors.json` is built in, and
`<config>/connectors.json` (same shape, `{ "entries": [...] }`) adds entries or replaces
built-in ones with the same `id`. An entry has a name, description, category, publisher,
homepage, a [Lucide](https://lucide.dev) icon name, and either a `command` with `args` and
`env` or a `url` with `headers` and `bearer`, plus the `fields` the user fills in (`text`,
`path`, `path_list`, `url`, or `secret`, each optionally required, with a placeholder and a
default). `{{field}}` in an argument, an `env` or header value, or the URL is replaced by the
field's value; an argument that is exactly `{{field}}` of a `path_list` becomes one argument
per line, and an optional field left empty drops its argument or variable. A `secret` field
may only fill a whole `env` value (it becomes `secret_env`) or `bearer`. The built-in
directory has Filesystem, Git, GitHub (hosted), Fetch, Memory, PostgreSQL, SQLite, Slack,
Brave Search, Playwright, Notion, Context7, Sequential Thinking, and Time.

Planned:

- Client support for stdio and streamable-HTTP MCP servers, including tools, resources,
  prompts, and server-initiated sampling (routed through Imperium's router and budgets).
- Servers are configured globally, per project, or by plugins; each has its own permission
  scope and runs inside the session's isolation tier when it is a local process.
- OAuth flows for remote MCP servers open in the system browser; tokens go to the keychain.

## Permissions

Each tool call is checked against the effective permission set (config → policy → session →
user prompt). Modes:

- `ask` — prompt the user (default for writes, exec, network, and anything outside the
  environment).
- `allow` / `deny` — rules by tool, path glob, command prefix, domain, or MCP server.
- `auto` — a classifier model approves routine actions and escalates unusual ones.

Decisions are logged in the session journal.

## Isolation tiers

The UI always shows the active tier for each session and agent.

**Implemented (phase 7):** `isolation.tier` chooses where `run_command` (and auto-fix and
scheduled sessions' commands) run, and each session reports its tier (`session.list`,
`session.create`, and the session header). `policy` is tier 0. `os` runs commands under
bubblewrap on Linux (the filesystem read-only except the workspace and a private `/tmp`, its own
process namespace, `--unshare-net` without network) or Seatbelt on macOS (`sandbox-exec`: writes
only to the workspace and temporary folders, network denied without network); on Windows the
tier is not available yet and commands fail rather than running unsandboxed. `container` runs
commands with `isolation.container_runtime` (default `docker`) in `isolation.container_image`
(default `ubuntu:24.04`) with only the workspace mounted at `/workspace`, removed afterwards,
and `--network none` without network. `isolation.network` (default `true`) applies to both.
The file tools stay tier 0 (they run in the daemon, confined to the workspace). Not yet: policy
forcing tiers per model or agent, GPU flags for containers, Windows AppContainer, and a default
dev image.

| Tier            | Boundary                                                                     | Protects against                                    | Does not protect against                           |
| --------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | -------------------------------------------------- |
| 0 — Policy      | Daemon refuses built-in file tools outside allowed roots                     | Model mistakes with built-in tools                  | Code run through `exec`, which has the user's full rights |
| 1 — OS sandbox  | Linux: Landlock + seccomp (bubblewrap); macOS: Seatbelt profile; Windows: AppContainer / restricted token (best effort) | Executed code reaching files outside allowed roots, optional network block | Kernel or sandbox escapes; Windows coverage is partial |
| 2 — Container   | Docker/Podman container with only the workspace mounted                      | Executed code affecting the host                    | Container escapes; mounted folders are writable    |
| 3 — Remote      | Separate machine over SSH                                                    | Anything on the local machine                       | The remote machine itself                          |

- Tier 0 is always on. Higher tiers are opt-in per session or forced by policy (for example,
  "always use a container for `exec` from subagents running hosted models").
- Container images are user-selectable; a default dev image ships with common toolchains.
- GPU access inside containers uses the NVIDIA container toolkit / ROCm devices where available.

## SSH hosts

**Implemented (phase 7):** `imperium remote connect <destination>` (a `user@host` or a `Host`
from `~/.ssh/config`) runs `imperium daemon endpoint` on the host over SSH (`BatchMode`, so
keys come from the SSH agent and no password is ever asked for or stored), starting the
host's daemon with `imperium daemon start` if it is not running, then forwards a free local
port to the remote daemon's loopback port (`ssh -N -L`, with `ExitOnForwardFailure` and
keep-alives) until Ctrl+C. The forwarded endpoint, with the remote daemon's per-launch token,
is recorded in `<data>/remotes/<name>.json` (readable only by the user on Unix) and removed on
disconnect; `imperium --remote <name> <command>` talks to the remote daemon, which owns the
host's files, processes, models, and GPUs (its own `models.folders`, runtimes, and isolation
apply). Not yet: installing `imperiumd` on the host, Unix-socket binding, `~/.ssh/config`
discovery in the app, choosing a remote in the desktop app (its connection form accepts the
recorded URL and token), and cross-host routing.

- Hosts are configured from `~/.ssh/config` entries or added manually; keys stay in the user's
  SSH agent. Imperium never stores SSH passwords.
- On first connect, Imperium installs or updates a matching `imperiumd` on the host (with
  consent), starts it bound to a Unix socket, and forwards that socket.
- The remote daemon owns the remote files, processes, models, and GPUs. The local router can
  still send remote subagents to hosted endpoints or to local-machine models when the user
  allows cross-host routing.
- Model folders, runtimes, and isolation tiers on a host are configured on that host.
