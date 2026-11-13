# 05 — Tools and isolation

## Built-in tools

| Tool group  | Tools                                                     | Notes                                            |
| ----------- | --------------------------------------------------------- | ------------------------------------------------ |
| Files       | `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep` (implemented) | Exact-match unique edits; reads numbered and paged with `offset`/`limit`; [searches](#glob-and-grep-implemented) honour `.gitignore` and page with `offset` |
| Execution   | `run_command`, `command_output`, `command_stop` (implemented) | Runs in the project root via `cmd /C` (the line passed verbatim) or `sh -c`; default timeout 120 s; returns exit code, stdout, stderr; `background: true` [returns a process id at once](#background-commands-implemented) |
| Web         | `web.search`, `web.fetch`                                 | Search providers: SearXNG (self-hosted), Brave, Tavily, Kagi, Exa; pluggable. Pages are [cached](#fetch-cache-and-robotstxt-implemented) and `robots.txt` is honoured |
| Browser     | `browser.open`, `browser.read`, `browser.act`, `browser.screenshot` | Chromium over CDP; visible or headless |
| Agents      | `spawn_agent`, `await_tasks`, `send_to_task`, `cancel_task` | See [agent engine](03-agent-engine.md)        |
| Media       | `media.generate`, `media.transcribe`                      | See [media](09-media-generation.md)              |
| Interaction | `ask_user` (implemented)                                  | Pauses the agent until the user answers          |
| App config  | `set_model_card`, `install_extension`, `uninstall_extension` (implemented) | Change the user's Imperium config through the daemon, journalled and undoable; see [06](06-extensibility.md#agents-manage-the-app-config-implemented) |

Implemented in phase 8 (`imperium-web`): `web_fetch` reads an http(s) page as text (title,
final URL, scripts and markup removed, long pages in chunks via `offset` and `limit`; non-text
content is refused) and `web_search` queries the configured provider (`web.search` =
`searxng` with `web.searxng_url`, or `brave`, `tavily`, `kagi` or `exa` with `web.api_key` from the
keychain, never from the settings file)
and returns numbered results. Each provider implements the `SearchProvider` trait and is built
from its `web.search` name by `SearchProviders`, so adding one is an implementation and a
`register` call; unknown names leave search off. Agents are told to cite the sources they use
with their URLs and to treat web content as data. Searching runs without asking; `web_fetch` asks unless the host
is in `web.allow_domains` (subdomains included) or the user allowed fetching for the session.

### Fetch cache and robots.txt (implemented)

**Search providers.** `kagi` (the Kagi Search API, `Authorization: Bot <token>`; its related-search
entries are not results) and `exa` (`x-api-key`, with a short text excerpt per result) join
`searxng`, `brave` and `tavily`. A key provider is built only when `web.api_key` names a keychain
secret that exists; otherwise search stays off with a warning. Keys are sent only as request
headers, are redacted from `Debug` output, and never appear in error messages.

**Cache.** `web_fetch` keeps the text of pages it reads in `<cache>/web`, one file per URL
(fragment removed; readable by the owner only on Unix), so a page read again, or read in later
`offset` chunks, is not requested again, including by another session. `web.cache_ttl_s`
(default 900; `0` turns the cache off; at most 30 days, larger values are refused) is the longest
an entry lives, and a response's `max-age` or `s-maxage` can only shorten it. `web.cache_max_mb`
(default 50; `0` turns the cache off) caps the folder: after every write, and when a session
opens the cache, expired files and then the oldest files are deleted until it fits, and a page
larger than the cap is not stored. The cap is *eventually* enforced: sessions writing at the same
moment can leave the folder slightly over it until the next write trims it, and a temporary file
of a write in flight is left alone until it is ten minutes old. Only sharable responses are
stored: a `200` that has no `Set-Cookie` or `WWW-Authenticate`, no `Cache-Control` of `no-store`,
`private` or `no-cache` (or a zero or unreadable `max-age`), no `Vary` of `*`, `Cookie` or
`Authorization`, and a URL with no credentials and no query parameter whose name looks like one
(`token`, `key`, `sig`, `auth`, `secret`, `pass`, `session`, `code`, ...). `web_fetch` sends no
cookies and no credentials of its own, so a cached page is what any visitor sees; the rules above
keep a page that was personalised anyway out of the cache that other sessions share. Browser
pages and search results are not cached.

**Redirects.** The HTTP client never follows a redirect itself. `web_fetch` follows up to ten by
hand and checks every hop, by its full URL, before requesting it: against the host's
`robots.txt`, and against the approval rule (a redirect is followed only to the host that was
asked for or to a host in `web.allow_domains`, otherwise the call fails and names the target so
the user can approve that URL). Search providers get no redirects, so an API key is never
replayed to another host (a SearXNG instance that redirects must be configured with its final
URL).

**robots.txt.** With `web.robots = "respect"` (the default), `web_fetch` reads the host's
`/robots.txt` (up to five redirects, 512 KiB, a leading byte-order mark ignored) before the cache
is read and before every request, and refuses a URL it disallows for the `Imperium` agent
(RFC 9309: a group whose `User-agent` equals the product token `imperium`, or is a prefix of
it, ignoring case, wins over `*`; the longest matching `Allow` or `Disallow` decides, `Allow` on
a tie; `*` and a trailing `$` work in patterns; percent-escapes are normalised on both sides). The
refusal names the URL and the setting. Rules are held per host in memory for an hour. A missing
file (any 4xx) allows everything. When the file cannot be read (a 5xx answer or no connection),
`web.robots_unreachable` decides: `allow` (the default, fail open: a flaky server does not stop
the user's work) or `deny` (fail closed, as RFC 9309 suggests); the failure is remembered for a
minute. `Crawl-delay` is ignored: `web_fetch` reads one page per call at the agent's request
and never crawls, so there is nothing to pace. `web.robots = "off"` skips the check. The check
covers `web_fetch` only: browser tools show the user a real page and follow the domain rule, and
the user can always open a page themselves.

The domain rule compares the host the URL parser reads (the same parser `web_fetch` connects
with), lower-cased, in ASCII form, without a trailing dot, so a backslash, userinfo, case or an
internationalised name cannot make one host look like an allowed one; only http(s) URLs qualify.

None of this changes the permission model: `web_fetch` still asks unless the host is in
`web.allow_domains` or fetching is allowed for the session, the safety floor and permission modes
apply as before, and an agent profile that locks an agent to local files still removes the web
tools.

**Browser tabs and downloads (not built, design).** The browser is one page. Next stage: the
`browser` module tracks targets (`Target.getTargets`, `Target.attachToTarget` with flattened
sessions) so `browser_open` takes a `new_tab` flag, `browser_tabs` lists tabs (id, URL, title)
and `browser_switch` and `browser_close` act on one, each tab keeping its own `browser_view`
mirror and the user's takeover applying to the active tab. Downloads are denied by default
(`Browser.setDownloadBehavior` with `behavior: deny`); when a click starts one the agent is told,
and `browser_download` fetches the file into the session's scratch folder through the same
approval as a write, size-capped and never executed. Both need protocol additions (tab fields on
`browser_view` and `session.browser_act`), so they are a separate protocol minor.

Browser tools (`browser_open`, `browser_read`, `browser_click`, `browser_type`) drive Chrome,
Chromium, or Edge over the DevTools Protocol, found in the usual install locations or set with
`web.browser_path`. The browser runs with its own profile in `<data>/browser` (never the user's
profile, so no saved logins), headless by default (`web.headless`), started on first use and
shared by a session's agents. Starting it waits for the browser's own start-up to finish (a
fresh profile can take it 10 to 45 s on a slow machine; the wait is up to 90 s) so that the first
page opens at normal speed. Opening a page follows the `web_fetch` domain rule; clicking and
typing always ask unless allowed for the session. After every action the session emits a
`browser_view` event with the page's URL, title, and a JPEG screenshot, which the desktop's
Browser panel shows, so the user sees what the agent sees. The user can take the browser over
from that panel ([07](07-desktop-ui.md#preview-and-browser-panels-implemented)):
`session.browser_control` sets who drives, `session.browser_act` carries the user's click, typing,
key, scroll, and navigation, and while the user drives every `browser_*` call is refused before
any approval with a message saying so (a call already waiting for the browser re-checks once it
holds it). Giving the browser back restores the agent's tools. Screenshots for vision models,
tabs, and downloads are later work; the browser is one page.

### Live preview (implemented)

`preview_open` shows a page the project serves in the desktop's Preview panel, with either a
loopback `url` or a `command` and the `port` it listens on (`preview_close` closes it).

- **Commands** run in the session's background process table (the one background
  `run_command` uses, so `command_output` and `command_stop` see the server), under the
  session's isolation tier. A preview command is judged exactly as `run_command` on that
  command: its allow, ask, and deny rules, the "always allow" rule an approval offers (a
  `run_command` prefix, never a blanket `preview_open` rule), the safety floor (a destructive
  command still asks in an unattended session started with `run_command` pre-approved), and
  permission modes (plan mode refuses it). Under `required-before-action` it is also refused
  until the required context is loaded. Readiness is the port accepting connections on the
  loopback address (60 s); the command's output is returned if it exits early; a port already
  in use is refused rather than shown. Opening another preview, `preview_close`, and the
  session stopping or being deleted end the server and everything it started (a process group
  on Unix, a Job Object that kills on close on Windows). If the daemon itself is killed
  abruptly, a server on Unix may outlive it; Windows ends it with the daemon.
- **Where commands are refused**, before any approval: sessions in a remote environment (they
  are not offered the preview tools at all), the container tier, and the OS tier without
  network, because the app could not reach the port.
- **URLs** are limited to `localhost` and `127.0.0.1` over http or https with no user name or
  password, parsed as a browser would (so `localhost.evil.com`, `localhost@evil.com`, and
  `127.0.0.1:80@evil.com` are refused). `[::1]` is not accepted: the window's content security
  policy cannot name an IPv6 literal, so it could not be shown. The reason for the rule: the
  page runs scripts inside the app window next to the daemon's controls, so remote pages are
  never shown there; the agent's own browser is the way to read remote pages. A URL asks first,
  because another loopback port may be something private on this machine, unless its port is
  the one this session's own preview command started (and that is still running; a page on
  that port keeps the server, any other page stops it). Closing the preview never asks. If the
  server ends (`command_stop`, a crash), the next preview call closes the dead page.
- **Hooks** match the tool actually called: `tool.before` and `tool.after` hooks see
  `preview_open`, not `run_command`, for a preview command. Permission rules, suggestions, the
  floor, and modes are the `run_command` ones; hooks are not rerouted.
- The dev server's own hot reload keeps the page current; Imperium only shows it.

Implemented in phase 1 (`imperium-tools`): paths resolve inside the project folder and may not
escape it through `..`, absolute paths, or symbolic links (tier 0). Reads and listings run
without asking; writes, edits, and commands ask the user, who can allow once, allow that tool
for the rest of the session, or deny. [Path grants and rules](#permissions) open paths outside
the folders and change what asks; [permission modes](#permission-modes-implemented) change it
for a whole session. On timeout the shell is killed, but processes it started may keep running
until the OS sandbox tier adds process-tree cleanup (background commands are stopped with their
whole tree).

All tools are described by JSON Schema and are also exposed over an MCP server endpoint, so
external agents can use Imperium's tools.

### Glob and grep (implemented)

`glob(pattern, path?, max_results?, offset?, include_ignored?)` lists files by name and
`grep(pattern, path?, glob?, case_insensitive?, context?, max_results?, offset?,
include_ignored?)` searches their contents with a regular expression (Rust `regex` syntax).
Both are read-only, so they never ask (an ask rule still asks, a deny rule still refuses), and:

- `path` (default the working directory) is checked like `list_dir`'s: inside a folder, a
  project worktree, or a grant, after resolving `..` and links; `grep` also takes one file.
- The walk (the `ignore` crate, as ripgrep walks) skips `.git` and what `.gitignore`, `.ignore`,
  and git's exclude files leave out, also outside a git repository; `include_ignored` lists
  those too. Hidden files are included. Symbolic links are not followed into folders, and a link
  to a file counts only when its target is inside the workspace.
- Files an ask or deny rule covers, for the tool itself or for `read_file`, are left out (a
  search cannot ask per file), and the output says how many; so `deny * **/.env` or
  `ask read_file .env` keeps secrets out of searches too, and `read_file` still asks for one.
- A pattern without `/` matches the file name at any depth (`*.rs`); one with `/` matches the
  path relative to the searched folder (`src/**/*.ts`); `{a,b}` gives alternatives. `grep`'s
  `glob` filters the files searched the same way.
- `grep` prints `path:line:text` per matching line and `path-line-text` for up to 10 `context`
  lines around each, with `--` between groups; it skips binary files (a NUL byte in the first
  8 KiB) and files over 10 MiB, and cuts lines at 300 characters.
- Results are sorted by path, `max_results` (default 100, at most 1000) per call, and the
  output ends with the `offset` to page on; a walk stops after 200,000 entries and says so,
  and one call returns at most 100,000 characters.

Remote runtimes do not have them yet (the model is told to use `run_command` with `find` or
`grep` there).

### Background commands (implemented)

`run_command` with `background: true` starts the command under the session's isolation tier
(the same sandbox, folders, grants, and permission rules as a foreground command, and the same
approval: a background command is a command) and returns its id (`p1`, `p2`, …) at once, for
servers and watchers. `command_output(id, since?)` returns the process's status (`running for
N s`, `exited with code N`, or `stopped`) and what it wrote to standard output and error since
the last read, or from byte `since`, at most 30,000 bytes per call with the offset to continue
from; `command_stop(id)` kills it with every process it started and waits up to 10 s for it to
end. Everything a command starts stays reachable after the command itself exits (`sh -c 'x &'`,
`start /b x`): on Unix the command leads its own process group, which is killed as a whole; on
Windows it runs in a Job Object that kills every process in it when closed (and its process
tree is killed first, for a child started before it joined the job); a container-tier
command's container (named uniquely per daemon, session, and command) is removed with
`docker rm -f`. A command that exited but left processes running says so in its status, still
counts as running, and is never dropped from the table while they run. On Unix, a descendant
that leaves the process group on purpose (`setsid`, `setpgid`, daemonising tools) escapes
`command_stop` on the policy tier and on macOS; the Linux OS tier's process namespace and the
container tier still end it with the command. Reading and stopping touch only what the session started, so they
count as reads and never ask.

A session runs at most 8 background commands at once and keeps the last 32 (finished ones are
dropped oldest first); each keeps the last 1 MiB of output and says how much was dropped before
it. Its processes are killed when the session is stopped, archived, or deleted, when its handle
is dropped, and when the daemon shuts down (which waits up to 5 s for the kills). Remote
runtimes do not run background commands yet.

### `ask_user`

The root calls `ask_user(questions[])` once with every question it has; each has `question`,
optional `options`, and a `recommended` answer. The session emits `questions_asked`
(`request_id`, `questions`) and the root waits without model calls, while subagents it already
spawned keep running. Any client answers with `session.answer` (`session_id`, `request_id`,
`answers`, one string per question in order); the session then emits `questions_answered` so
other clients close the prompt. A blank answer means "use the recommendation". If nobody answers within 10 minutes (applied
whether or not a client is attached), the recommendations are taken and the root is told so. The root gets
back each question with its answer. The session status is `needs_attention` while a batch is
pending.

- CLI: `imperium chat` prints each question and its numbered options; Enter takes the
  recommended answer, a number picks an option, other text is the answer.
- Desktop: the session shows a form with the options as buttons and the recommendation
  prefilled; Send answers calls `session.answer`.
- Unattended sessions (project agents, scheduled tasks) answer with the recommendations
  immediately, since nobody is watching.

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

The directory is data, not code: `crates/imperium-core/catalog/connectors.json` is built in, and
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

The client speaks to stdio and streamable-HTTP servers and offers their tools (implemented).
Planned:

- Resources, prompts, and server-initiated sampling (routed through Imperium's router and
  budgets).
- Per-project server configuration, and local servers run inside the session's isolation tier.
- OAuth flows for remote MCP servers open in the system browser; tokens go to the keychain.

## Permissions

Without rules, reads and listings run, and writes, edits, commands, and the tools whose
providers ask (MCP tools, `web_fetch` outside the allowed domains, browser actions, memory
saves) ask the user, who can allow once, allow the tool for the rest of the session, or deny.
**Path grants** and **permission rules** (implemented) change that, at two scopes: global
(`[permissions]` in the user config) and per [environment](14-projects-and-automation.md#environments-implemented)
(saved with it in `environments.json`). The desktop app edits both
([permissions](07-desktop-ui.md#permissions-implemented)); the daemon's methods are
`permissions.get` (the global set and the presets), `permissions.set` (replace the global set),
`permissions.add_rule` (one rule into an environment, a session's environment, or the global
set, replacing a rule for the same tool and pattern), and `permissions` on
`environments.create` / `environments.update`.

```toml
[permissions]
grants = [
  { path = "~/.cargo", mode = "read_write" },
  { path = "~/.rustup", mode = "read" },
]
rules = [
  { tool = "run_command", pattern = "cargo test", action = "allow" },
  { tool = "run_command", pattern = "git push", action = "ask" },
  { tool = "*", pattern = "**/.env", action = "deny" },
  { tool = "mcp__github__*", action = "allow" },
]
```

### Path grants

A grant opens a path outside the environment's folders, `read` (read and list; `write_file` and
`edit_file` are refused with "read-only") or `read_write` (like a folder), for global package
caches and toolchains. `~` means the home folder. Where a folder and a grant both cover a path,
the deepest decides, so a read-only grant inside a folder makes that part read-only. A grant's
path must exist when a session starts (missing ones are skipped); an environment's grants are
listed before the global ones, so its mode wins for the same path. Grants apply to sessions
started after a change, and the agent's system prompt lists them. The app offers one-click
presets resolved for the daemon's machine: Cargo (`CARGO_HOME` or `~/.cargo`, writable), rustup
(read-only), the npm, pnpm, Yarn, pip, and uv caches, the Go module cache (`GOMODCACHE`, else
`GOPATH/pkg/mod`), Maven, Gradle, and NuGet, using `%LOCALAPPDATA%` on Windows, `~/Library` on
macOS, and the XDG folders elsewhere.

Every session in an environment on this machine also gets one grant of its own, not listed in
the settings: its **scratch folder**, `<workspaces.root>/<environment-id>/sessions/<session-id>/`
([14](14-projects-and-automation.md#environment-working-folders-implemented)). It is a
`read_write` grant added through the same workspace, so the permission rules, deny rules, and the
safety floor apply to it exactly as to any other path, `run_command` can write to it under the
sandbox tiers below, and the system prompt calls it the agent's scratch folder instead of listing
it with the other grants. It is deleted when the session is archived or deleted.

How far grants reach depends on where commands run:

| Where | File tools | Commands |
| --- | --- | --- |
| Tier 0 (policy) | Grants and modes enforced | Not limited (the user's own rights) |
| Tier 1 (OS sandbox) | Enforced | Writable grants are bound writable (bubblewrap `--bind`, Seatbelt `subpath`); everything else is read-only already |
| Tier 2 / Docker image | Enforced | Every grant is bind-mounted, read-only ones with `:ro`: a POSIX path at the same path, a Windows path at `/grants/<n>` |
| Docker container, WSL, SSH | Enforced, for the environment's own grants as paths inside the runtime (Windows paths translated for WSL; `~` grants skipped) | Not limited: commands run with the runtime's own rights, and an existing container cannot gain mounts |

Global grants are paths on this machine, so remote runtimes (a running container, WSL, SSH) get
only their environment's grants.

### Rules

A rule is a tool name (`*` wildcards allowed; `*` alone is every tool), a pattern, and an
action: `allow` (run without asking), `ask` (ask even where the tool would not), or `deny`
(refuse at once; the model is told a rule denied it). The pattern depends on the tool:

- **`run_command`:** a command prefix matched word by word (`cargo test` matches
  `cargo test -p x`, not `cargo testing`); a final `*` word matches the rest (`git *`), and `*`
  or `?` in a word match within it (`npm run test*`). Leading `NAME=value` assignments are
  skipped, and the program is compared by its file name without `.exe`, `.cmd`, or `.bat` (and
  case-insensitively on Windows), so `rm` also matches `/bin/rm` and `RM.EXE`. A line is split at
  `;`, `&&`, `||`, `|`, `&`, and newlines (quotes are not parsed, which only makes rules
  stricter), and each command is matched on its own: any command denied denies the line, any
  asked asks, and the line is allowed only when every command is. Allow rules with a pattern
  never match a line with command substitution (`` ` ``, `$(`, `<(`, `>(`) or output redirection
  into a file (`2>&1` and redirection to `/dev/null` or `nul` are fine, but only as whole
  words: `>nul.txt` and `>&1out` name files).
- **File tools** (`read_file`, `write_file`, `edit_file`, `list_dir`): a path glob. `*` and `?`
  stay within a path segment, `**` crosses segments, and `dir/**` also matches `dir`. A pattern
  without `/` matches the file name at any depth (`.env`, `*.pem`); an absolute pattern (or one
  starting with `~`) matches the absolute path; any other matches the path relative to the
  working directory. Matching ignores case on Windows.
- **Other tools** (MCP tools, web and browser tools, memory, and the app config tools
  `set_model_card`, `install_extension`, `uninstall_extension`): only rules without a pattern
  match. The app config tools run without asking by default (removing an extension an agent
  did not install asks); a rule narrows them to *ask* or *deny*
  ([06](06-extensibility.md#agents-manage-the-app-config-implemented)). An install running
  without asking does not let the installed code run: a plugin with hooks or MCP servers is
  installed turned off, and `activate_plugin` (which never asks) cannot start it, until the user
  turns it on in Connectors.
- **The review pane's steps** `git_commit` (commit a session's changes) and
  `open_pull_request` (push the branch and open a pull request with `gh`): not agent tools but
  the user's own actions from the review pane, gated the same way with rules without a
  pattern. Without a rule they ask (the pane asks for a yes before running); `allow` runs them
  at once, `deny` refuses them. A wildcard rule counts too: `{ tool = "*", action = "allow" }`
  also allows `git_commit` and `open_pull_request`, so they no longer ask
  ([review pane](14-projects-and-automation.md#review-pane-implemented)).

Evaluation: every matching rule of both scopes is collected; an environment rule overrides the
global rules of equal specificity (both name the tool exactly or both use a wildcard, and their
patterns have the same number of literal characters); of what is left, **deny wins over ask,
and ask over allow**. So a specific allow does not lift a broader deny, while an environment can
turn a global `deny run_command cargo publish` into an allow with the same rule. With no
matching rule the tool's default applies. A rule is checked before `tool.before` hooks run; an
allow rule skips the approval but not the required-context check of
[workflow enforcement](18-workflow-contract.md).
*Always this session* allows a tool for the session where its default would ask, but an ask
rule asks every time, whatever the session allowed (an allow rule is how to stop the asking).
Rules apply to running sessions from their next call.

Approval requests carry a `suggested_rule` (`permission_requested`): the command and its
subcommand for a simple command line (none for a line of several commands, substitution, or
redirection), `<folder>/**` for a file tool, the whole tool otherwise. The app's **Always allow**
adds it to the session's environment with `permissions.add_rule` and allows the request.

Rules are a convenience on top of the folder sandbox and the isolation tiers, not a sandbox of
their own: a shell can reach a denied program in ways a prefix cannot see (`sh -c`, scripts,
aliases). Not yet: domain rules for `web_fetch`, an `auto` mode where a classifier approves
routine actions, and logging decisions in the session journal beyond the events it already
records.

### Agent permission profiles (implemented)

A persistent agent of an environment can be limited further than the environment: an allowlist
of MCP servers, an allowlist of plugins, and a **local files only** lock that removes web,
browser, search, media, MCP, plugin and command tools and confines the file tools to the
environment's folders and the agent's scratch folder. A profile only narrows: it hides tools
from the list the model gets and refuses calls to them, and the rules, permission modes and
safety floor below still decide every call that remains. It binds the agent's subagents and the
sessions it starts, and keeps the servers and plugins it forbids from starting. Only **local files
only** is a boundary against the agent itself; allowlists are not, because an agent that can run
commands can change its own profile through the daemon. See
[agent permission profiles](14-projects-and-automation.md#agent-permission-profiles-implemented).

### Permission modes (implemented)

Each session has a permission mode, which decides calls together with the rules. New sessions
people start begin in `permissions.mode` (default `default`); unattended ones (schedules,
project agents, auto-fix) begin in `default`, since nobody is there to answer. A session
changes its own with `session.control` `set_permission_mode` (`/mode <mode>` in `imperium chat`,
the [selector in the session footer](07-desktop-ui.md#environments-and-sessions-implemented) in the app); the change
applies from every agent's next call, also during a turn, is kept across restarts, and shows in
the session summary's `permission_mode`.

| Mode | Reads | File writes and edits | Commands | Other tools that ask (MCP, `web_fetch`, browser, media) |
| --- | --- | --- | --- | --- |
| `ask` (ask every time) | run | ask, every call | ask, every call | ask, every call |
| `default` | run | allow rule, else ask | allow rule, else ask | allow rule, else ask |
| `accept_edits` | run | run | allow rule, else ask | allow rule, else ask |
| `plan` (plan only) | run | refused | refused | refused |
| `full` (full access) | run | run | run | run |

In every mode, deny rules refuse first and ask rules ask, every time. "Ask, every call" means
allow rules and *Always this session* approvals are ignored (tools an unattended session was
started with pre-approved still run). Plan mode refuses without asking, whatever the rules
allow, and tells the model to present its plan; it also refuses `start_session` (the session
started would not be in plan mode) and `memory_remember`. Tools that never ask (searching the
web or earlier sessions, `ask_user`, delegation) still run, and the app-config tools
(`set_model_card`, `install_extension`, `uninstall_extension`) count as tools that ask in `ask`
and `plan`.

**The safety floor** is a built-in check that applies everywhere: no mode, allow rule, session
approval, or unattended session's pre-approved tools lift it (plan mode refuses the call
anyway). Each call it covers asks every time; in an unattended session (auto-fix, schedules,
project agents) the request waits in the approval queue, with a notification, until the user
answers:

- Commands that delete recursively or by force (`rm` with `-r`, `-R`, `-f`, `--recursive`,
  `--force` anywhere in its arguments; `rmdir` / `rd` / `del` / `erase` with `/s` or `/q`;
  PowerShell `Remove-Item` / `ri` / `rm` with `-Recurse` or `-Force`, also abbreviated;
  `find -delete`), format or partition disks (`format`, `mkfs*`, `dd`, `diskpart`,
  `Format-Volume`, `Clear-Disk`, `shred`), shut down (`shutdown`, `reboot`, `halt`,
  `poweroff`), raise privileges (`sudo`, `su`, `doas`, `runas`), or rewrite git history or
  discard work (`git push` with `--force`, `-f`, `--force-with-lease`, `--delete`, `--mirror`,
  `--prune`, or a `+ref` / `:ref` refspec; `git reset --hard` / `--merge` / `--keep`;
  `git clean` other than a dry run; `git branch -D`; `git checkout -- …`, `git checkout .`,
  `git checkout -f`; `git restore`; `git stash drop` / `clear`; `filter-branch`), with git's
  global options (`-C dir`, `-c k=v`, `--git-dir`) skipped. Each command of a line is checked,
  including inside `$( )`, backticks, and parentheses; shells running a command string
  (`sh`/`bash`/`zsh -c`, `cmd /c`, `powershell`/`pwsh` with `-Command` or with the command
  as its arguments; an encoded PowerShell command always asks) and wrappers (`env`, including
  `-u`/`-C` values and `-S` strings; `nice`; `nohup`; `time`; `timeout`, including `-s`/`-k`
  values; `command`; `exec`; `eval`; `xargs`; `find -exec`; cmd's `call` and `start`) are
  unwrapped; program names are compared without folder, extension, or case, and with quotes,
  backslashes, and `^` removed (`r"m"`, `'r'm`, `r\m`, `r^m`); long options may be abbreviated
  (`--recur`, `--for`); shell keywords (`if`, `then`, `do`, `while`, `until`, `!`, …) and
  redirections (`>out`, `2> err`) before a command are skipped. These always ask: `$IFS` in a
  line; a program word the floor cannot read (starting with `$` or a backtick, a brace
  expansion such as `{r,}m`, or one with `<` or `>` glued to it); a shell fed commands by a
  pipe, a here-string, or standard input (`… | sh`, `bash <<< …`, `bash<<<…`,
  `powershell -Command -`); PowerShell running a string or another process (`iex`,
  `Invoke-Expression`, `Start-Process`, `saps`, `Invoke-Command`); and `git -c alias.<name>=…`
  with a `!` (shell) alias, quoted or not.
- `imperium env agent add|set|rm`, which could loosen an agent's own
  [permission profile](14-projects-and-automation.md#agent-permission-profiles-implemented).
- Writing files that hold secrets (`.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`,
  `id_ed25519*`, `id_ecdsa*`, `.npmrc`, `.pypirc`, `.netrc`) or anything with a `.git` folder
  in its absolute path, such as a linked worktree's main repository's hooks.

The floor is a **best-effort guard against an agent's mistakes, not a security boundary**: it
reads the command line, so a script, an alias, or a program that deletes files itself passes
it. The folder sandbox and the isolation tiers are what confine commands.

## Isolation tiers

The UI always shows the active tier for each session and agent.

**Multi-folder sandbox (implemented):** a session's workspace is its
[environment's](14-projects-and-automation.md#environments-implemented) folders, not one
project folder. The first folder is the working directory: relative paths resolve there and
`run_command` starts there. The file tools accept an absolute path inside any of the folders and
refuse anything outside all of them (after resolving `..` and symbolic links), and the agent's
system prompt lists the further folders. A folder path may hold spaces (`D:\Git
Repositories\Imperium-AI`), and the file tools read the looser spellings models write: a path in
quotes or with surrounding whitespace, a `file://` URL, a JSON argument whose Windows backslashes
are left unescaped (a `read_file` or `list_dir` path that would decode to a newline or tab, as
`D:\new\tools` does, is read with literal backslashes), and on Windows a Git Bash (`/d/...`),
WSL (`/mnt/d/...`), or `/D:/...` spelling of a drive path. Each reading is checked like any
other path, and the first that lands inside a folder or grant is used. The OS tier makes every
folder writable (one
`--bind` per folder for bubblewrap, one `subpath` per folder for Seatbelt); the container tier
mounts the first folder at `/workspace` and the others at `/folders/2`, `/folders/3`, … in order.

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

**Read-only mounts (implemented):** a session started with `read_only`
([14](14-projects-and-automation.md#narrowed-sessions-implemented)) mounts its folders read-only
under each tier: the OS tier binds nothing in the folders writable (the whole filesystem is
read-only there already, and bubblewrap binds the folders read-only last, Seatbelt denies writes
under them last), the container tier adds `:ro` to the folder mounts, and tier 0 cannot confine a
command, so it refuses `run_command` and background commands instead of running them. The file
tools refuse writes and edits without asking. A writable path grant inside or around a read-only
folder does not reopen it: the file tools, the OS sandbox and the container mounts (a grant that
overlaps a folder is mounted `:ro`) all keep the folder read-only, while grants elsewhere, such as
the session's scratch folder, keep their mode.

### Environment runtimes (implemented)

An [environment's runtime](14-projects-and-automation.md#runtimes-implemented) can move a
session's tools off this machine. With a remote runtime the workspace's folders are POSIX paths
inside it, and every file and command tool is a short `sh -c` script run through the runtime's
own client, so the runtime needs nothing but a POSIX shell:

- **Docker container** (`docker` with `container`): `docker exec -i -w <folder> -- <container> sh -c …`.
- **WSL** (`wsl` with `distro`, Windows only): `wsl.exe -d <distro> --cd <folder> --exec sh -c …`
  (`--exec` hands the script to `sh` as one argument; after a plain `--`, wsl.exe would join the
  arguments for the login shell). Windows paths given as folders or to tools (`C:\work\app`,
  `\\wsl$\<distro>\…`) are translated (`/mnt/c/work/app`).
- **SSH** (`ssh` with `host`): `ssh -o BatchMode=yes -T [-p port] -- <host> "sh -c 'cd <folder> && …'"`
  with the system client, so `~/.ssh/config` aliases, keys, and agents apply and no password is
  ever asked for. `user@host:port` is split into the destination and `-p`.

`read_file` runs `cat`, `write_file` runs `mkdir -p` and `cat >` with the content on standard
input (so any bytes survive), `edit_file` reads and writes the same way, and `list_dir` runs
`ls -1Ap`. Paths are resolved against the working directory and checked against the folders as
written (after resolving `.` and `..`); links inside the runtime are not followed from here.
`run_command` reports the exit code, output, and errors as on this machine, and the session's
isolation reads `runtime · ssh <host>` (or the container or distribution). A Docker **image**
without a container is the container tier above with the environment's local folders mounted
(network per `isolation.network`). The permission prompts and approvals are unchanged; hooks still run on this
machine (in its temporary folder).

**Worktrees:** a session can be started on git worktrees of its environment's repositories so
its edits stay apart from the user's checkout; see
[worktrees](14-projects-and-automation.md#worktrees). The sandbox is then the worktrees (and
any folders outside git). Otherwise an environment's local sessions also get the git worktrees of their
folders' repositories (discovered from git's records, checked to point back at the repository,
and looked up again when a path falls outside everything else), as writable as the folders,
under the same path checks, with deny rules still winning; the environment's **Project
worktrees** switch turns this off. A writer subagent (`spawn_agent` with `access: "write"`)
works only in worktrees made for it, with the session's grants, permission rules (absolute
path rules for the root's paths match the same file in its worktree) and sandbox tier
unchanged, the project worktrees closed to it, its hooks run in its worktree, and tools tied to
the session's folders (the preview) not offered, so its file edits stay out of the root's checkout
until the root merges its branch (the confinement is of the file tools: commands and MCP tools
follow the permission mode and the safety floor, not the worktree)
([parallel writers](14-projects-and-automation.md#worktrees)).

| Tier            | Boundary                                                                     | Protects against                                    | Does not protect against                           |
| --------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | -------------------------------------------------- |
| 0 — Policy      | Daemon refuses built-in file tools outside allowed roots and grants, and writes under read-only grants | Model mistakes with built-in tools                  | Code run through `exec`, which has the user's full rights |
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
apply). With `daemon.socket` on at the host (Unix hosts), `imperium remote connect
<destination> --socket` forwards the host's Unix socket to a local socket instead of a TCP
port (`ssh -N -L <local>.sock:<remote>/ipc/daemon.sock`, the local end mode 0600), and the
recorded endpoint then has no URL, only that socket
([02](02-architecture.md#protocol)). Not yet: installing `imperium-core` on the host,
`~/.ssh/config` discovery in the app, choosing a remote in the desktop app (its connection form accepts the
recorded URL and token), and cross-host routing.

- Hosts are configured from `~/.ssh/config` entries or added manually; keys stay in the user's
  SSH agent. Imperium never stores SSH passwords.
- On first connect, Imperium installs or updates a matching `imperium-core` on the host (with
  consent), starts it bound to a Unix socket, and forwards that socket.
- The remote daemon owns the remote files, processes, models, and GPUs. The local router can
  still send remote subagents to hosted endpoints or to local-machine models when the user
  allows cross-host routing.
- Model folders, runtimes, and isolation tiers on a host are configured on that host.
