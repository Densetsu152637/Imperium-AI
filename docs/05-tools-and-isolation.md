# 05 — Tools and isolation

## Built-in tools

| Tool group  | Tools                                                     | Notes                                            |
| ----------- | --------------------------------------------------------- | ------------------------------------------------ |
| Files       | `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep` (implemented) | Exact-match edits (`old_string` must be unique, or `replace_all` changes every occurrence); reads numbered, capped at 2,000 lines and a window-sized character budget by default and paged with `offset`/`limit`; `list_dir` capped at 500 entries and paged with `offset`; [searches](#glob-and-grep-implemented) honour `.gitignore` and page with `offset` |
| Execution   | `run_command`, `command_output`, `command_stop` (implemented) | Runs in the project root via `cmd /C` (the line passed verbatim) or `sh -c`; default timeout 120 s; returns exit code, stdout, stderr; `background: true` [returns a process id at once](#background-commands-implemented) |
| Web         | `web.search`, `web.fetch`                                 | Search providers: SearXNG (self-hosted), Brave, Tavily, Kagi, Exa; pluggable. Pages are [cached](#fetch-cache-and-robotstxt-implemented) and `robots.txt` is honoured |
| Browser     | `browser.open`, `browser.read`, `browser.act`, `browser.screenshot` | Chromium over CDP; visible or headless |
| Agents      | `spawn_agent`, `await_tasks`, `send_to_task`, `cancel_task` | See [agent engine](03-agent-engine.md)        |
| Media       | `media.generate`, `media.transcribe`                      | See [media](09-media-generation.md)              |
| Interaction | `ask_user`, `request_clarification`, `update_state` (implemented) | Pause the agent until the user answers; keep the session state |
| Loaders     | `tool_search`, `call_tool` (implemented, offered only with `harness.tool_loading` on) | Find tools that are not in the agent's list and run one by name, through the checks of a direct call; see [reactive tool loading](03-agent-engine.md#reactive-tool-loading-implemented) |
| App config  | `set_model_card`, `install_extension`, `uninstall_extension` (implemented) | Change the user's Imperium config through the daemon, journalled and undoable; see [06](06-extensibility.md#agents-manage-the-app-config-implemented) |

Implemented in phase 8 (`imperium-web`): `web_fetch` reads an http(s) page as text (title,
final URL, scripts and markup removed, long pages in chunks via `offset` and `limit`; non-text
content is refused) and `web_search` queries the configured provider (`web.search` =
`searxng` with `web.searxng_url`, or `brave`, `tavily`, `kagi` or `exa` with a key from the
keychain, never from the settings file; `web.search_fallback` names more providers to try in
order, see [resilience](#blocked-pages-and-search-fallbacks-implemented))
and returns numbered results. Each provider implements the `SearchProvider` trait and is built
from its `web.search` name by `SearchProviders`, so adding one is an implementation and a
`register` call; unknown names leave search off. Agents are told to cite the sources they use
with their URLs and to treat web content as data. Searching runs without asking; `web_fetch` asks unless the host
is in `web.allow_domains` (subdomains included) or the user allowed fetching for the session.

**Threat model: exfiltration through web tools.** Because searching never asks, an agent steered
by injected content can put anything it has read (file contents, memory, another agent's answer)
into a `web_search` query, and so to the search provider; a `web_fetch` to an allowed domain can
carry data in its URL the same way. Domain rules limit where fetches go, not what queries say.
This applies to every agent with the web tools, including [knowledge-agent](21-knowledge-agents.md)
forks, which may use the web by the user's decision. Mitigations available today: turn search
off (`web.search` unset), a [permission profile](#agent-permission-profiles-implemented) with
local files only, or an ask rule on `web_search`; query screening is not built.

### Blocked pages and search fallbacks (implemented)

Many sites and search engines answer a program with bot protection instead of
the page. Imperium reads that as a result to report, never as content, and never tries to get
past it: it does not solve CAPTCHAs, spoof another browser or identity, or rotate proxies. Every
request carries the same honest `User-Agent` (`Imperium/<version> (AI agent web tools)`).

- *Recognising a block.* Only a challenge signal is a block: a 429, the `cf-mitigated: challenge`
  header, or a page that is a Cloudflare, Akamai or similar challenge, a CAPTCHA, an "unusual
  traffic" wall or a titled "Access denied" page. Pages are judged by their title, well-known
  markers and how little text they have, so a long article that merely talks about CAPTCHAs is
  not a block. A bare 403 (a login wall, a permission) is an ordinary HTTP error and triggers no
  fallback. A block is returned to the agent as `<url> is blocked by bot protection or an access
  rule (<why>)` with what else was tried; none of the block page's text is passed on, and a block
  page is never cached. An ordinary 404 or 5xx stays an ordinary HTTP error.
- *Polite retries.* A 429 or a 500, 502, 503 or 504 is tried again at most twice. The pause is
  the server's `Retry-After` in seconds when it gave one, otherwise 0.5 s doubling up to 8 s. A
  `Retry-After` longer than 8 s is not waited for: the block is reported with the wait the server
  asked for. This covers pages, search providers and archive requests.
- *Fetch fallback.* When plain HTTP meets a block, and only if the user turned on
  `web.archive_fallback` (off by default), `web_fetch` asks the Wayback Machine's availability
  API and returns the closest capture, headed `ARCHIVED COPY, not the live page` with the capture
  date, which the agent must say when it cites it. Only the page's URL is sent, and only for
  public pages: never a URL with a user name, password or credential-like query parameter, a
  single-label name, a `.local`, `.internal`, `.corp`, `.lan`, `.home.arpa` or `.localhost` name, a
  private, loopback or link-local address, or a name that resolves to one. An archived page that
  is itself a block is refused. The headless browser is not a fallback here: it has its own
  permission decision, so a browser fallback waits for the browser rework.
- *Search fallbacks.* The chain is `web.search` first, then each `web.search_fallback` provider
  in order, each name once; `none` is skipped, so with `web.search = "none"` the fallbacks alone
  form the chain, and with both empty there is no provider. A provider whose URL or key is
  missing is left out with a warning. When one is blocked, fails (a bad key, a rate limit, no
  connection) or finds nothing, the next is tried; the result ends with `Answered by search
  provider: <name>` and one `Skipped <name>: <why>` line for each provider passed over. When
  every provider fails the error lists each reason. `web_search` is always offered; with no
  provider it answers that none is configured and tells the agent to ask the user to set one up
  in Settings, Web. A provider's key is the keychain secret named by its own setting
  (`web.brave_key`, `web.tavily_key`, `web.kagi_key`, `web.exa_key`), else the shared
  `web.api_key`; keys go out only as request headers, are redacted from `Debug` output and never
  appear in errors or logs.
- *Who can change this.* The file tools never write `config.toml` and no app-config tool changes
  `[web]`. The agent floor makes an agent's direct `imperium config set|unset web.*` and
  `imperium secret set|delete` commands ask the user (also wrapped in a shell, or with
  `--remote`). Under the policy tier that is a speed bump, not a boundary: a quoted executable
  path containing spaces is not caught yet (the quote-aware tokenizer lands with PR #330), and a
  shell write to `config.toml` is not checked. For real protection use the OS or container
  isolation tier, or an ask rule on `run_command`. Turning a provider on stays the user's
  decision.

### Fetch cache and robots.txt (implemented)

**Search providers.** `kagi` (the Kagi Search API, `Authorization: Bot <token>`; its related-search
entries are not results) and `exa` (`x-api-key`, with a short text excerpt per result) join
`searxng`, `brave` and `tavily`. A key provider is built only when its own key setting or `web.api_key` names a keychain
secret that exists; otherwise it is left out of the chain with a warning (and search stays off when none is left). Keys are sent only as request
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
`robots.txt`, and against the approval rule (a redirect is followed only within the scheme, host
and port that were asked for, or to a host in `web.allow_domains`; never from https to http;
otherwise the call fails and names the target so the user can approve that URL). A page is
cached only when every hop stayed on the original scheme, host and port, so one session never
gets another origin's content from the cache under a URL it was not allowed to follow. Search
providers get no redirects, so an API key is never replayed to another host (a SearXNG instance
that redirects must be configured with its final URL).

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

**Browser tabs and downloads (implemented).** The browser has up to eight tabs (`MAX_TABS`).
`browser_open` takes a `new_tab` flag (the page opens in a new tab, which becomes the active one;
the same domain rule and approval apply), `browser_tabs` lists the tabs (id, title, URL; the
active one marked), `browser_switch` makes one active and returns its text, and `browser_close`
closes one (the last tab cannot be closed; closing the active one hands over to a neighbour). The
other browser tools work on the active tab. Ids are short numbers that are never reused while the
browser runs. Every tab is driven over the browser's one debugger connection, its pipe (see
[the debugger pipe](#the-browsers-debugger-pipe-and-profile-implemented)): `Target.getTargets`
lists the pages, `Target.createTarget` and `Target.closeTarget` open and close them, and the
active tab is driven through a session of its own (`Target.attachToTarget` with `flatten`; the
previous tab's session is let go on a switch). The tab list is
reconciled with what the browser reports after every action, and a page that opens a window
(`target=_blank`, `window.open`) gets a tab of its own, up to the limit (the rest are closed). The
agent is told when a page opened a tab and is not switched to it. Clicks count as a user gesture so
a link that opens a window works; the limit still holds. Each `browser_view` event carries the
tab and the list of tabs (protocol 0.35); the desktop keeps one mirror per tab
([07](07-desktop-ui.md#preview-and-browser-panels-implemented)), and the user's takeover applies
to the active tab (`session.browser_act` gained `new_tab`, `switch_tab` and `close_tab`).

Downloads are refused by the browser itself. At start-up a browser-level command on the debugger
pipe sets `Browser.setDownloadBehavior` to `deny` with events on, and a browser that cannot be
made to refuse downloads does not start; the refused downloads it reports while another answer is
awaited are kept, not skipped. As defence in depth the browser's profile is also written, before it
starts, with an empty `blocked-downloads` folder as its download and save folder and a prompt
nobody sees. If the pipe later closes or fails, the browser is treated as unsafe:
it is closed and the next browser tool starts a new one (the open pages are lost, and the agent
is told). When a click or script starts a download, the next browser tool result says so (the URL
and the suggested name, flattened to one line and framed as page text), and `browser_download`
saves it. `url` is required: the agent names the URL it was told about, so the line the user
approves is the request.

*Approval.* Every call is approved afresh: the question shows the exact URL (control characters
written out as escapes, so it stays one line), the folder and the size limit, and it is not
covered by "allow for this session", an allow rule, pre-approval of the tool or the `full`
permission mode, and offers no "always allow" rule. This is the tool provider's `always_asks`,
which the session treats like the safety floor (the same way `mcp_sampling` asks each time). A deny
rule on `browser_download` or plan mode still refuses it, an agent profile that locks an agent to
local files removes it with the other web tools, and it is refused while the user drives the
browser. `browser_switch` asks when the tab's page is outside `web.allow_domains` (or unknown),
because a page can open a tab on any site; opening a page, in a new tab or not, follows the
`web_fetch` domain rule.

*Where and how.* The file is fetched by the same redirect rules and `robots.txt` check as
`web_fetch` (the host and port asked for, or `web.allow_domains`; never https to http;
`web.robots`), on a client of its own with a connect limit, a 30 s stall limit and 10 minutes in
all, not the 30 s the page client has. It is not fetched by the browser (its profile has no
logins). The result names where it ended after redirects. It is saved only into one folder chosen
by the session, never by a page or the agent: `web.download_dir` when set, else `downloads` in the
session's scratch folder (its working folder if it has none); a remote or read-only session offers
no `browser_download`. Before writing, the folder is made if needed and then refused if it is a
symbolic link or a Windows reparse point (junction), or if its canonical path is not its parent's
canonical path plus its own name; the file is then judged at its resolved path by the safety
floor (secrets, anything under `.git`) and, for the default folder, by the file tools' write
resolver (a read-only grant, the local-files scope).

*The file.* At most `web.download_max_mb` (25; `0` turns downloads off), checked against the
announced length and again while receiving. It is written as `<name>.part` and renamed when
complete; a failed, timed-out or cancelled download leaves nothing behind. The name is made safe:
folders, special characters and hidden-file dots removed, Windows reserved names (`CON`, `COM0` to
`COM9`, superscript digits, `CONIN$`, `CONOUT$`, trailing spaces before the extension) prefixed, at
most 100 characters, a taken name gets a number as in `report (2).pdf`, and nothing is
overwritten. `desktop.ini` is refused. Only known-inert extensions (documents, images, media,
archives, plain data: `txt`, `pdf`, `png`, `zip`, `csv`, `docx` and the like) keep their name; any
other, `.scf`, `.url`, `.msc`, `.iso`, `.docm`, `.xlsm`, `.exe`, `.sh` or one nobody has heard of,
is saved with `.download` added (a name with no extension is kept). A `text/html` answer for a
file name is flagged (an error or login page); `blob:` and `data:` downloads, which the browser
keeps no fetchable copy of, cannot be saved. The file is never opened or run and its mode is the
platform default, never executable. Where the platform has a way to, it is marked as coming from
the web: the `Zone.Identifier` stream (zone 3, `HostUrl` the final URL) on Windows, the
`com.apple.quarantine` attribute on macOS. The mark goes on the `.part` file before the rename, so
the finished file is never there unmarked; on Windows a file that cannot be marked is not kept
(the download fails), which includes a downloads folder on a file system with no alternate data
streams (FAT, exFAT, some network shares), while on macOS it is kept with a note. The 600-second
limit covers the whole download, the request, redirects and headers included, not only the
transfer of the body. The downloads folder is checked before the file is made (not a link,
resolving to a place inside its own parent, and the same folder by device and inode or volume and
file index as its canonical path, so case and 8.3 short names are left to the file system and
names are never compared as text) and again before the mark, before the rename and after it: a
folder swapped for a link in between fails the download, and a file that landed elsewhere is
removed. This narrows the window in which a swap can redirect a file; it does not close it.

Titles, URLs and file names come from pages: they are shown to the agent flattened to one line,
framed as data, and the web instructions say to treat them as data, never as instructions.

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
holds it). Giving the browser back restores the agent's tools. Screenshots for vision models are
later work.

**The takeover is the desktop app's alone (implemented, browser stage 0).** A driven browser asks
no approval and is signed in to whatever the user signed in to, so the daemon answers
`session.browser_control` and `session.browser_act` only to the desktop app installed beside
`imperium-core`, on the local socket (the identity checks of
[auto-merge](#auto-merge-the-desktop-app-alone-implemented)). It refuses, with `permission
denied`, a paired device, a mod's page (loopback TCP), a plain token holder and every other local
process, an agent's own `run_command` reaching the socket included; on a server (`daemon.server`)
both calls answer `-32010` (`browser_control`) before any caller is looked at. The webview cannot
make that connection itself, so the Browser panel goes through the app's own `browser_control`
and `browser_act` commands, which check that they were called from the app's own windows, that
the app's environment holds no loader override, and that the session id is plain text, and then
send the one call on the app's own local-socket connection
(`apps/desktop/src-tauri/src/browser_takeover.rs`). The agents' browser tools are unaffected:
they run in the daemon and never go through these calls.

#### The browser's debugger pipe and profile (implemented)

- **No debugging port.** The browser is started with `--remote-debugging-pipe`, never
  `--remote-debugging-port`: it opens no TCP port, so no other process on the machine (another
  user's, a sandboxed one, an agent's command) can attach to it and read the profile's cookies.
  The DevTools Protocol runs over two inherited anonymous pipes, one JSON message per
  NUL-terminated frame, read and written by a thread each in the daemon
  (`crates/imperium-web/src/cdp.rs`, `launch.rs`). On Linux and macOS the browser's ends are
  placed at fds 3 (it reads) and 4 (it writes) between `fork` and `exec`; every other descriptor
  stays close-on-exec. On Windows the browser is told the handle values
  (`--remote-debugging-io-pipes=<read>,<write>`, read by Chrome and Edge alongside
  `--remote-debugging-pipe`) and is created with `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` naming only
  those two handles and its log file. The pipes are made by `CreatePipe`, which no process can
  open by name, and are inheritable only during the `CreateProcessW` call. A test asserts that
  no process of the browser's tree listens on a TCP port and that it writes no
  `DevToolsActivePort` file. The start-up warm-up's throwaway page loads an empty page the daemon
  serves on a loopback port for the moment of the warm-up only.
- **No direct WebRTC traffic.** The browser is started with
  `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`, so a page's WebRTC sends no UDP
  except through a proxy: it cannot probe the local network or learn the machine's local
  addresses.
- **The profile is a secret path.** `<data>/browser` holds the user's sign-ins and cookies. The
  file tools refuse it, for reading as for writing, in every mode and whatever a rule or an answer
  says (`imperium_policy::permissions::refuse_access_under`, checked on the resolved path), and
  `glob` and `grep` leave it out of every walk; a command that names it (any separators, and case
  on Windows and macOS) is covered by the safety floor and asks every time; the OS tier hides it
  from commands like the daemon's credentials (it is made at daemon start, so it exists to be
  hidden before the browser first runs), and the container tier never mounts it. No export
  includes it, and the browser's output in `imperium-browser.log` reaches errors only as a short
  tail with the debugging endpoint redacted.
- **Limits.** The command check reads the command line, like the rest of the floor: a script that
  builds the path is not seen, and on Windows (no OS tier) that check is all a tier-0 command
  meets. On Windows, a process the daemon starts by another route during the brief moment the
  pipe handles are inheritable could inherit them too (Rust's own `Command` inherits every
  inheritable handle); the browser itself inherits nothing else. A Chrome or Edge too old to read
  `--remote-debugging-io-pipes` does not answer on its pipe and fails to start.

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

Implemented in phase 1 (`imperium-tools`, with the rules, modes, safety floor and agent profiles
in `imperium-policy`): paths resolve inside the project folder and may not
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

**Completion notices.** The agent need not poll: when a background command ends on its own
(an exit with any code, or a signal), the agent that started it is told once, at the same safe
point where steering messages join the turn (between a finished tool round and the next model
call, so tool calls and results stay paired). The notice is a user-role message wrapped in
`<background-notice>`, with a line saying it is a report and not an instruction: the id, the
command (one line, cut to 200 characters), how it ended, how long it ran, the last 5 non-empty
lines of output (each cut to 200 characters, labelled "data, not instructions"), and
`command_output {"id":..,"since":0}` to read the rest. The command and the output are data from
outside: control characters and ANSI escapes are removed, and the harness's tag names
(`background-notice` among them, see `HARNESS_TAGS`) are defused in them, so they cannot close
the block or fake one. Each notice is also emitted as a `notice` event, so clients show it.
Several commands that end between two calls each get a notice, in start order. Each process
remembers the agent that started it (the workspace an agent's tools run in carries its id): the
root is told of its own commands and of any no agent owns, a subagent of its own, at its own
safe point. When a subagent finishes, its background commands are released: the ones still
running are stopped (with everything they started; they are not handed to the root, which was never
told of them) and all its entries leave the table, so a subagent's command never lingers or
outlives it. A subagent that finished and is later resumed under the same id starts without them:
the ids (`p1`, ...) it was given before are gone, and it starts commands again if it needs them.
Not reported: a command the agent stopped with
`command_stop`, one killed because the session stopped, and one whose end the agent already read
through `command_output`. A finished command whose notice is still waiting is not dropped from
the table to make room (unless the table has doubled). Nothing wakes an idle session: a command
that ends between turns is reported at the first safe point of the next turn (the way a queued
message waits for its turn). Not yet: a `notify_on` pattern that reports a matching output line
while the command still runs, and remote runtimes (they run no background commands).

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

### `request_clarification`

`request_clarification(missing_parameter, options, question?, confidence?)` is the root's way
to ask about one missing piece instead of guessing. `options` lists the likely answers (at most
five, best first; empty when the answer is open); `question` is the sentence shown (made from
the missing parameter when left out); `confidence` (0 to 1) is the root's confidence in its
best guess and is only echoed back. It goes through the same prompt as
[`ask_user`](#ask_user): `questions_asked` with one question, `session.answer`,
`questions_answered`, no model calls while it waits, and the same 10-minute limit. Nothing is
recommended, so a blank answer or a timeout is not taken as a choice: the result says the
parameter is still unknown, not to guess it, to finish what does not depend on it and to tell
the user what is waiting. It is offered only in attended sessions and when `harness.clarify`
applies (`auto`: a local root model): sessions that nobody reads (background, auto-fix, project
and persistent agents) are not offered it, because their runners answer `ask_user` with its
recommendation. The root's prompt says when to call it (confidence below
`harness.clarify_below`, after looking at the state, the conversation and the files), and
[the evaluation](03-agent-engine.md#harness-aids-for-small-models) scores it.

### `update_state`

`update_state(entities?, forget?, goals?, constraints?, drop_constraints?)` changes the
root's [session state](03-agent-engine.md#harness-aids-for-small-models) and returns the new
state. Offered to the root when `harness.state` is on; not a permission matter (it changes no
files) and allowed in plan mode.

## MCP

**Implemented (phase 5, `imperium-mcp`):** each session starts its MCP servers before its
first turn: stdio processes (newline-delimited JSON-RPC, stopped with the session) and
streamable-HTTP endpoints (JSON or SSE responses, `Mcp-Session-Id` kept), using MCP revision
`2025-06-18`. Tools are listed (following pagination) and offered to the root and subagents as
`mcp__<server>__<tool>`. A call asks for approval unless the server's `allow` list names the
tool (or `"*"`), or the user allowed it for the session; the result's text, resource text, and
descriptions of other content are returned to the model, and a tool-reported error fails the
call. Servers that fail to start are reported as session notices; the session carries on
without them. Server pings are answered; roots and elicitation requests are refused, and
sampling is refused unless enabled (below). A remote server that wants an OAuth sign-in is
reported as needing one, and the user signs in to it on purpose
([OAuth for remote servers](#oauth-for-remote-servers-implemented)).

**Planned (roadmap item 62, stage 2 of the [CAS plan](plans/cas-mcp.md) in progress):** a built-in
computer algebra server, `imperium_cas`, backed by SymPy and started per session like any stdio
server. Agent text never reaches `sympify`, `parse_expr` or `eval`: a hand-written parser reads a
small infix language, a size guard refuses runaway numbers before anything is computed, and a
fixed table builds the result into SymPy objects. Each call is meant to run in a fresh worker
process that has already imported SymPy, answers one call and exits; an audit hook ends the
worker if anything opens a file, starts a process, compiles code or imports a module. Each call
has a CPU limit (15 s by default), a memory limit (1 GiB) and a wall clock (20 s, counted from
when the call arrived, so time spent queued counts). On Linux the worker applies the CPU and
memory limits itself; on macOS it applies the CPU limit, and the server polls the worker's memory
every 50 ms, so a worker can overshoot the memory limit briefly, by at most 2 GiB; on Windows the
limits come with the server's sandbox (a later stage). On Linux with Landlock, neither the server
nor its workers can start programs outside the interpreter's installation. The parser, guard,
builder, worker, limits and worker pool exist so far, as the `sidecars/cas/` project and its
`python-cas` CI job; the MCP server, its sandbox and its setup are later stages.

### Resources, prompts, and sampling (implemented)

A server that declares the `resources` capability gets two more tools beside its own,
`mcp__<server>__resources_list` (resources and resource templates, one page per call with a
`cursor`) and `mcp__<server>__resources_read` (by `uri`, including a URI filled in from a
template). One that declares `prompts` gets `mcp__<server>__prompts_list` and
`mcp__<server>__prompts_get` (a prompt's name and its arguments, returned as its messages for the
agent to use). A server's own tool of the same name wins. These are ordinary MCP tools of that
server: they ask for approval unless the server's `allow` list names them (or `"*"`), the
permission rules match them as `mcp__<server>__*`, an agent's MCP-server allowlist removes them
with the server, and the **local files only** lock hides them with every other MCP tool.
Everything they return is framed as data: a notice that it is untrusted content from the server,
then the text inside `<mcp-resource>`, `<mcp-prompt>` or `<mcp-listing>` tags (a closing tag
inside the content is neutralised), cut at 60,000 characters. Binary resource content is
described by size and type, not decoded.

**Sampling** (a server asking the session's model for a reply, `sampling/createMessage`) is off
unless the user's own config sets `sampling = true` on that server; a repository's `.mcp.json`
or environment config can never turn it on, and the connector forms keep the value that is in
the file. The client then declares the `sampling` capability, and a request is answered only
while some call of this session to that server is in flight (usually the tool call that made
the server ask; a stdio server's requests are shared by the calls waiting on it, so with
parallel calls any of them may carry the question); at any other time it is refused, and a
request still waiting when its call ends or is cancelled is refused with `-1`. Each request
waits for the user's approval, shown as a permission request for `mcp_sampling` that states the
server, the model, the token limit, the message count and size, whether there is a system
prompt (with a short excerpt), an upper bound on the cost from the model's price, and an
excerpt of the last message, quoted with line breaks marked `⏎`. A request whose cost bound is
more than a blocking budget has left in its cost limit is refused before the user is asked, with
the two figures in the message (a budget that asks, one that only warns, and one with no cost
limit do not count: an ask budget raises its own approval prompt before the call, and calls in
flight are not counted; a model with no price has no bound to compare). It is never pre-approved,
"allow for the session" does not apply to it, and it is refused outright in an unattended
session (background, auto-fix, project agents: "sampling needs a person watching the session")
and cannot be answered through `imperium mcp-serve`, in every permission mode. A tool call may
make at most three requests, and none after one was declined or failed. After approval the
session's current model writes the reply with no history, no session instructions, no tools, no
reasoning control, and a system note that the server's messages are untrusted data; the call
passes through the usage budgets and is counted like any other (by estimate when the provider
reports no usage). The request is bounded: text only, at most 50 messages and 100,000
characters of prompt, and at most 4,096 generated tokens (1,024 when the server names none);
the server's model preferences and `includeContext` are ignored, and the reply names the model
that wrote it. Declining returns the MCP error `-1` to the server. The time spent waiting for
the user or the model is added to the server's timeout, but nothing else resets it: a server
that only sends pings cannot keep a call alive.

*Not built:* inserting an MCP prompt into the desktop or CLI composer as a slash command (it
needs a daemon method and a protocol minor; today only the agent can use prompts), resource
subscriptions and list-changed notifications, image or audio sampling, and OAuth for remote
servers.

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

**Imperium as an MCP server (implemented, session level).** `imperium mcp-serve` is an MCP
server on stdio that lets an external agent such as Claude Code start sessions, follow them,
read their tool logs and answer their permission requests through the daemon, with everything
under the daemon's normal permission checks; see [20](20-claude-harness.md). With `--tools` it
serves Imperium's own tools (files, search, web, memory) as MCP tools instead, each call run
inside a session under the same checks, read-only unless the user enables writes
([20](20-claude-harness.md#exposing-imperiums-own-tools-implemented)). A session never starts
`imperium mcp-serve` as one of its own MCP servers: that launch is refused, so a call cannot
loop back into the daemon it came from.

A stdio server's tokens go in `secret_env`, environment variables read from the keychain when
the server starts, so the config file never holds them:
`secret_env = { SLACK_BOT_TOKEN = { secret = "mcp.slack.bot-token" } }`. A server may also
record `catalog = "<id>"`, the connector it was added from, and `enabled = false` keeps it
configured without starting it. Plugins found in the plugin folders can be turned off by name
with `extensions.disabled_plugins`.

### OAuth for remote servers (implemented)

A remote (HTTP) server that protects itself with OAuth answers 401 with a `WWW-Authenticate`
challenge. Imperium follows the MCP authorization specification: it reads the server's protected
resource metadata (RFC 9728, from the challenge's `resource_metadata` or the well-known
locations; the metadata must name a `resource`, and it must be this server: the same origin
and a path that covers it segment by segment, so `/mcp` covers `/mcp/x` but not `/mcpx`), then
the authorization server's metadata (RFC 8414; the issuer it names must equal, exactly, the one
it was fetched for, and an issuer with a query or fragment is refused). A server that publishes
no resource metadata at all is treated, as the earlier revision of the specification has it, as
its own authorization server. The authorization
server must advertise PKCE with `S256`, or Imperium refuses to sign in. It registers itself there
as a public native client (RFC 7591 dynamic client registration; a server without a registration
endpoint is not supported yet) and runs the authorization code flow. The registration is kept
(with the port it was made for) and reused by the next sign-in to the same authorization server
when that port is free, and dropped if a sign-in that used it fails:

- **The user's own browser.** The daemon listens on `127.0.0.1` on a random port, builds the
  authorization URL (`response_type=code`, the client id, the loopback `redirect_uri`, a random
  `state`, the `S256` `code_challenge`, the `resource` indicator of RFC 8707 set to the server's
  URL, and the `scope` the challenge or metadata asks for) and hands it back. The desktop app
  opens it with the system's browser; `imperium mcp login <server>` prints it. Imperium never
  drives a browser for this and learns nothing from it but the redirect.
- **The redirect is checked.** The listener answers only `GET /callback` addressed to
  `127.0.0.1:<port>` (so a page cannot reach it through a name it controls). A request with
  the wrong `state` is answered 400 and ignored, and so is any stray request: anything on the
  machine can reach a loopback port, so only the response with the right `state` ends the flow
  (an error or a missing code with the right `state` ends it as a failure). The code is
  exchanged with the PKCE verifier and the `resource` indicator. A sign-in that is not finished
  within five minutes ends.
- **Storage.** The tokens and the registration go to the keychain through the daemon's secret
  store, never to a config file, a log, or a tool result: `mcp.<server>.oauth.meta` (client
  registration, endpoints, expiry, and the URL the tokens are for), `.access` and `.refresh`
  (separate entries because a keychain entry may be small), and `.client` (the kept client
  registration, which signing out keeps and removing the server deletes). They are bound to the
  server's URL: a server whose URL changes is signed out, and a token is offered only to exactly
  the URL it was issued for. A lifetime longer than a year is clamped. If the keychain refuses a
  refreshed token, it is kept in memory for the life of the daemon (and a warning logged) so a
  rotated refresh token is not lost.
- **Use.** Every request to the server carries `Authorization: Bearer <token>`, the header
  marked sensitive. A token within a minute of expiring is refreshed first. The daemon keeps one
  token source per server, shared by every session, and its lock makes the refresh single-flight
  (a rotated refresh token replaces the old one and can be used once); signing out takes the
  same lock. When the server answers 401 to a token
  it was just given, the token is refreshed once and the request repeated; after that the
  server needs a sign-in. A refresh token the authorization server rejects (`invalid_grant`)
  signs the server out, but only after a second look at the store: if the refresh token stored
  is no longer the one that was sent, another session refreshed first and its token is used
  instead. A failure to reach the authorization server does not sign anyone out.
- **Redirects never carry credentials elsewhere.** The HTTP client for an MCP server follows a
  redirect only within the origin it came from, so neither the token nor a configured header
  is replayed to another origin; the authorization server's endpoints are not redirected at all.
  Endpoints must be `https`; `http` is accepted only to a loopback address and only when the
  server being signed in to is itself local (development, tests). When the server is public,
  an endpoint, issuer or metadata address that is a literal loopback, private, link-local or
  unspecified IP address (or `localhost`) is refused, so a public server cannot aim Imperium at
  the local machine or a private network. Hostnames are not resolved for this check.
- **Unattended runs never start a sign-in.** Starting a session only reads what is stored. A
  server that needs a sign-in the user has not done (or that expired for good) is a session
  notice: `needs sign-in: run imperium mcp login <server> or use Sign in on the server in
  Connectors`; the session carries on without its tools. Only an explicit request (`mcp.login`)
  starts a flow, and a second one for the same server replaces the first.
- **Sign-out.** `mcp.logout` (or `imperium mcp logout <server>`) asks the authorization server to
  revoke the refresh token where it offers a revocation endpoint (best effort), forgets the
  three keychain entries, and cancels a sign-in still waiting for the browser. Removing a server
  revokes (in the background, best effort) and forgets its sign-in and its client registration.

A server takes a sign-in only when it is an HTTP server of the user's own configuration
(`[mcp.servers.<name>]`, including one set through an `IMPERIUM_MCP__SERVERS__*` environment
variable) without a `bearer_token` and without an `Authorization` header of its own (those are
fixed credentials and are sent as they are). A server only a repository's `.mcp.json` declares
is not signed in to: a sign-in is started by name from the user's config.

*Residual risk:* the authorization server is whatever the server's metadata names, and the user
approves the sign-in in their browser at that address. A malicious MCP server could name a
legitimate authorization server that ignores the `resource` indicator (RFC 8707); the token it
issued would then not be limited to that server, though Imperium still sends it only to the URL
it signed in for. Sign in only to servers you trust.

In the desktop app a server row shows *Waiting for sign-in*, *Needs
sign-in* or *Signed in*, with *Sign in*, *Cancel sign-in* and *Sign out* actions; a sign-in
that is under way keeps its address on screen so it can be opened again or copied. From the
command line, `imperium mcp login <server>` prints the address and waits for the result;
`--remote` is refused, because the redirect returns to the machine the daemon runs on.

*Not built:* pre-registered client ids for authorization servers without dynamic registration;
client credentials and other grants; sign-in for servers that a plugin or a repository's
`.mcp.json` declares (the sign-in is keyed by a name in the user's config, and only those can be
signed in to); step-up authorization after a 403 `insufficient_scope`; opening a sign-in from a
paired phone (the redirect returns to the desktop).

### Connectors: managing MCP servers from the app (implemented)

The desktop app's **Connectors** window (title bar, or Settings → Connectors) lists installed
MCP servers, plugins, and skills, and offers a directory of popular servers to add. It works
through these daemon methods, which write `[mcp.servers.<name>]` in the user config with
`toml_edit` the way `config.set` does: comments, formatting, and unknown keys are kept, the
whole configuration is validated after the edit, and a change that would make it invalid is
undone. Server names are 1–40 lowercase letters, digits, `_` or `-`.

| Method | Does |
|---|---|
| `mcp.list` | The servers in the user config with their last known status (`untested`, `connected` with the tool names, `error` with the reason, or `disabled`) and any secrets missing from the keychain, and for a remote server without a token of its own its `sign_in` (`signed_in`, `signing_in`, or `needs_sign_in`); the plugins in `<config>/plugins` and `extensions.plugin_dirs` (with skill, command, hook, and server counts and whether each is enabled); and the user's and plugins' skills. |
| `mcp.catalog` | The connector directory. |
| `mcp.install` | Adds a directory entry under a name, filling its settings from the form's values. |
| `mcp.save` | Adds or replaces a server from its full settings (custom servers and *Configure*). |
| `mcp.set_enabled` | Turns a server on or off (`enabled = false`). |
| `mcp.remove` | Removes a server; optionally deletes the secrets it referred to. |
| `mcp.test` | Starts the server as a session would, lists its tools, and stops it; the result is what `mcp.list` shows until the next test. |
| `mcp.login` | Starts an OAuth sign-in to a remote server and returns the address to open in the user's browser ([OAuth for remote servers](#oauth-for-remote-servers-implemented)). `mcp.list` shows `sign_in` as `signing_in` until it ends. |
| `mcp.logout` | Signs out of a remote server: revokes where possible, forgets the stored tokens, cancels a sign-in under way. |

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
  match. `set_model_card` runs without asking by default. `install_extension` and
  `uninstall_extension` always ask, in every permission mode: an agent never installs an
  extension without the user's explicit approval. They are the tool provider's `always_asks`
  (as for `browser_download`, above): approved afresh for each call, not covered by "allow for
  this session", an allow rule, pre-approval, the `auto` classifier or the `full` mode, with no
  "always allow" rule offered; a deny rule or plan mode still refuses them
  ([06](06-extensibility.md#agents-manage-the-app-config-implemented)). Approving the install does
  not let the installed code run, or load: a skill or plugin an agent installs is withheld by
  the load-time gate (no skills, commands, agents, hooks or servers) until the user approves it in
  the Imperium desktop app, and a plugin with hooks or MCP servers is also installed turned off,
  so `activate_plugin` (which never asks) cannot start it.
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
aliases). Not yet: domain rules for `web_fetch`, and logging every decision in the session
journal beyond the events it already records (the [`auto` mode](#auto-mode-implemented)
journals its own).

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
| `auto` | run | as `default`, but a [classifier](#auto-mode-implemented) may let it run | as `default`, classifier as for edits | as `default`, classifier as for edits |
| `accept_edits` | run | run | allow rule, else ask | allow rule, else ask |
| `plan` (plan only) | run | refused | refused | refused |
| `full` (full access) | run | run | run | run |

In every mode, deny rules refuse first and ask rules ask, every time. "Ask, every call" means
allow rules and *Always this session* approvals are ignored (tools an unattended session was
started with pre-approved still run). Plan mode refuses without asking, whatever the rules
allow, and tells the model to present its plan; it also refuses `start_session` (the session
started would not be in plan mode) and `memory_remember`, `memory_edit` and `memory_forget`. Tools that never ask (searching the
web or earlier sessions, `ask_user`, delegation) still run, and the app-config tools
(`set_model_card`, `install_extension`, `uninstall_extension`) count as tools that ask in `ask`
and `plan`; `install_extension` and `uninstall_extension` ask in every other mode too.

**Woken turns are capped (implemented, pull-request card stage 7).** When the daemon wakes a
session because someone wrote a review comment on its pull request
([14](14-projects-and-automation.md#review-comment-wake-implemented-stage-7-protocol-063)), the
turn runs in the stricter of the session's mode and `ask`: every mode but `plan` becomes `ask`
(allow rules, *Always this session* approvals, `accept_edits`, `full` and `auto` do not apply
during the turn), and `plan` stays `plan`. Deny rules and the safety floor apply as always, and
nothing is pre-approved. On top of the mode, the session is **capped** (`Permissions::cap`, held
by `permissions::TurnCap`): **every call that is not a read asks, every time, with no exception**,
whatever the tool's own default, an allow rule, an allow list, the session's approvals or a
pre-approval says. That covers file writes and edits, commands, web search and fetch, browser and
media tools, MCP and extension tools, `activate_plugin`, and the agent tools that change something
outside the conversation: `spawn_agent`, `start_session`, `steer_agent`, `queue_agent`,
`resume_agent`, `keep_as_knowledge`, `ask_agent`, `delegate_chunks`, `memory_remember`,
`memory_edit` and `memory_forget`, also when reached through `call_tool`. Deny rules still refuse
and plan mode still refuses. Reads (`read_file`, `list_dir`, `glob`, `grep`, `memory_search`,
`memory_read`) run. `update_plan` and `update_state` are **refused** ("not available in a turn
woken by review comments"): what they write is read again at the top of later turns and after a
compaction, where it would be trusted, so nothing a capped turn plants outlives it; `await_tasks`,
`list_agents`, `list_knowledge_agents`, `load_skill`, `context_load` and `ask_user` read or wait.
The cap also counts as the safety floor, so a tool the session was started with pre-approved still
asks. A session the daemon starts for a wake (a deleted or archived owner, or a failed checkout) is
capped for its whole life (`SessionOptions::capped`, kept in its record). **The user cannot lift
that cap**: to work uncapped, start a new session or branch one. The session summary says so
(`SessionSummary.capped`), the desktop shows a "capped (review wake)" badge in the session footer,
and a capped session's approval requests offer no "always allow" rule. If the user binds such a
session to the pull request and turns auto-fix on, later wakes run on that standing cap. In a
capped turn "Always this session" is not offered and is answered as a single "allow" if sent: an
approval given under the cap would otherwise outlive it. A branch of a session that handled
review comments carries the comment text in its history, like the session itself, and is not
capped: the card's notice applies to it too.
The session's rules and approvals are untouched, and the session's own mode comes back when
the turn **and its subagents** are over. Subagents share the session's permissions, so the cap
is held until none is working (a paused one is cancelled). If the turn does not end in time the
session is stopped and stays capped until the turn really ends, so resuming it later does not
run it uncapped. If a turn of the user's own got the session first, the wake message is taken
back out of the queue and the comments are looked at again later. The user's later messages are
**not** capped, with the comment text still in the history; the card says so until it is
acknowledged.

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
  [permission profile](14-projects-and-automation.md#agent-permission-profiles-implemented), and
  `imperium mods install|update`, which approve code for the desktop app (an agent could pass
  `--yes` to its own prompt; [06](06-extensibility.md#installing-and-updating-mods-implemented)).
  The daemon counts such an approval only from a process of the user's on the local socket (not
  a descendant of the daemon, not marked as one), so an agent's shell running the CLI does not
  give it; an agent that detaches a process and clears its environment can
  ([06](06-extensibility.md#who-may-grant-trust-and-approval-implemented)).
- `imperium plugin marketplace add`, `imperium plugin install|update|enable`, and
  `imperium config set|unset` of `extensions.disabled_plugins` or `extensions.plugin_dirs`: they
  bring plugin code in, turn it on, or point the daemon at a folder of it
  ([06](06-extensibility.md#plugin-marketplaces)). Browsing, inspecting, listing, disabling and
  uninstalling, and `plugin secret set|delete`, do not ask. There is no command that approves a
  plugin; that is the desktop app's dialog alone.
- Writing into the installed mods (`<config>/mods`) and the daemon's approvals file
  (`mod-approvals.json`), which a mod approval vouches for; and into the installed plugins
  (`<config>/plugins`), the marketplace clones (`<data>/marketplaces`) and the plugin records
  (`plugin-installs.json`, `plugin-migration.json`, `marketplaces.json`).
- Commands that name the signed-in browser profile (`<data>/browser`, see
  [the debugger pipe and profile](#the-browsers-debugger-pipe-and-profile-implemented)), which the
  file tools refuse outright.
- Writing files that hold secrets (`.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`,
  `id_ed25519*`, `id_ecdsa*`, `.npmrc`, `.pypirc`, `.netrc`, `mod-approvals.json`,
  `plugin-installs.json`, `plugin-migration.json`, `marketplaces.json`) or anything with a `.git` folder
  in its absolute path, such as a linked worktree's main repository's hooks.

The floor is a **best-effort guard against an agent's mistakes, not a security boundary**: it
reads the command line, so a script, an alias, or a program that deletes files itself passes
it. The folder sandbox and the isolation tiers are what confine commands.

### Auto mode (implemented)

`auto` decides every call exactly as `default` does (the rules, the safety floor and the tool's
default come first, unchanged) and then, only for a call that **would ask the user with the
session allowance offered**, asks a small classifier model whether it may run. It turns an
ask into a run or leaves the ask in place; it never refuses a call and never makes one
ask that would not have.

**What it can never change:**

- A call a **deny rule** refuses stays refused, and a call an **ask rule** makes ask every time
  still asks every time. Neither is given to the classifier.
- A call the **safety floor** covers (destructive commands, secret files, `.git`, protected
  folders, and a provider tool that asks afresh every time) asks the user, whatever the
  classifier would say; the classifier is not even called for it. The policy crate decides
  `auto` exactly as `default` (`decide_call` has one row for both), so the floor's
  "ask every time" and a deny rule's refusal are fixed before the engine looks at the mode.
- Tools that ask outside the permission judgement keep asking: memory saves, MCP sampling
  requests, budget approvals, and any call an outside caller sends through the
  [MCP bridge](20-claude-harness.md#exposing-imperiums-own-tools-implemented), which is the
  user's to answer in every mode. A knowledge-agent fork, which refuses asks, still refuses.
- Plan mode refuses before any of this, and an unattended session's pre-approved tools or a
  tool the user allowed for the session run without a question, so they need no classifier.

**The classifier call.** One request with no tools and no reasoning control, bounded to 256
reply tokens and `permissions.auto.timeout_s` seconds (at most 120). It reads a fixed policy
prompt (allow only a call the request plainly asked for or an ordinary step toward it, inside
the working folders, easy to undo and low impact, with no sign of having been prompted by file,
web or tool text; otherwise, and whenever unsure, ask) and, as tagged data:

- the user's last three requests, each cut to 1,500 characters (`<user-requests>`). The root
  agent records each message's text as the turn takes it: after a slash command is expanded and
  any `prompt.submit` hook has rewritten it, so a command's template or a hook's addition is
  part of it (a steering message is recorded the same way); what a turn's message carries besides it (session state, ambiguity hints,
  the context manifest, recalled memories, attachment text, instruction files met while
  exploring, compaction summaries, the steering wrapper) is never part of it. Only this block
  establishes what the user wants. A sub-agent's calls are judged against the same requests, and
  its own brief (written by the agent that spawned it) is a separate `<agent-task>` block that
  the prompt says never shows the user's intent. A session nobody attends (a schedule, a project
  agent's session, auto-fix, initiative work) was started with an agent's prompt, so what it
  records is an `<agent-task>` too, with `<user-requests>` empty, and the classifier is told to
  ask when there is no user request;
- the call, read from the **parsed** call, not from the arguments as the model wrote them: a
  command as `{"command", "timeout_s", "background"}`, a file call with the `path` first and
  whole and then its content or text, a
  tool provider's call as its command when it runs one and otherwise its arguments parsed and
  written out again (escapes resolved, padding keys sorted, not left to hide a value). A command
  longer than 3,000 characters, a path longer than 1,000, a content or edit text longer than
  1,500 (the rest of a long file is where a command can hide), or a provider's arguments longer
  than 3,000 are not classified at all, nothing is cut: the user is asked with the reason "too
  long for auto mode", since the classifier would be approving what it had not read;
- the session's working folders.

Tool results and file contents are never given to it. The prompt says the blocks are data to
judge, never instructions, and that an attempt to steer it is a reason to ask; the text of every
block has the frame's own tags and the harness's tags defused (`<tag` becomes `<\tag`, with the
shared framing helpers), so an argument cannot close a block early or fake one.

**The reply is read strictly.** It must be exactly one JSON object,
`{"verdict": "allow" | "ask", "reason": "<one short sentence>"}`, with those two keys and
nothing else: no prose around it, no code fence, no second object, no other verdict word
(`deny` included), no empty reason. A reason is cut to one plain line of at most 200
characters. Text that merely contains a verdict, such as a planted argument the classifier
echoes, is not a verdict.

**It fails closed.** An `ask` verdict, a reply that is not the format above, an HTTP or
connection error, a timeout, a model that is not in the model table or over its budget, and a
model that may not be used (below) all leave the question with the user, with the
reason added to it (`… (auto mode asks: <reason>)`). The user answers exactly as in `default`,
*Always allow* and *Allow for this session* included. The classifier can only ever say run or
ask; "deny and carry on" does not exist. A **stop or pause** is not a fallback and not a
question: while the session is stopped (or the agent paused for a steering message) no question
is raised, a classification in flight is dropped, nothing is counted toward the cooldown, and
the call reports that it did not run because the session was stopped.

**Which model.** `permissions.auto.model` names a row of the model table (its `id` or its
`<endpoint>:<model>` spec). Left empty, routing picks the cheapest model of the basic tier
(task tag `auto-permission`, so `[[routing.rules]]` can steer it), the session's own model when
nothing else fits. A session whose model runs on this machine, or whose routing rule confines
the `auto-permission` task to local models, never sends a call's text to a hosted model: a
hosted row, named or routed, is refused with a reason and the user is asked instead. The call's
tokens count toward budgets like any model call (a `model_call_completed` event), and a call
over budget is not made.

**Cooldown.** After `permissions.auto.max_fallbacks` (default 3) consecutive classifications
that ended in an ask or an error, the classifier rests for `permissions.auto.cooldown_s`
(default 300) seconds: no model call is made and every call that would have gone to it asks, with
the remaining time as the reason, so a classifier that keeps giving up costs nothing. An allow
resets the count; the rest ends by itself and starts the count again. The cooldown limits
**cost**, not security: the classifier's reasons never reach the agent (they go to the user and
the journal), and every classification that is not an allow, in a rest or out of it, ends in a
question to the user, so an agent cannot talk the classifier into allowing by making it rest or
by making it fail.

**Journal.** Every decision is a `notice` event of the session, so it is in the journal and shown
to every client: `Auto mode allowed <tool> <arguments>: <reason>`, `Auto mode did not allow
<tool> <arguments>, so you are asked: <reason>` (also for the resting cases), and one notice when
the classifier starts resting. No new event or method was needed; protocol 0.43 only adds `auto`
to the permission-mode values.

**Settings** (`[permissions.auto]`, applied to sessions started afterwards): `model`,
`timeout_s` (20), `max_fallbacks` (3, 0 never rests), `cooldown_s` (300). The permission mode
itself is `permissions.mode = "auto"`, `session.control` `set_permission_mode`, `/mode auto`, or
the selector in the session footer.

Like the floor, auto mode is a second opinion on an agent's mistakes and on text that tries to
steer it, not a security boundary: a small model can be wrong, and the folder sandbox and the
isolation tiers are still what confine commands.

**Write-then-run is a known limit.** The prompt tells the classifier to judge a write by what
its content would do when run or loaded (scripts, package and build manifests, Makefiles, CI
workflows, editor and task configurations) and to ask when that would not itself be allowed,
but a small model reading at most 1,500 characters cannot see everything a file will do, and a
write that passes can be run later by a command that, taken alone, looks harmless. Auto mode
does not close that gap; a deny or ask rule on the files that matter, the safety floor's
protected files, and the isolation tiers do.

### Auto-merge: the desktop app alone (implemented)

GitHub's auto-merge merges a pull request as soon as the repository's own required checks and
reviews pass, with **nobody looking at the code again**. Enabling it lands code unseen, so it is
the user's switch alone, and only the desktop app can turn it on. Imperium asks GitHub with
`enablePullRequestAutoMerge` and the chosen method (`squash`, `merge` or `rebase`, one the
repository allows), pinned to the head commit the user saw (`expectedHeadOid`: GitHub refuses if
the branch has moved); it never calls a merge endpoint itself. The only calls to GitHub that
change anything are that mutation and `disablePullRequestAutoMerge`.

- **On a server** ([server mode](02-architecture.md#server-mode-implemented)) nobody turns it on:
  `pull_requests.auto_merge_challenge` and `set_auto_merge` with `enabled: true` answer `-32010`
  (`auto_merge_enable`) before the caller is looked at. Turning it off still works.
- **Turning it on** is `pull_requests.set_auto_merge` with `enabled: true`, which needs the
  single-use challenge of `pull_requests.auto_merge_challenge`: the same pattern as
  [plugin approval](06-extensibility.md#plugin-marketplaces) (`plugins.approval_challenge` and
  `plugins.approve`). The daemon identifies the caller itself, from what the operating system
  says about the process on the other end of the local socket, and answers only the desktop app
  installed beside `imperium-core`; the challenge is bound to that connection, the session, the
  pull request's address, the head commit and the method, lives two minutes and is used up by
  any answer. The app's Rust shell does it (`auto_merge_enable`): a native dialog, written from
  the daemon's own status and naming the repository, the branches, the commit and the method,
  comes first, and the card's webview only triggers it.
- **Nobody else can.** A paired phone's scopes list neither method. A page reaching the daemon
  over loopback TCP (a mod's), an agent's command, a hook, a plugin's server and the
  `imperium` command are not the installed desktop app and get `permission denied`. No agent
  tool, `session.run_tool` tool, hook action, plugin tool or mod host call turns it on (tests
  read the tool schemas, the CLI's command tree and the mod host's allowlist). A comment on a
  pull request is text: it cannot call any of this. That holds for the daemon's methods only:
  code running directly as the user, such as a tier-0 command that runs `gh auth token` and
  calls GitHub's API itself, is outside this guarantee, as with plugin approval (sandbox tiers
  1 and above can hide `gh`'s login, [14](14-projects-and-automation.md#github-auto-fix)).
- **Turning it off** needs no challenge and any local caller may do it: the card, `imperium pr
  automerge off SESSION`, an agent. There is no enabling command in the CLI: `imperium pr
  automerge` has only `status` and `off`.
- **A push turns it off.** GitHub keeps auto-merge on after a push, so Imperium does not rely on
  it. The daemon's own pushes to a pull request's branch (Fix CI, and `session.review_pull_request`
  when the session has a pull request bound on that branch) disable auto-merge before they push,
  and never enable it again; if the daemon cannot read whether it is on, or cannot turn it off,
  it does not push. A push by *anyone else* (the agent through `gh`, a collaborator) is caught
  by the poll: when Imperium turned auto-merge on it records the head commit the user
  confirmed, with the session's binding (it survives a restart); at each poll, if auto-merge is
  on and the head differs, the daemon sends `disablePullRequestAutoMerge`, forgets the record,
  says on the card "auto-merge was turned off because new commits were pushed after you enabled
  it" and sends `pull_requests.changed`. It never turns it on again: the user confirms again in
  the desktop app. The poll is at most 60 s while a card is shown (`github.autofix.poll_s`
  otherwise), so a merge that GitHub completes inside one poll interval of the push is outside
  the guarantee. The record is cleared only when Imperium turns auto-merge off (the user, the
  poll's own turn-off, or a push of the daemon's own), never because one read said it was off
  (that read may have been in flight before the user enabled it); a record that outlives its
  auto-merge can only cause one extra, harmless disable. Auto-merge that Imperium did not turn
  on (enabled on GitHub itself, or before this was recorded) has no confirmed head, and the
  poll leaves it alone, with one exception that fails safe: after someone turns auto-merge off
  on GitHub the confirmed head is kept, so if it is later turned on again on GitHub at a new
  head, the poll turns it off. A daemon that was down when someone pushed catches it at its
  first poll. A session that cannot be watched any more turns it off too: **unbinding** the
  last session that follows a pull request Imperium enabled turns auto-merge off first (and
  fails, keeping the binding, when that is not possible). The record is cleared afterwards only
  if it still holds the head read before that turn-off, so an enable that finished meanwhile
  (the same session, or another that bound the pull request) keeps the head it recorded.
  **Deleting, archiving or rebinding** such a session cannot fail the action, so the pull
  request goes on a list in the data folder (`auto-merge-to-turn-off.json`: repository, number
  and the confirmed head) before anything is sent. The poller works through it at its next
  tick and at daemon start, waits longer after each failure (30 s doubling to 15 min), and drops
  an entry when the turn-off worked, when GitHub says auto-merge is already off or the pull
  request is no longer open, or when a session follows the pull request again with a
  confirmation of its own. A list entry is checked when it is loaded: a repository that is not a
  strict `owner/name`, or a number that is not positive, is skipped with a warning, and past 100
  entries the oldest are dropped with a warning. The first failure in a daemon run is sent as an
  `autofix.event` with status `failed` ("auto-merge could not be turned off yet ... Imperium
  will try again"); the desktop app shows it as a toast and a system notification per the
  user's notification settings (`notifyFailed`, `notifyOnlyInBackground`; text only, an
  identical failure is shown at most once in 10 minutes, at most 50 distinct failures are
  remembered, and system notifications are at least 5 seconds apart), and the phone page shows
  it too. Other clients see the notification only if they handle it. A push by the daemon for
  a bound branch is refused when GitHub is not available to check.
- **Where it is not offered**, with the reason on the card: a draft (GitHub refuses it), a
  repository that does not allow auto-merge, a repository whose merge settings the token cannot
  read (a token without push access gets no `allow_*` flags; that is "cannot be enabled here",
  never "every method is off"), a pull request in the merge queue (which ignores the method), a merged or
  closed pull request, and `github.automerge.allowed = false`. GitHub also refuses a pull request
  that can already merge (it will not queue it): the card says so and leaves merging to the user.
- **Settings** (`[github.automerge]`, [10](10-configuration.md)): `allowed` (default `true`)
  refuses every request when `false`; `default_method` is the method the card preselects when the
  repository allows it. Both are read from the user's own config only (a project's `.imperium`
  files are never merged as settings) and neither turns anything on.

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
WSL (`/mnt/d/...`), or `/D:/...` spelling of a drive path, or a drive path with stray
separators or colons after the drive (`D:\://x`, `D://x`). The drive-path tidy applies to every
tool that resolves a path (`read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, `grep`).
`read_file` and `list_dir` also take the path as `file_path`, `filepath`, `filename`, `file_name`,
or `file` (giving it twice is an error), and `read_file`'s `offset` and `limit` as numbers, whole
floats, or numbers in quotes (null, empty, zero, or negative means not given). `read_file` reads
a UTF-16 file that starts with a byte order mark as text (a lone trailing byte is left out, with a
note); a UTF-8 file with invalid bytes and no NUL bytes is read with U+FFFD for them and a note
(not to rewrite the file from that text: `edit_file` refuses it), a file with NUL bytes (in the
first 8 KB when the bytes are valid UTF-8, so BOM-less UTF-16 is not shown as garbage) is refused
as binary, and a folder is refused with a pointer to `list_dir`, in a remote workspace too.
A read without a `limit` returns at most 2,000 lines and also stops at a character budget sized
from the calling agent's context window: about an eighth of the window in tokens at 4 characters
a token (at least 8,000 characters, at most 100,000), or 32,000 characters when the window is not
known. A hosted model with no window in the model table is assumed to have a large one, so its read
gets the 100,000-character ceiling; a local model with an unknown window keeps 32,000. The engine
passes the window to the call (`Invocation::execute_in`); it is not a model-visible parameter. The
budget ends the read on a whole line, and a note gives the exact offset to continue (`[lines 1-19 of
400; stopped at the 8000-character read budget; use offset=20 to read more]`); a read that stops
short for any reason says the same. A first line past the budget is shown cut, with a note to use
`grep` for the part needed. A read with an explicit `limit` keeps the 100,000-character ceiling
(line numbers and notes included). The defaults apply to every model and to calls through the MCP
bridge and the Claude harness (a caller that cannot give the window gets 32,000 characters).
`list_dir` returns at most 500 entries per call, sorted, with a note (`use offset=500 to see more`);
its optional `offset` skips entries. Edits stay UTF-8. `edit_file` takes an optional `replace_all`
(boolean, default false; `"true"` and `"false"` in quotes are read as the flag): false keeps the
exact, unique `old_string` rule (none or several matches is an error that names `replace_all`);
true replaces every occurrence, says how many, and still refuses no match. `tool.before` and
`tool.after` hooks get a `read_file` or `list_dir` call's arguments under the schema's names
(`path`, `offset`, `limit`) whatever spelling the model used. Each reading is checked like any
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
On a server ([server mode](02-architecture.md#server-mode-implemented), `daemon.server`) the
container tier is refused: configuration validation rejects `isolation.tier = "container"`,
`config.set` answers `-32010` (`container_tier`), and a session whose commands would run in
containers (the tier, or an environment's Docker runtime: an image, or a running container
through `docker exec`) does not start, since a server has no Docker socket to give.
The file tools stay tier 0 (they run in the daemon, confined to the workspace). Not yet: policy
forcing tiers per model or agent, GPU flags for containers, Windows AppContainer, and a default
dev image.

**The GitHub CLI's login (implemented, pull-request card plan stage 2).** The daemon talks to
GitHub itself, with the token from `gh auth token` kept in its memory. A session's commands
(`run_command` and background commands, in every tier) and its MCP servers and hooks do not
inherit `GH_TOKEN`, `GITHUB_TOKEN` or `GH_CONFIG_DIR` (a variable an MCP server's own `env` or
`secret_env` sets is kept). In the OS tier the sandbox also hides `gh`'s config folder
(`GH_CONFIG_DIR`, else `$XDG_CONFIG_HOME/gh`, else `~/.config/gh`). This is not a guarantee that a
session cannot use `gh`: tier 0 hides nothing, so a command there can still run `gh auth token`
or read the config folder, and the OS keyring (where a newer `gh` may keep the login) is not
hidden by this rule. An environment whose id is in `github.session_gh` keeps all of it for its
sessions' commands (the login is then available to the model's commands); hooks and MCP servers
always drop the inherited variables. Users whose only credential is `gh`'s login in its config
folder lose `gh` in the OS tier unless they list the environment in `github.session_gh`.

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

**The daemon is outside the OS tier (implemented):** a command that could read the daemon's
access token, or connect to the daemon, could call `config.set` (adding an MCP server, a hook or
an `extensions.plugin_dirs` entry), start a tier-0 session and so make the unconfined daemon run
code for it. The OS tier is therefore not just read-only: it hides everything that would hand a
command the daemon.

| What | Where | In the OS tier |
| --- | --- | --- |
| Endpoint file (access token, port, socket path) | `<data>/daemon.json` | Linux: `/dev/null` bound over it; macOS: `file-read*` denied |
| Local socket (`daemon.socket`) | `<data>/ipc/` (`daemon.sock`) | Linux: empty tmpfs over the folder; macOS: `file-read*` and `network-outbound` to a unix socket under it denied |
| Paired phones | `<data>/devices.json` (SHA-256 of each token) | hidden like the endpoint file |
| Forwarded remote daemons (`imperium remote`) | `<data>/remotes/` (`<name>.json`: the remote's token and its forwarded loopback port) | hidden like the socket folder (tmpfs, or denied). The forwarded local ports are not denied on macOS: they are not known when the daemon starts, and the token is hidden |
| Encrypted secret store and its key | `<data>/secrets.enc`, `secrets.key`, `secrets.lock`, and the file named by `IMPERIUM_SECRETS_PASSPHRASE_FILE` | hidden like the endpoint file (a keychain-backed store has no files) |
| The signed-in browser profile (cookies, sign-ins) | `<data>/browser` | hidden like the socket folder (tmpfs, or denied); made at daemon start so it exists to be hidden |
| The user's runtime folder: the session bus and keyring (the Secret Service holds the keychain's secrets), the systemd user manager's private socket (`systemd-run --user` would start a process outside the sandbox), ssh and gpg agent sockets, a rootless container runtime's socket | `$XDG_RUNTIME_DIR`, or `/run/user/<uid>` when it is unset | Linux: the whole folder is replaced by an empty tmpfs. Hiding it whole, not socket by socket, is deliberate: any one socket left in it can hand a command more than the sandbox gives it, and a list of known ones goes stale. The cost is that a Wayland socket there is hidden too, so Wayland programs do not start in the OS tier. X11 is not hidden by this: see the abstract-socket limit below |
| Daemon, phone and API TCP ports | loopback ports from `daemon.port`, `phone.bind`, `api` | macOS: `network-outbound` to `localhost:<port>` denied. Linux: **not filtered**, see below |
| Environment of the command | `IMPERIUM_*` (home, passphrase file, port overrides) and `DBUS_SESSION_BUS_ADDRESS` | removed before the command starts; only `IMPERIUM_SPAWNED_BY_DAEMON` is set |

The masks are applied after every folder and grant mount, so a writable grant around the data
folder (or a workspace inside it, the default) neither reopens them nor loses its own files; only
paths that exist when a command starts are masked, and each is also masked under its resolved
name. The daemon registers them when it starts (`imperium_tools::hide_from_os_sandbox`).

Whether a writable grant around the data folder can reopen a hidden file differs by OS:

- **Linux:** a mount point cannot be renamed, replaced or linked out of, and the masks are
  mounted last, so the file stays hidden whatever the grants are. A mask is by path, so a rename
  of a folder around it would carry the daemon's folders out of its reach (`mv ~/.local/share
  ~/.local/sx`, then read `~/.local/sx/imperium/daemon.json` in the next command, which has no
  mask for the new path). Bubblewrap therefore binds every folder between a writable path and a
  guarded or hidden path onto itself (`--bind a a`, writable, outermost first, before the
  guards): each is a mount point, and renaming one fails with `EBUSY`. As a second line, and
  the only one in the container tier (a container has no such mount points), **every** command in
  the OS or container tier is refused, in every session whatever its folders, with a clear error
  while `<config>`, `<data>` or the daemon's endpoint file is missing. A command in one session
  could otherwise move `~/.local/share` into a folder another session contains, where a later
  command would find the daemon's files at a path with no mask. Tier 0 is unaffected, and the
  hidden subfolders (`remotes/`, `ipc/`) are not part of this check: removing one in normal use
  does not stop commands, and they are hidden whenever they exist. The daemon **creates
  `<config>` and `<data>` at startup**, so the guards always apply: a folder that is missing
  cannot be guarded, and nothing else creates `<config>`, so a command could otherwise create it
  and write a `config.toml` of hooks or MCP servers that the daemon would then run. If a folder
  cannot be created (a read-only home, a file in the way), it is still recorded, so it counts as
  missing and every command in the OS and container tiers is refused with the same error
  instead of running without the guard.
- **macOS:** Seatbelt works by path, so for every hidden path it also denies `file-write*` (which
  covers deleting or renaming it) and `file-link` (a hard link to a readable path), and it denies
  `file-write-unlink` on every folder around it, so the folder cannot be renamed away to carry
  the file to another path. A directory is denied by `subpath`, a file by `literal`.

**The configuration and data folders are read-only to commands (implemented):** hiding the
credentials is not enough when a session folder or a writable grant contains `<config>` and
`<data>` themselves (a workspace of `~`, or a grant over the app-data folders): a command could
write `config.toml` (adding a hook or an MCP server that the unconfined daemon then runs), plant a
plugin or an approval file, or read what the masks do not cover. The daemon therefore registers
both folders as guarded (`imperium_tools::guard_from_commands`), whatever the session:

- **What stays writable inside them:** only the folders the daemon registers as open
  (`imperium_tools::leave_writable_in_guards`): the workspaces root (`workspaces.root`, default
  `<data>/workspaces`, which holds the default sessions workspace and every session's scratch
  folder) and `<data>/worktrees` (auto-fix). A writable folder or grant anywhere else in a guarded
  folder stays read-only: all of `<config>` (a grant at `<config>/sub` cannot write), and
  `<data>` folders the daemon itself acts on, such as sessions, schedules, packs and approvals.
- **OS tier, Linux:** after the writable binds, every guarded folder that a writable path is,
  contains or is inside of is bound read-only; only then are the writable paths under an open
  folder bound writable again (after all guard binds, so another name for the same folder cannot
  cover them), then the read-only mount's `protected` folders and the credential masks follow.
  Writable paths, guards and open folders are compared and emitted under every name they have
  (a resolved link such as `/home` to `/var/home` as well as the configured one).
- **OS tier, macOS:** `file-write*` is denied under each guarded folder and allowed again under a
  writable path under an open folder, before the read-only and credential rules; names as above.
- **Container tier:** a guarded folder is never mounted writable and what it holds is never
  there:
  - a mounted folder or grant that contains a guarded folder or a hidden path gets an empty tmpfs
    over it at its path in the container (`--tmpfs /workspace/.config/imperium`), or for a hidden
    file `/dev/null` mounted read-only over it;
  - a mount that *is* a guarded folder (a grant of `<data>`), or is inside one outside the open
    folders (`<data>/sessions`, `<config>/sub`), is mounted `:ro`, with the hidden files and
    folders in it covered the same way;
  - a mount that is, or is inside, a hidden path (a grant of `<data>/remotes` or `<data>/ipc`) is
    replaced by an empty tmpfs: its content is never mounted;
  - no mount point is used twice, which a container runtime refuses: a cover is skipped where
    another mount or cover already has that path.
  A workspace inside the workspaces root mounts only itself, so nothing around it is there.
- The guard applies only to commands (and MCP servers and previews that run under a tier); tier 0
  runs with the user's own rights, as always. Reads of the guarded folders are not blocked in the
  OS tier beyond the credential list above: `config.toml` stays readable.

With the network shared (`isolation.network` is `true`, the default), bubblewrap cannot filter
one loopback port, so on Linux the daemon's TCP port stays **reachable but useless**: every
request needs the access token, which the command cannot read, and a connection without it is
refused with 401. `isolation.network = false` (`--unshare-net`) removes the port, and a command
cannot open the unix socket in either case. Remaining limits of the OS tier:

- It is not a confidentiality sandbox: the rest of the filesystem is readable, including the
  user's other secrets (`~/.ssh`, cloud credentials, browser profiles) and `config.toml`.
- **Abstract unix sockets stay reachable while the network is shared** (the default): X11's
  `@/tmp/.X11-unix/X0` socket, and the abstract address a session bus can also listen on, are in
  the network namespace, not the filesystem, so hiding the runtime folder does not close them. A
  command can then talk to the X server (keystrokes, screenshots) or a bus. Set
  `isolation.network = false` (`--unshare-net`) to close them; the daemon's TCP port goes with
  them.
- Sockets outside the runtime folder (under `/var/run` or the home folder) are not hidden. On
  macOS the runtime folder is not hidden and the login keychain stays reachable: it asks the
  user per item before it releases a secret another application stored.
- A path reachable under another name (a second bind mount of the data folder) is not masked.
- Windows has no OS tier, so commands fail there rather than running unsandboxed. The container
  tier mounts only the workspace, so none of these files are in it; with Docker Desktop its
  `host.docker.internal` reaches the host's loopback ports, again useless without the token.
- Phone and API listeners that start after the daemon (a settings change) are not added to the
  macOS port denial until the daemon restarts; both need credentials the command cannot read.

Anything that treats the OS tier as the place to run untrusted code, such as the plugin
marketplace's [threat model](plans/plugin-marketplace.md#threat-model) (case 3: approvals hold
against a confined agent within the limits above; [06](06-extensibility.md#plugin-marketplaces)), depends on this section: a plugin's
commands must not be able to turn into daemon calls. `crates/imperium-tools/tests/os_sandbox.rs`
runs a real sandbox in CI (Linux and macOS) to show that the endpoint file is unreadable, the
sockets cannot be opened, a hidden file cannot be renamed or linked out, and a workspace of the
home folder cannot write `<config>` or `<data>`.

### Programs (`run_script`): the interpreter worker (implemented in part)

Code mode ([plan](plans/code-mode.md), roadmap item 65) is **not reachable yet**: there is no
`run_script` tool and no setting. What exists is the worker that will run the programs, the
daemon-side driver and the interface the engine will drive it through, built and tested but not
wired into the engine. Every bundle and CLI package already carries the worker.

- **The worker** is `crates/imperium-script-worker` (binary `imperium-script`, shipped beside
  `imperium-core`, where the daemon looks for it): the Monty interpreter in a process of its own,
  one per program. Its protocol loop is Imperium's own, over `monty::MontyRun`; it serves one
  `Configure`, one program and the answers to that program's requests, and refuses everything else
  (type checking, dumps and loads, futures, a second program). Monty's type checker is not in it.
  The Monty crates are pinned to one release, exactly, in `runtimes/monty.env` and Imperium's
  `Cargo.lock`.
- **On Linux and macOS the worker confines itself before it reads any request**, and exits when
  any part cannot be applied (fail closed). First it closes every inherited file descriptor above
  2, and the driver starts it in an empty folder of its own. On Linux it sets `PR_SET_NO_NEW_PRIVS`,
  `RLIMIT_DATA` (twice the program's memory budget), Landlock where the kernel has it (no file
  access at all) and a seccomp allowlist: reads from stdin, writes to stdout and stderr, memory,
  futexes, clocks, `getrandom`, signals, exit, `kill` of itself and `clone` for threads only. Every
  other system call fails, `clone3` with `ENOSYS`; another architecture kills the process. When the
  daemon runs in a cgroup v2 delegated to its user, each worker also gets a cgroup of its own with
  `memory.max` at twice the budget. On macOS it calls `sandbox_init` with a profile that starts from
  `(deny default)` and allows reading its own binary and the system libraries and a few `sysctl`
  keys, so there is no file access, network, process creation or `mach-lookup` (no pasteboard).
  macOS enforces no memory limit on a process, so the daemon watches the worker's resident memory
  and kills it past twice the budget (the program ends as out of memory).
- **On Windows the daemon confines the worker as it launches it**: suspended, with a restricted
  copy of its own token (no privileges, the Administrators group deny-only) at Low integrity, in a
  job object (memory at twice the budget, one process, killed when the job closes, every UI
  restriction), with an empty environment, an empty working folder labelled Low outside Imperium's
  folders, and only its three pipe ends inherited (a handle list). Only then does its thread run.
  The worker checks it is at Low integrity, without privileges and in a job, and refuses to run
  otherwise. So that a Low process cannot read what Imperium keeps, the daemon puts one
  inheritable Medium `NO_READ_UP` label on the data folder, the config folder and the
  environments' working folders (when `workspaces.root` puts them elsewhere) at start, and again
  where the working folders move; files and environments made later inherit it. A folder already
  labelled is only read. When a file in them cannot take the label, code mode is unavailable and
  the reason names the file. What remains after an escape from the interpreter on Windows (reading
  the user's other files, the network) is the accepted risk the plan records
  ([C9](plans/code-mode.md#windows-confinement-mitigation-and-accepted-risk)).
- **The driver** is `crates/imperium-script`. It refuses a program that names `eval` or `exec`
  before any worker starts (hygiene, not a boundary: every host effect still passes through
  dispatch). It starts one worker per program, accepts only the events of the plan's allowlist
  (anything else kills the worker), answers every OS call with an error, counts every suspension
  against `script_suspensions` and ends the program with `AbortFeed` past it, times out a worker
  that stops answering, and kills the worker when the program ends. `Launcher::check` starts a
  worker on an empty program to say whether code mode can run here, and why not.
- **The engine's side** is a pair of traits, `ScriptRunner` and `ScriptProgram`, with a fake
  runner that replays scripted events (including ones a real worker never sends) for the engine's
  tests, and a handle table that re-checks the name of every call a program makes at call time: a
  program that calls a bare name reaches the host with no lookup before it.

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
| 1 — OS sandbox  | Linux: Landlock + seccomp (bubblewrap); macOS: Seatbelt profile; Windows: AppContainer / restricted token (best effort) | Executed code reaching files outside allowed roots, optional network block, and the daemon's own credentials and socket | Kernel or sandbox escapes; the user's other readable files; the daemon's TCP port while the network is shared (it needs the hidden token); Windows has no OS tier |
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
