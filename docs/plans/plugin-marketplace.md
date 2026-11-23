# Plan: plugin marketplace

Revision: 2026-11-23, revision 6 (plan review rounds 4 and 5: the OS tier's daemon escape and the
limits of PR 240, the container tier's folder precondition, pidfd start time, the Windows release
checks); written against `experimental` at `88e8fd50` (last updated). Roadmap
[item 51](../12-roadmap.md#next-in-order). Status: planned; no stage started.

Imperium already loads Claude Code plugins from folders and lets agents install one from a git
URL or a folder ([06](../06-extensibility.md#agents-manage-the-app-config-implemented)). What it
lacks is Claude Code's **marketplace**: a catalogue repository the user adds once, browses, and
installs, enables, disables, updates and uninstalls plugins from by name. This plan adds that,
with the trust model a download that can run code needs, on the CLI, the protocol and the
desktop app. It implements nothing; each stage below is one pull request.

## Goals

Each goal is boolean and has a check. "Fixture marketplace" means a git repository the tests
build in a temporary folder: `.claude-plugin/marketplace.json` listing one plugin by relative
path (commands, an agent, a skill, a `tool.before` hook and a stdio MCP server), one plugin by
a `url` source pointing at a second temporary repository, and one `strict: false` entry with no
`plugin.json`. Tests never use the network. "Counted approval" is defined in
[who may approve](#who-may-approve); what approvals protect against is in the
[threat model](#threat-model).

1. **Marketplaces.** A marketplace can be added from a git URL, a local folder, or a GitHub
   `owner/repo`; listed; refreshed to its source's newest commit; and removed. Anthropic's
   official Claude Code marketplace is pre-added. Check: the daemon tests of
   [stage 2](#stage-2-marketplaces-in-the-daemon) and, by hand,
   `imperium plugin marketplace add <fixture>` / `list` / `refresh` / `remove`.
2. **Browse.** Every plugin of every added marketplace is listed with its name, description,
   version, author, category and keywords, and the components it contains (commands, agents,
   skills, hook events, MCP servers), or a statement that the components are known only after
   inspecting an external source. Check: `plugins.browse` test over the fixture;
   `imperium plugin browse`.
3. **Lifecycle, per user and per environment.** A plugin is installed as `name@marketplace`,
   enabled or disabled for the user or for one environment, updated, and uninstalled (to
   `<data>/config-trash/`, undoable). Check: the tests of stages
   [3a-ii](#stage-3a-ii-inspect-and-install), [3b](#stage-3b-uninstall-list-and-undo) and
   [4](#stage-4-enable-disable-update-and-approve).
4. **Three surfaces.** Browsing, installing and managing plugins work from the protocol
   (`plugins.*`), the CLI (`imperium plugin …`) and a desktop Plugins window; **approving** code
   works only in the installed desktop app (U4). Check: the protocol method snapshot lists the
   methods in [protocol additions](#protocol-additions); `imperium plugin --help` has no approving
   command; the desktop checks of stages
   [9](#stage-9-desktop-plugins-window-marketplaces-browse-and-suggestions) and
   [10](#stage-10-desktop-install-trust-and-update).
5. **Nothing runs at add, refresh, browse, inspect or install.** Check: a fixture plugin whose
   hook and MCP server each write a marker file; after every one of those operations the marker
   is absent (stage 2 and 3a-ii tests).
6. **Code in `<config>/plugins` needs a counted approval, bound to content, for new sessions.**
   A new session loads the hooks or MCP servers of a plugin folder under `<config>/plugins` only
   when the daemon's record for that folder holds a counted approval of its exact content hash
   and of what it runs. That covers marketplace installs, agent installs, folders copied in by
   hand, and folders moved or renamed there. A change of content (an update, an edit, a rename, a
   link, a rewritten source file, a new kind of component) withholds the whole plugin from new
   sessions until a fresh counted approval, whatever `extensions.disabled_plugins` says. Running
   sessions keep what they loaded. Folders listed in `extensions.plugin_dirs` are outside this
   goal, as today, and so is code running unconfined as the user (U5). Check: stage 3a-i and 4
   tests.
7. **Claude Code compatibility.** An unmodified Claude Code marketplace and its plugins load:
   relative, `github` and `url` plugin sources, `plugin.json` component paths and inline
   `mcpServers`, and marketplace entries with `strict: false`. Check: stage 1 parser tests,
   stage 3a-ii's `strict: false` install test; by hand in
   [stage 11](#stage-11-agents-docs-and-close-out), the official marketplace.
8. **Secrets in the keychain.** A value a plugin's MCP server needs is stored in the secret
   store and never written to a config file, the plugin folder, the journal or a log. Check:
   stage 5 tests.

## User decisions

Made by the user on 2026-11-22 (U1 to U3) and 2026-11-23 (U4 to U7).

- **U1. The official marketplace is pre-added.** Anthropic's official Claude Code plugin
  marketplace is a built-in entry of `plugins.marketplaces`. Its exact source is not recorded in
  this repository, so the plan names it **"the official marketplace source, confirmed at
  implementation"**; [stage 2](#stage-2-marketplaces-in-the-daemon) confirms it from Anthropic's
  Claude Code documentation, records the URL in this plan and pins it in the build, and
  [stage 11](#stage-11-agents-docs-and-close-out) checks it against the live repository. Its name
  is reserved for that pinned source, even while the user has it turned off, and "built-in" is
  decided by source, not by name. Pre-adding it grants no trust.
- **U2. A trusted project may suggest marketplaces and plugins.** A project folder in
  `mcp.trusted_projects` may list, in its `.imperium/config.toml`, marketplaces and plugins to
  **offer** the user. Offers are shown only; they never add a marketplace, install, approve or
  enable anything ([stage 7](#stage-7-repository-suggestions)).
- **U3. Plugin hooks block, never rewrite.** For every plugin under `<config>/plugins`, hooks
  may block but never rewrite a tool call or prompt, including after an approval, for now.
- **U4. Only the installed desktop app's native dialog approves.** An approval that lets a
  plugin's hooks or servers run counts only when it comes from the native dialog of the
  installed desktop app, identified by the daemon as described in
  [who may approve](#who-may-approve). CLI approvals do not count until roadmap
  [item 30](../12-roadmap.md#next-in-order)'s stronger caller check exists: a process detached
  with a clean environment passes `peer::may_grant` (as `crates/imperium-core/src/peer.rs`
  documents) and could approve with no dialog at all.
- **U5. The threat model has a documented limit.** Approvals guard against the user accidentally
  enabling a bad plugin, against callers that reach only the protocol, and against agents
  confined by the OS or container tier. Agents and other code running **unconfined at tier 0** (the
  default, and the only command tier on Windows) are **out of scope**: they already run arbitrary
  code as the user and can write the approval store and the config directly. Checks that would
  defend only against such code are dropped or kept only where they are cheap
  ([threat model](#threat-model)). Review round 4 found that the OS tier did not confine an agent
  from the daemon; [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240), merged at
  `a58b0efa`, fixed that, and the OS tier now protects approvals within #240's limits (case 3).
- **U6. An OS presence check where available.** After the dialog, the daemon itself raises an OS
  presence check (Touch ID or the password on macOS, Windows Hello, polkit on Linux) where the OS
  offers one without setup; a result reported by the app would mean nothing. Where none is
  available, the dialog alone counts and the approval is labelled `dialog-only`
  ([presence](#presence-check-per-os)).
- **U7. No new code-plugin approvals on Windows until releases are signed.** Until Windows
  release builds are Authenticode-signed and installed per-machine (roadmap
  [item 59](../12-roadmap.md#next-in-order)), no new approval counts on Windows. Carried-over
  approvals and plugins without hooks or servers keep working.

## Threat model

**What approvals protect against (in scope):**

1. **The user's own mistakes.** Enabling a plugin with code without seeing what it runs, or
   keeping an approval after its content changed. The trust prompt, the content-bound approval
   and the re-ask on change cover this.
2. **Callers that reach only the protocol.** Anything holding the daemon token over loopback TCP,
   a paired phone, a mod frame, and every agent tool the daemon runs (`install_extension`, the
   config tools). None of them can approve: approvals come only over the local socket, from the
   app's process.
3. **Agents confined by the container tier, and by the OS tier within #240's limits.**
   - **Container tier:** protects approvals when its preconditions hold: the container has no
     host networking (`--network none` or a bridge network, never `--network host`) and the
     daemon's socket is not mounted into it (it mounts only the workspace), and **no session
     folder or path grant contains `<config>` or `<data>`**. This plan relies on
     [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`) to
     enforce that last precondition for container sessions; it does not restate the mechanism. Such an agent cannot
     write `plugin-installs.json`, the config or `<config>/plugins`, cannot read the daemon token,
     and cannot reach the daemon.
   - **OS tier: within #240's limits.** Before
     [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240), bubblewrap's read-only root
     (`--ro-bind / /`) and the shared host network let an OS-tier agent read `daemon.json`'s token,
     reach the daemon over its socket or loopback TCP, and ask the **unconfined** daemon to act for
     it (for example `config.set` of `extensions.plugin_dirs`, an ungated plugin folder whose hooks
     then run unconfined). It could not *approve* (the daemon refuses its process,
     [check 3](#who-may-approve)), but it did not need to. #240 (`fix/os-tier-daemon-escape`,
     merged at `a58b0efa`) closes this. Among other things it hides `daemon.json`, `ipc/`,
     `remotes/`, `devices.json` and the secret files; masks the runtime folder; strips
     `IMPERIUM_*` and `DBUS_SESSION_BUS_ADDRESS` from the environment; guards the config and data
     folders read-only; and makes the sandbox fail closed when a registered path is missing (see
     #240 for the full list). The OS tier therefore protects approvals **within #240's limits**.
     Its known remaining limits include abstract unix sockets (an X11 display, for example,
     through which an agent could click a `dialog-only` approval), other files of the user such as
     `~/.ssh`, the macOS runtime folder and keychain, and that Windows has no OS tier
     ([remaining gaps](#remaining-gaps)). The full, current detail is in
     [05 Isolation tiers](../05-tools-and-isolation.md#isolation-tiers).

**What is out of scope (U5):** code running **unconfined as the user**. That is every agent
command at tier 0 (the default on every platform, and the only command tier on Windows, where the
OS tier does not exist yet), every hook and stdio MCP server of any plugin, and any other program
the user runs. Such code can already do anything the user can: edit `plugin-installs.json` and
the migration marker, the config and the plugin folders; replace files of a per-user install;
inject into the desktop app (Windows process injection, `WEBVIEW2_*` variables or DLL search
order when it launches the app, `ptrace` by the app's parent on Linux); and drive the dialog with
input automation. Approvals are **not a boundary** against it, and no surface says they are. The
safety floor asks about an agent's commands and writes that touch these files, as a guard
against mistakes, not a defence ([05](../05-tools-and-isolation.md#permissions)). A user who
wants approvals to hold against their agents runs those agents under the container tier
(without host networking, and with no session folder or grant containing `<config>` or `<data>`),
or under the OS tier within the limits of
[#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`; case 3).

**Proportionality.** The checks below are kept because they matter for an in-scope case, or are
cheap one-liners that close common accidental paths. Runtime code-signature verification of the
calling app, debugger detection, and requiring a root-owned install on Linux were in revision 3
and are **dropped**: they defended only against same-user code at tier 0.

## Scope and non-goals

In scope: the marketplace format and the plugin manifest as Claude Code publishes them (the
public format, from Imperium's existing parser and public documentation; no external site was
fetched for this plan), marketplace storage, inspection and approval, install and lifecycle,
plugin secrets and child environments, repository suggestions, CLI, protocol and desktop
surfaces (including the daemon's identification of the desktop app and the OS presence check),
and the agent tool `install_extension` taking `name@marketplace`.

Non-goals (each with what would reopen it):

- **Defending approvals against unconfined same-user code** (U5). Reopen when a tier confines
  agents by default on every platform.
- **A hosted Imperium registry** or search across marketplaces the user has not added. Reopen when
  there is a service to host it.
- **Automatic updates.** Refresh reports what changed; updating is always an explicit action.
- **Signature verification of plugins** (see
  [provenance](#provenance-and-signatures-declined-for-now)).
- **A marketplace served as a bare `marketplace.json` URL**, and **npm or pip plugin sources**.
  They are reported as unsupported with the source type named. Reopen when a marketplace the user
  wants is published only that way (including the official one, if stage 2 finds it is).
- **Sandboxing plugin hooks and MCP servers.** They run as today: as the user, outside the
  isolation tier ([assumptions](#assumptions)). Reopen with roadmap work on running local servers
  and hooks inside the isolation tier ([05](../05-tools-and-isolation.md#mcp)).
- **Claude Code command features that run code**: `!` shell lines in command templates and
  `allowed-tools` front matter. Imperium's commands stay text; the inspection flags them.
- **Repository-scoped installs.** A repository's `.imperium/config.toml` cannot add marketplaces,
  install, approve or enable a plugin; it may only suggest (U2). A trusted repository can still add
  `extensions.plugin_dirs` whose plugins load ungated, as today
  ([14](../14-projects-and-automation.md#environment-configuration-implemented)).
- **Applying plugin changes to running sessions or without a daemon restart.**
- **Imperium-native `imperium-plugin.toml` manifests and plugin permission manifests**
  ([06](../06-extensibility.md#permissions-model-for-extensions)).
- **Hook rewrites** (U3), **CLI approvals** (U4, reopen with item 30), and **approving on a machine
  without the installed desktop app** (headless, SSH-only, phone).
- **The trusted main-context mod loader.** Whether this plan's identification of the desktop app
  is enough for main-context mods stays item 30's open decision, blocked on the user.

## Assumptions

Checked against the source at `88e8fd50`:

- `imperium-extensions::plugins` loads a plugin from a fixed layout (`skills/`, `commands/`,
  `agents/`, `hooks/hooks.json`, `.mcp.json`) and reads only `name` and `description` from
  `plugin.json`; custom component paths and inline `mcpServers` are ignored, and a folder without
  `.claude-plugin/plugin.json` is not a plugin.
- `imperium-core::extension_install` does a safe fetch: shallow `git clone` without submodules,
  LFS, prompts or the `ext::` transport, a 5-minute limit, a 256 MiB copy without `.git` or
  symbolic links, an unpredictable staging folder (`Staging`), and `.imperium-source.json`
  provenance. It refuses to install over an existing name. A plugin with hooks or servers lands
  in `extensions.disabled_plugins`. `fetch` and `Staging` are `pub(crate)`; `git_fetch`, `git` and
  `copy_tree` are private.
- **Today's "downloaded" status is plugin-controlled.** `plugins::installed_by_agent` reads
  `.imperium-source.json` inside the plugin folder. This plan replaces it with a daemon-owned
  record ([stage 3a-i](#stage-3a-i-record-store-and-the-load-time-gate)).
- **Today's switch is uncounted.** The Connectors switch turns a plugin on by editing
  `extensions.disabled_plugins` through `config.set`, which any holder of the daemon token can
  call. So can `extensions.plugin_dirs`.
- `imperium-core::mod_install` has the inspect-then-install pattern with a content hash
  (`tree_sha256`), a daemon-owned approval store (`mod-approvals.json`) and a `modified` state.
  `crate::peer::may_grant` is not a boundary (its module documentation says so).
- **The OS tier** (`crates/imperium-tools/src/sandbox.rs`) runs commands under bubblewrap with
  `--ro-bind / /`, writable binds for the workspace only, `--unshare-pid`, and `--unshare-net`
  when network is off (with network on it shares the host network); or Seatbelt on macOS. Since
  [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`) it hides
  the daemon's token, socket and secrets from commands and guards the config and data folders
  read-only, within the limits in [05](../05-tools-and-isolation.md#isolation-tiers). It is not
  available on Windows. The container tier
  mounts only the workspace. Hooks and MCP servers never run in a tier.
- The desktop app's Rust side talks to the daemon with `imperium-client`, which tries the local
  socket first (`apps/desktop/src-tauri/src/daemon.rs`); it depends on `tauri-plugin-dialog`.
  Installers ship `imperium-core` beside the app.
- **Release signing today** ([11](../11-ci-cd.md#secrets)): macOS builds are signed and notarised
  when the Apple secrets are set; Windows builds are not code-signed and the NSIS installer uses
  Tauri's per-user default; Linux packages are not signed.
- Plugin MCP servers have an empty `allow` list, so each of their tool calls asks unless a
  permission rule allows it.
- **Hooks and stdio MCP servers inherit the daemon's whole environment.**
  [06 Hooks](../06-extensibility.md#hooks) says hooks run inside the session's isolation tier, but
  `hooks.rs` runs them with `imperium_tools::shell`, and local MCP servers are not confined either
  ([05](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented)).
  Only `${VAR}` **expansion** in a downloaded plugin's `.mcp.json` is restricted today. Stage 3a-i
  fixes the 06 line; stage 5 adds an environment allowlist.
- `extensions.disabled_plugins` is read when the daemon starts.
- Environments are user-owned records in `<data>/environments.json`. A repository's
  `.imperium/config.toml` may only narrow, except that a folder in `mcp.trusted_projects` may add
  `extensions.plugin_dirs` and `[mcp.servers]` and disable plugins
  ([14](../14-projects-and-automation.md#environment-configuration-implemented)).
- Secret names are 1 to 64 characters of lowercase letters, digits, `.`, `_` and `-`
  ([10](../10-configuration.md#secrets)).
- The protocol is at 0.47 on `experimental`. Every stage that adds protocol takes **the next
  minor at its merge**.
- Rust is built and tested in CI only (`bash scripts/check-rust.sh`); locally run only
  `cargo fmt --all --check`. Web checks: `bash scripts/check-web.sh` after `pnpm install`.

## Relevant paths

- `crates/imperium-extensions/src/plugins.rs`, `lib.rs`, `hooks.rs`; `crates/imperium-mcp/`
- `crates/imperium-core/src/extension_install.rs`, `mod_install.rs`, `connectors.rs`,
  `state/extensions.rs`, `environments.rs`, `peer.rs`, `config_changes.rs`, `rpc.rs`
- `crates/imperium-config/src/settings.rs`, `environment.rs`, `mcp_servers.rs`
- `crates/imperium-protocol/src/methods/` (new `plugins.rs`), `lib.rs` (`PROTOCOL_VERSION`),
  `snapshots/methods.txt`, `packages/protocol/src/generated/`
- `crates/imperium-policy/src/permissions.rs` (`FLOOR_PATHS`, `protect_dir`), `floor.rs`
- `crates/imperium-tools/src/sandbox.rs` (the OS tier, for the threat model)
- `crates/imperium-cli/src/main.rs`, `mods.rs` (prompt pattern), new `plugins.rs`
- `apps/desktop/src-tauri/src/`, `apps/desktop/src-tauri/tauri.conf.json`,
  `apps/desktop/src/connectors/` (the pattern), new `apps/desktop/src/plugins/`
- `.github/workflows/build.yml` (signing and entitlements)

## Related docs

[06 Extensibility](../06-extensibility.md), [05 Tools and isolation](../05-tools-and-isolation.md),
[10 Configuration](../10-configuration.md), [14 Environments](../14-projects-and-automation.md#environment-configuration-implemented),
[07 Desktop](../07-desktop-ui.md#connectors-implemented), [11 CI/CD](../11-ci-cd.md#secrets),
[02 Protocol](../02-architecture.md#protocol). For behaviour already built, those docs win; for
anything planned here, this plan wins until its stage merges and updates them.

## Decision

**Chosen: a marketplace layer on the existing fetch, a daemon-owned record per plugin folder,
and approvals that count only from the installed desktop app's dialog.** Marketplaces are git
clones cached in the data folder; installs reuse `extension_install`'s fetch and land in
`<config>/plugins/<name>`; the daemon keeps a record per folder (origin, source, commit, content
hash, and the approval), and the session loader withholds any folder under `<config>/plugins`
with code that does not match a record with a counted approval, by folder path, whatever the
plugin's own files or `extensions.disabled_plugins` say.

Rejected:

- **Only extending `install_extension` with more source forms.** Fails goals 2, 3 and 6.
- **Driving Claude Code's own `claude plugin` CLI or sharing `~/.claude/plugins`.** Imperium must
  work without Claude Code installed, and Claude Code's trust decisions are not Imperium's.
- **Two installed plugins of the same name from different marketplaces.** Skills, commands,
  server names, `extensions.disabled_plugins` and agent profiles key on the plugin name. A taken
  name is refused; the folder name must equal the manifest name.
- **Approving only the hook and server command lines.** Hooks run scripts inside the plugin; the
  approval covers the content hash and the run summary.
- **Trusting `.imperium-source.json` or the manifest name for gating**, and **gating only recorded
  folders** (moving a folder would escape it).
- **Approvals from any token holder, or from `may_grant` callers** (U4).
- **Verifying the app's code signature at run time, debugger checks, root-owned Linux installs**
  (U5: they defended only against unconfined same-user code).

## Security model

Installing a plugin is installing code from the internet: once enabled, its hooks run on every
matching event and its stdio MCP servers run as processes of the user, outside any isolation
tier. Its skills and commands are text that steers agents. The controls below apply within the
[threat model](#threat-model).

### Nothing runs before approval

- Adding or refreshing a marketplace clones it (the existing safe `git` invocation) and parses
  JSON. Browsing reads the cached clone. Inspecting fetches the plugin to a staging folder and
  parses it. Installing **fetches again**, checks that the content still has the inspected hash,
  and moves that copy into place. None of these runs a hook, a server, a build step, an install
  script, or a git hook.
- A plugin with hooks or MCP servers is installed **withheld**; it loads only after a counted
  approval. Skills-and-commands-only plugins load once installed, bound to their recorded hash.

### The trust prompt shows exactly what runs

`plugins.inspect` returns, every surface shows before installing or updating, and the desktop's
native approval dialog repeats in short:

- **Identity and provenance:** `name@marketplace`, version, the marketplace's source and commit,
  the plugin's own source (the marketplace's repository or a third party's) and exact commit, and
  author, homepage and licence **labelled as the plugin's own unverified claims**.
- **Every hook:** event, matcher, and the full command line or URL, with `${CLAUDE_PLUGIN_ROOT}`
  expanded; all can block, none can rewrite (U3).
- **Every MCP server:** stdio command and arguments, or remote URL and header names; the
  environment variables and `${VAR}` references it uses, and which are secrets.
- **Launchers that fetch more code at start** (`npx`, `uvx`, `bunx`, `pipx run`, `docker run`
  with an image not pinned by digest, any `@latest`), flagged. **A missing flag is not a
  guarantee:** any hook script or server can download and run code on its own.
- **Text components:** commands, agents and skills by name; commands with `!` shell lines or
  `allowed-tools` flagged as not run by Imperium.
- **Content hash:** SHA-256 over the plugin folder's files (paths and bytes, without `.git` and
  Imperium's own `.imperium-source.json`), including `.claude-plugin/marketplace-entry.json`
  ([stage 3a-ii](#stage-3a-ii-inspect-and-install)).
- **For an update:** a diff of hooks and servers, and a statement that other files changed.

### Pinning

- An install pins the plugin to the commit fetched (a marketplace entry's `sha` or `ref` is
  honoured; otherwise the default branch's head at inspect time). The marketplace clone is pinned
  to the commit of its last refresh. Neither moves without an explicit refresh or update.
- `--sha256` (CLI) and `sha256` (protocol) pin the content hash a previous inspection showed.

### Updates re-ask

- Any content change of a plugin with hooks or servers in either version installs the new
  content **withheld** until a fresh counted approval. A plugin with neither installs and loads.
- An update reads the source from the daemon's record, never from the plugin folder, and refuses
  a different marketplace or source.

### The load-time gate

At session start the daemon looks at every folder under `<config>/plugins`. It withholds the
**whole plugin** (skills and commands too) from the new session, whatever
`extensions.disabled_plugins` says, when any of these holds:

1. the folder has hooks or servers and **no record**: reason "not installed by Imperium: approve
   to load". Approving it in the desktop app writes a record with `origin = user`;
2. the folder name, the recorded name and the manifest name are not all equal;
3. the folder contains a symbolic link or a reparse point (junction) anywhere;
4. its content hash differs from the recorded one;
5. it has hooks or servers and there is no counted (or carried-over) approval, or the approval's
   hash or run summary (`runs()`, [stage 1](#stage-1-marketplace-and-manifest-parsers)) differs
   from what is there now.

An unrecorded folder with neither hooks nor servers loads as today. Gating is by canonical folder
path. A withheld plugin is reported with the reason. The hash is computed for every folder when
the daemon starts and kept in memory with its cache key: the sorted list of every file's relative
path, size and modification time; a session start re-walks that list and rehashes a folder whose
list differs.

**Migration.** At stage 3a-i's first daemon start, every folder already in `<config>/plugins` is
recorded with `origin = preexisting` and its current hash and run summary; a folder not in
`extensions.disabled_plugins` gets a **carried-over** approval, so nothing the user runs today
stops working. The daemon then writes a **migration marker**, `<data>/plugin-migration.json`,
separate from the store. If the store is missing or unreadable while the marker exists, the daemon
does not migrate again: every folder with code is withheld until approved. Deleting both files
re-runs the migration and carries over whatever is present; only unconfined code can do that,
which is out of scope (U5).

### Provenance and signatures (declined for now)

Claude Code marketplaces carry no signatures, and verifying git commit signatures needs an
allowed-signers list nobody publishes. This plan **declines signature verification of plugins**
and shows provenance instead. Revisit when the format adds signatures or the user wants
`git verify-commit` against a signers file they maintain.

### Who may approve

**Only the installed desktop app's native dialog (U4), within the [threat model](#threat-model).**
An approval is **counted** when all of these hold, checked by the daemon:

1. **Local socket only.** Never loopback TCP, a paired phone or a mod frame. *(Protocol-only
   callers.)*
2. **The peer is identified without races.** Windows: at accept, the daemon opens the client
   process (`GetNamedPipeClientProcessId`, then `OpenProcess`) and holds the handle for the
   connection; the process's creation time must be no later than the accept, and the handle is
   checked again at approve. Linux: `SO_PEERPIDFD` at accept (kernel 6.5 and later), else
   `pidfd_open` on the `SO_PEERCRED` pid immediately at accept; all later reads go through the
   pidfd. Because `pidfd_open` can still open a process that reused the pid in between, the
   daemon reads the process's start time (`/proc/<pid>/stat` field 22, read while the pidfd shows
   the process alive) and refuses a process that started later than the accept, as on Windows.
   macOS: the connection's audit token. *(A reused process id cannot stand in for the app.)*
3. **The peer is the installed app, outside any sandbox.** Its executable path is the desktop
   app installed beside the running `imperium-core` (on macOS read with the audit token); on Linux
   its mount and pid namespaces equal the daemon's (an OS-tier sandbox has its own); on macOS it is
   not sandboxed (`sandbox_check` on its pid via the audit token). *(The CLI, and confined agents
   that reach the daemon, for example through one of the OS tier's remaining limits.)*
4. **Cheap refusals.** The peer's environment holds no `WEBVIEW2_*` variable
   (`WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, …), no
   `LD_PRELOAD`, `LD_LIBRARY_PATH` or `LD_AUDIT`, and no `DYLD_*`. The app calls
   `SetDefaultDllDirectories` at start on Windows, and CI checks that macOS release entitlements
   leave out `com.apple.security.cs.allow-dyld-environment-variables`,
   `com.apple.security.cs.disable-library-validation` and WebKit inspection. *(Common accidental
   and casual injection paths; not a boundary against unconfined code, U5.)*
5. **A challenge bound to content.** The daemon issued a single-use challenge to that same
   connection for this action, folder, hash and run-summary digest, valid two minutes; at approve
   it recomputes the folder's hash and run summary and requires them to equal the challenged
   values. *(Replay, and approving other content than the dialog showed.)*
6. **The OS presence check passed** where available (U6, [below](#presence-check-per-os));
   otherwise the approval is stored as `dialog-only`. *(Accidental clicks, and anything that can
   click but cannot give the user's fingerprint, PIN or password.)*
7. **Windows only (U7):** the daemon is a release build compiled with a **compile-time constant**
   that only the signing job sets (for example `option_env!` of a variable defined only in that job
   step, read at build time; never a file or an environment variable read at run time), and its
   own executable lies under the per-machine Program Files folder as returned by
   `SHGetKnownFolderPath(FOLDERID_ProgramFiles)` (never the `%ProgramFiles%` variable, which the
   caller's environment controls). Until roadmap
   [item 59](../12-roadmap.md#next-in-order) ships both, no new approval counts on Windows.

The desktop's flow: the Plugins window asks the app's Rust side through a Tauri command
(`plugin_approve`); the Rust side requests a challenge, shows a **native dialog**
(`tauri-plugin-dialog`) with the plugin id, what it runs and the hash, and on *Approve* sends the
approval with the challenge over its own local-socket connection; the daemon then raises the
presence check. The webview never sends an approval.

**Dev builds.** A release daemon never counts an approval from a debug build of the app. A debug
daemon (`cfg(debug_assertions)`) uses a development verifier that accepts the app built in the
same cargo target folder; its approvals are stored as `dev`, shown as `dev` in `plugins.list`, and
ignored by release daemons. On a debug daemon approvals protect nothing
([gaps](#remaining-gaps)).

**The CLI records nothing that counts.** The CLI has no approving command; any approval that
fails the checks records nothing (`approved: false`, with the reason). CLI commands that remain:
`plugin marketplace add|list|refresh|remove`, `plugin browse`, `plugin inspect`,
`plugin install` and `plugin update` (a plugin with code lands withheld, with a pointer to the
desktop app), `plugin enable|disable` (enabling works only for an approved plugin),
`plugin uninstall`, `plugin list`, `plugin secret set|delete`, and `plugin suggestions`.

**Agents.** `install_extension` (any source, including `name@marketplace`) installs as today:
withheld when the plugin has hooks or servers. An agent tool call cannot approve (case 2 of the
threat model). A container-tier agent, within its preconditions, cannot write the store, read the
token or reach the daemon (case 3). An OS-tier agent cannot pass the identity check either, and
since [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`) it
cannot read the token or write the config; it is confined within #240's limits (case 3). An
unconfined agent can do what the user can (U5). The safety floor asks about an
agent's shell running `imperium plugin marketplace add`, `imperium plugin install|update|enable`
and `imperium config set|unset` of `extensions.disabled_plugins` or `extensions.plugin_dirs`, and
about file-tool writes into `<config>/plugins`, `<data>/marketplaces`, `plugin-installs.json` and
`plugin-migration.json` ([stage 6](#stage-6-cli-and-the-safety-floor)), as a guard against
mistakes.

**Turning a plugin off and on again** does not revoke its approval; `plugins.set_enabled` may
turn an approved plugin back on from any caller.

### Presence check per OS

The daemon raises the check itself, after a valid challenge and before recording; a presence
result reported by the app or any other caller is never accepted. Feasibility, from platform
documentation and not yet prototyped (stage 8c prototypes each one first):

| OS | Mechanism | Available without setup | Feasibility from `imperium-core` |
| --- | --- | --- | --- |
| macOS | LocalAuthentication, `LAContext.evaluatePolicy(.deviceOwnerAuthentication)` (Touch ID, else the login password), with the plugin id in the reason text | Always (the password is always set) | Expected to work from a per-user, non-bundled process in the login session, since the system draws the prompt. If the prototype shows it needs an app bundle, the daemon runs it in a small helper it ships and spawns, and reads the result from the helper's exit (a confined agent cannot interpose on that; unconfined code is out of scope) |
| Windows | Windows Hello, `UserConsentVerifier` through `IUserConsentVerifierInterop::RequestVerificationForWindowAsync` | Only when Hello (PIN or biometrics) is set up | **Unverified.** The desktop interop needs a window handle and the prompt must reach the foreground, which a windowless background daemon may not get. The prototype tries a daemon-owned top-level window. If that does not work, Windows has no daemon-raised presence check: approvals count on the dialog alone, labelled `dialog-only`. Moot until U7's preconditions hold |
| Linux | polkit `CheckAuthorization` over the system bus with `AllowUserInteraction`, subject the daemon's own process, for an action `org.imperium.plugins.approve` with `auth_self` (no `_keep`, so every approval asks) | When the `.deb` or `.rpm` installed the action file under `/usr/share/polkit-1/actions` and a polkit authentication agent runs in the session (desktop sessions do; headless and AppImage installs do not) | Feasible with a D-Bus client in the daemon; unavailable cases fall back to `dialog-only` |

Within the threat model the check matters for accidental approvals and for confined code that can
reach the user's display but not their fingerprint, PIN or password; it is not a boundary against
unconfined same-user code, which could also run its own prompt or tamper with the daemon (U5).

### What an approved plugin still cannot do

- **Permission rules, the safety floor and agent profiles are unchanged.** Its MCP tools ask
  unless a user rule allows them; floor-covered calls ask every time; an agent's `plugins`
  allowlist and local-files-only lock
  ([14](../14-projects-and-automation.md#agent-permission-profiles-implemented)) keep its servers
  and hooks out of that agent's sessions.
- **Hooks block but never rewrite** (U3).
- **No daemon environment through `${VAR}`**, and from stage 5 an allowlisted process
  environment. Until then hooks and servers inherit the daemon's whole environment.
- **A repository cannot install or enable one** (it may suggest, U2, and disable).

### Remaining gaps

Stated plainly, so no surface claims more:

- **Unconfined same-user code is out of scope (U5).** It can edit the store and marker, the
  config and the plugin folders, delete both migration files to re-run the migration, inject into
  the app or launch it with `WEBVIEW2_*` variables, DLL search-order tricks or `ptrace`, and drive
  the dialog. Agents at tier 0, the default and Windows' only tier, are such code.
- **The OS tier confines an agent from the daemon only within #240's limits.**
  [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`) hides the
  daemon's token, socket and secrets from OS-tier commands; its limits include abstract unix sockets (such as an X11 display, so an
  OS-tier agent could still click a `dialog-only` approval's dialog), other user files such as
  `~/.ssh`, the macOS runtime folder and keychain, and no OS tier on Windows; see
  [05](../05-tools-and-isolation.md#isolation-tiers). The container tier protects only without host
  networking, without the daemon's socket mounted, and with no session folder or grant containing
  `<config>` or `<data>` (enforced by #240).
- **`dialog-only` approvals** (Windows if Hello is infeasible or not set up, Linux without
  polkit) rest on a click that input automation can make, including from an OS-tier agent that
  reaches the display.
- **Dev approvals protect nothing.** On a debug daemon any build in the target folder can approve;
  such approvals are labelled `dev` and ignored by release daemons.
- **`extensions.plugin_dirs` is ungated.** Any token holder can `config.set` it to add a folder of
  plugins that load with hooks and servers and no record; a trusted repository can add one too.
  Gating it is a follow-up outside this plan.
- **Same-size edits with a restored modification time** are caught only at the next daemon start.
- **Hooks and servers are unconfined.** An approved plugin runs as the user.

### Secrets

A server's `${VAR}` that is not `CLAUDE_PLUGIN_ROOT` is a **required secret** of the plugin. The
user sets it with `imperium plugin secret set <plugin> <VAR>` (value from standard input or a
prompt without echo), the desktop's password field, or `plugins.set_secret`; it is stored as
`plugin.<plugin>.<var>` (lowercased; a name over 64 characters is refused) and read only when that
plugin's server starts. A plugin with a missing required secret is listed as such and its server
does not start. Uninstalling offers to delete its secrets. Setting a secret is not an approval.

### Mod trust is separate

UI mods keep their own install, permissions and trust ([06](../06-extensibility.md#installing-and-updating-mods-implemented)).
A marketplace entry that is a mod (`mod.json`) is listed as unsupported.

## Storage layout and configuration

| What | Where | Owner |
| --- | --- | --- |
| Marketplaces | built-in default: the official marketplace (U1), its name reserved for its pinned source; user-added: `[plugins.marketplaces.<name>]` in `<config>/config.toml` (`source`, optional `revision`, `enabled = false` to turn the built-in one off), written with comments kept | defaults, user config |
| Marketplace clone | `<data>/marketplaces/<name>/`, made on first browse or refresh, replaced atomically | daemon cache |
| Marketplace state | `<data>/marketplaces.json`: commit read, refresh time, problems | daemon |
| Installed plugin | `<config>/plugins/<name>/`; folder name = manifest name; `.claude-plugin/marketplace-entry.json` for a marketplace install; `.imperium-source.json` for display only | as today |
| Records and approvals | `<data>/plugin-installs.json`, keyed by canonical folder path: name, origin (`marketplace`, `agent`, `user`, `preexisting`), id, source and commit, content hash, run summary, and an optional approval (hash, run summary, time, kind: `counted`, `dialog-only`, `carried-over` or `dev`) | daemon |
| Migration marker | `<data>/plugin-migration.json`, written once after the migration | daemon |
| On/off for the user | `extensions.disabled_plugins` (it can only turn off, never past the gate) | user config |
| On/off per environment | `plugins: { enabled, disabled, dismissed_suggestions, muted_suggestion_sources }` on the environment record in `<data>/environments.json` | user-owned |
| Suggestions | `[plugins] suggest_marketplaces` and `suggest_plugins` in a trusted project's `.imperium/config.toml` (U2) | repository, offers only |
| Secrets | secret store, `plugin.<plugin>.<var>` | keychain |
| Removed plugins | `<data>/config-trash/`, with a `config.changes` entry for `config.undo` | as today |

A `dialog-only` approval counts as approved; the kind is shown in `plugins.list` and the desktop.

**Effective state for a new session**, in order: passes the [load-time gate](#the-load-time-gate);
not in `extensions.disabled_plugins`, unless the session's environment record lists it in
`enabled`; not in the environment record's `disabled`; not disabled by a trusted repository's
`.imperium/config.toml`; allowed by the agent profile.

**Configuration layers.** `plugins.marketplaces` is read from built-in defaults (the official
marketplace), the user config and environment variables ([10](../10-configuration.md#layers);
stage 2 documents how that layer spells a table keyed by name); an environment variable can add a
marketplace to browse, never an install or approval, and never takes the built-in name with
another source. The environment layer accepts only `[plugins] suggest_marketplaces` and
`suggest_plugins`, and only from a trusted folder.

## Protocol additions

A new `crates/imperium-protocol/src/methods/plugins.rs`, with TypeScript types generated for the
desktop. A paired phone cannot call `plugins.*`. Each stage that adds methods takes **the next
protocol minor at its merge** and updates `PROTOCOL_VERSION`, the version line in
[02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method | Stage | Does |
| --- | --- | --- |
| `plugins.marketplace_add` | 2 | `{source, revision?}` → `MarketplaceInfo`; refuses a name already added or the built-in name with another source |
| `plugins.marketplace_list` | 2 | → `MarketplaceInfo[]` (name, source, `built_in` by source, commit, refreshed time, plugin count, problems) |
| `plugins.marketplace_refresh` | 2 | `{name?}` → per marketplace the commit before and after and plugins added, removed and changed |
| `plugins.marketplace_remove` | 2 | `{name}`; the built-in one is turned off, not freed |
| `plugins.browse` | 2 | `{marketplace?, query?}` → `PluginListing[]` |
| `plugins.inspect` | 3a-ii | `{id, revision?}` → `PluginPreview` (trust prompt contents, `sha256`, run summary, installed comparison) |
| `plugins.install` | 3a-ii | `{id, sha256, environment?}` → `{name, path, withheld, reason, notes}`; never approves |
| `plugins.uninstall` | 3b | `{name, delete_secrets}` |
| `plugins.list` | 3b | `{environment?}` → plugins under `<config>/plugins` with id, origin, source, commit, approval kind (`counted`, `dialog-only`, `carried-over`, `dev`), `withheld` and its reason, effective on/off and what decided it, required and missing secrets |
| `plugins.update` | 4 | `{name, sha256}` → like install, with `replaced`; never approves |
| `plugins.set_enabled` | 4 | `{name, enabled, environment?}`; enabling a withheld plugin is refused with the reason |
| `plugins.approval_challenge` | 8a | `{folder or name, sha256, runs_digest}` → `{challenge, expires}`; answered only to a peer passing checks 1 to 4 |
| `plugins.approve` | 8a | `{folder or name, sha256, runs_digest, challenge}` → `{approved, kind, reason}`; counts only under [who may approve](#who-may-approve) |
| `plugins.set_secret`, `plugins.delete_secret` | 5 | `{name, variable, value?}`; the value never returns |
| `plugins.suggestions`, `plugins.dismiss_suggestion` | 7 | offers for an environment; dismiss one offer or a whole project folder |
| `EnvironmentInfo.plugins` | 4 | the per-environment lists |

`mcp.list`'s `PluginInfo` gains optional `id`, `origin` and `withheld` (stage 3b).

## Stages

Fifteen stages, ordered by dependency; each is one pull request of about 800 changed lines or
fewer, tests included. Every stage: branch from `experimental`, update the docs its behaviour
changes, run `cargo fmt --all --check` locally, and rely on CI for `bash scripts/check-rust.sh`
and `bash scripts/check-web.sh`. A stage that grows past the limit splits at the seam its handoff
notes name. **No new approval counts until [stage 8c](#stage-8c-os-presence-check) turns counting
on**; earlier stages test through an injected verifier or records written directly.

Precondition: [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) is merged before
stage 1 starts (threat-model case 3 relies on it). Met: #240 merged into `experimental` at
`a58b0efa`.

## Stage 1: Marketplace and manifest parsers

- Objective: pure parsing in `imperium-extensions`, no daemon or protocol change.
  (a) `marketplace.rs`: parse `.claude-plugin/marketplace.json` (`name`, `owner`, `metadata`
  including `pluginRoot`, `plugins[]` with `name`, `source`, `description`, `version`, `author`,
  `homepage`, `repository`, `license`, `keywords`, `category`, `tags`, `strict`, and the
  component fields). Sources: a relative path (under `pluginRoot`, refused if it leaves the
  repository), `{source: "github", repo, ref?, sha?}` (→ `https://github.com/<repo>.git`),
  `{source: "url", url, ref?, sha?}`; anything else is an `Unsupported(type)` entry. Unknown
  fields are ignored. Names pass `check_name`. A function writes an entry's **canonical JSON**
  (sorted keys, no insignificant whitespace).
  (b) `plugins.rs`: read the full `plugin.json` (`version`, `author`, `homepage`, `repository`,
  `license`, `keywords`, and `commands`, `agents`, `hooks`, `mcpServers` as a path, a list of
  paths, or for `hooks` and `mcpServers` an inline object), honour those paths inside the plugin
  root only (a path through a link is refused) in addition to the default folders. A folder with
  `.claude-plugin/marketplace-entry.json` and no `plugin.json` is a plugin whose manifest is the
  entry (`strict: false`); with both, `plugin.json` wins and the entry's components add to it.
  (c) `fn runs(&Plugin) -> RunSummary`: each hook (event, matcher, expanded command or URL) and
  each server (command, args, URL, header names, `${VAR}` references), serialisable, comparable
  and digestible, with the launcher flag.
  (d) Replace `installed_by_agent` with a `downloaded` set of folders passed in `Sources`: a
  plugin in that set never expands daemon `${VAR}` and its hooks get `may_rewrite = false`. Until
  stage 3a-i fills the set, the caller passes the folders `installed_by_agent` would have named,
  so this stage changes no behaviour.
- Depends on: #240 merged.
- Owned paths: `crates/imperium-extensions/src/marketplace.rs` (new), `plugins.rs`, `lib.rs`,
  `crates/imperium-core/src/state/extensions.rs` (only to fill `downloaded` as before),
  `crates/imperium-extensions/Cargo.toml` if a dev-dependency is needed.
- Acceptance checks: unit tests (CI, `cargo test -p imperium-extensions`): each source form;
  `pluginRoot`; `..` refused; an unsupported source listed; custom paths and inline `mcpServers`
  and `hooks` loaded; a path outside the root or through a link refused; a folder with only
  `marketplace-entry.json` loaded; canonical JSON stable across key order; `runs` flags
  `npx -y pkg@latest` and two summaries of the same plugin compare equal; a plugin in `downloaded`
  never reads the daemon environment and gets `may_rewrite = false`; existing tests unchanged.
- Risks: unknown fields must stay ignored; component paths "add to the default folder" (record
  the choice in the module doc).
- Status: done (two pull requests: (b), (c), (d) first, then (a); the whole stage came to about
  1,700 changed lines).
- Outcome: `marketplace.rs` parses `marketplace.json` (sources, `pluginRoot`, unknown fields
  ignored, `check_name`, bad entries reported without hiding the rest) and writes an entry as
  canonical JSON. `plugins.rs` reads the full manifest, honours declared component paths inside
  the plugin only (a link is refused), loads an entry-only folder, summarises and digests
  `runs`, and takes the `downloaded` set through `Sources`; the daemon fills it with the folders
  an agent installed, so behaviour is unchanged. Choices: a bare relative source is under
  `pluginRoot` and `./x` is from the repository root; only `https://` Git URLs without
  credentials; declared `commands` and `agents` paths must be folders; declared components add
  to the defaults and the first definition of a name wins; `runs` describes the declared text
  (`${VAR}` kept) so it never carries a daemon environment value; the entry's metadata fills what
  `plugin.json` leaves out.
- Handoff notes: keep parsing free of I/O beyond reading the given folder. Stage 2 clones with
  the safe `git` call and then uses `marketplace::read`, `Entry::canonical_json` and
  `Source::clone_url`; stage 3a-i fills `Sources.downloaded` with marketplace installs (and
  `plugins::agent_installed_folders` stays for agent installs); stage 3a-ii hashes the folder
  including `marketplace-entry.json` and binds approvals to `runs(..).digest()`. Declared
  `commands` and `agents` folders reach the session through `Plugin::extra_commands` and
  `extra_agents`.

## Stage 2: Marketplaces in the daemon

- Objective: add, list, refresh, remove and browse marketplaces through the protocol. `source`
  accepts a git URL, a local folder (absolute, or `~`), or `owner/repo` (two name segments, no
  scheme → GitHub HTTPS; a folder that exists wins, and the result says which was chosen). Clone
  with `extension_install`'s helpers, made `pub(crate)` where private (`git_fetch`, `git`,
  `copy_tree`) without moving them. A local folder is read in place. Write
  `[plugins.marketplaces.<name>]` with comment-keeping edits (follow
  `crates/imperium-config/src/mcp_servers.rs`) and `<data>/marketplaces.json`. `plugins.browse`
  reads components from the clone for relative sources, `null` for external ones. Add `plugins`
  settings with **the official marketplace as a built-in default** (U1): its source pinned in the
  build, cloned lazily; `built_in` only for the pinned source; its name reserved for that source in
  every layer, even while `enabled = false`. The environment layer refuses `[plugins]` (stage 7
  later accepts the suggestion keys).
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/marketplaces.rs` (new), `extension_install.rs`
  (visibility only), `rpc.rs`, `crates/imperium-config/src/settings.rs`, `environment.rs`,
  `crates/imperium-protocol/src/methods/plugins.rs` (new), `methods/mod.rs`, `lib.rs`,
  `snapshots/methods.txt`, `packages/protocol/src/generated/`, docs 02, 06, 10, and this plan.
- Acceptance checks: daemon tests with a fixture marketplace (git repository and folder): add
  (each form; `owner/repo` asserted by URL without fetching), list, refresh after a new commit
  reports the changed entry, remove; a taken name refused; a malformed `marketplace.json` in
  `problems`; marker files absent after add, refresh and browse; the built-in entry listed with no
  network at daemon start; removing it writes `enabled = false`; the built-in name with another
  source refused while on and while off, from the user config and an environment variable; the
  pinned source under another name not `built_in`; `[plugins]` in an environment file refused;
  the snapshot lists the five methods; the next protocol minor at merge. **U1 check:** confirm the
  official source from Anthropic's Claude Code documentation, record the URL and page in this
  plan's Outcome, and pin it in a unit test; if it is not a git repository with
  `.claude-plugin/marketplace.json`, stop and report instead of guessing.
- Risks: clones take minutes; run them on a task, as `mods.inspect` does.
- Status: done, in two pull requests: 2a (#244) holds the config; 2b the protocol and the
  daemon. The protocol could not go in 2a because the router test requires every method in the
  snapshot to have a handler.
- Outcome: 2a: `[plugins.marketplaces.<name>]` settings with the official marketplace as a default
  (`claude-plugins-official`, source `anthropics/claude-plugins-official`); `Settings::validate`
  refuses the name with another source (compared by repository, so another spelling of the
  same repository passes) in every layer, on or off; comment-keeping writers
  (`save_marketplace`, `set_marketplace_enabled`, `remove_marketplace`); `[plugins]` refused in an
  environment file. **U1 check:** the source is confirmed from Claude Code's documentation,
  <https://code.claude.com/docs/en/discover-plugins> (the page's `/plugin marketplace add
  anthropics/claude-plugins-official`), and `git ls-remote` shows the repository with
  `.claude-plugin/marketplace.json` whose `name` is `claude-plugins-official`; a unit test pins it.
  2b: protocol 0.49 (`plugins.marketplace_add`, `_list`, `_refresh`, `_remove` and `plugins.browse`,
  generated types, snapshot, handshake in the desktop client) and `crates/imperium-core/src/marketplaces.rs`.
  Choices: the marketplace's name comes from its `marketplace.json`, not from the caller; a folder
  source is stored as its absolute path and a configured `owner/repo` is always GitHub (the
  folder-wins rule applies when adding only, against the daemon's working folder); refresh clones
  again and compares the commit, keeping the copy in place when it is unchanged; the record
  file keeps a hash of each entry so a refresh can name added, removed and changed plugins for a
  folder too; `built_in` is the official name with its pinned source; `plugins.marketplace_list`
  runs on the connection, add, refresh, remove and browse run on a task and are serialised
  (one network operation at a time); names compare ignoring letter case and a name ending in a
  dot is refused, the clone folder is keyed by the lower-cased name, and clones are staged under
  `<data>/.staging`; browsing a plugin's `components` reads names and two JSON
  files and never through a link. Tests use local git repositories (`file://`) and folders, no
  network, and assert that no hook or server marker file appears.
- Handoff notes: stage 3a-ii inspects a plugin with `marketplace::Entry` and `Source::clone_url`
  from the cached clone (`<data>/marketplaces/<lower-cased name>/`, or the folder for a folder source), using
  `read_again`'s staging-clone pattern; the shared `OPERATIONS` lock in `marketplaces.rs` should
  also guard a plugin fetch that reads a marketplace clone. `plugins.browse` leaves `installed`
  out until stage 3b. The clone's `.git` is kept, so a later stage can read the commit with
  `git rev-parse`; `Record.commit` is the commit of the last read.

## Stage 3a-i: Record store and the load-time gate

- Objective: `<data>/plugin-installs.json`, the migration and its separate marker
  (`<data>/plugin-migration.json`), the digest cache, and the [load-time gate](#the-load-time-gate).
  In `crates/imperium-core/src/state/extensions.rs`, compute the `withheld` and `downloaded`
  folders and pass both in `Sources`, so `imperium_extensions::load_trusting` drops withheld
  folders by canonical path. Define the `ApprovalVerifier` trait; its production implementation
  counts nothing until stage 8c. Tests write records directly. Fix the 06 Hooks line about the
  isolation tier and state the inherited environment.
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/plugin_records.rs` (new), `state/extensions.rs`,
  `crates/imperium-extensions/src/lib.rs` (`Sources.withheld`), docs 06.
- Acceptance checks: daemon tests with records written directly:
  - every folder with hooks or servers and no counted, `dialog-only`, carried-over or (debug only)
    `dev` approval is withheld; removing it from `extensions.disabled_plugins` does not load it;
  - an unrecorded folder with a hook (copied in, or a recorded folder moved to a new name) is
    withheld with "not installed by Imperium: approve to load"; an unrecorded skills-only folder
    loads;
  - rewriting `.imperium-source.json` does not change the gate or the `${VAR}` and rewrite rules;
  - renaming the manifest's `name`, or renaming the folder, withholds it;
  - a symbolic link (on Windows a junction, where the test can make one) withholds it;
  - renaming a file inside it (same count, same newest mtime) withholds it on the next session;
  - a skill edit withholds the whole plugin; a hook added with a forged record hash is still
    withheld because the run summary differs from the approval;
  - migration: folders not in `disabled_plugins` get a carried-over approval and load, folders in
    it are recorded without one; a second start does not re-migrate; a folder added later is
    unrecorded; with the marker present and the store deleted or unreadable, every folder with code
    is withheld and nothing is re-migrated;
  - a `dev` approval is ignored by the release verifier.
- Risks: hashing at daemon start (measure a 1,000-file walk in a test as an estimate); the gate
  must cover every loader path (`grep -rn "load_with\|load_trusting\|fn load(" crates`).
- Status: in review
- Outcome: `crates/imperium-core/src/plugin_records.rs`: the store (`<data>/plugin-installs.json`,
  `Record` and `Approval`, written atomically), the migration and its marker, the content hash
  with its in-memory cache, `ApprovalVerifier` (`ReleaseVerifier` honours counted, dialog-only and
  carried-over approvals and never `dev`; `DebugVerifier` also `dev`; nothing writes a counted or
  dialog-only approval, so no new approval counts) and `gate`, called from
  `DaemonState::extensions`, which passes `Sources.withheld` and a `downloaded` set made of the
  record origins `marketplace` and `agent` plus the provenance-file folders as before. Withheld
  reasons are session notices. `DaemonState::migrate_plugins` runs at daemon start after
  `migrate_memory`. `plugins::discover_except` skips withheld folders by canonical path.
  Decisions after review: the hash leaves out only a top-level `.git` and `.imperium-source.json`
  (a shipped `.imperium-*.json` is content), and the link check runs before any exclusion; rule
  2's folder-name clause applies only to records with origin `marketplace` or `agent` (a
  preexisting clone may sit in a folder named for its repository; a renamed folder has no record,
  and a changed manifest name changes the hash); a folder that is or holds a link is not carried
  over, is reported at daemon start with the reason and the remedy, and stays withheld; the gate
  returns the allowed folders and `discover_gated` loads a folder under `<config>/plugins` only if
  it is listed (`Sources.allowed`), so the gate is not a deny-list.
  Choices: only folders directly under `<config>/plugins` are gated (`extensions.plugin_dirs`
  are the user's or a trusted project's own choice and load as before); a folder that is itself
  a link is withheld; a folder holding a link is not hashed; `.git` and `.imperium-source.json` are
  left out of the hash only at the folder's top; the record keeps the run-summary digest, not
  the summary; a missing or unreadable migration marker counts as present, a store with no
  marker is kept and only the marker is written, and an unreadable store is never replaced;
  migration records an agent-installed folder (provenance file with a session) as `agent` and the
  rest as `preexisting`, and carries over an approval for every recorded folder whose plugin is
  not disabled, skills-only ones included; the existing agent_config test for a plugin with a
  server now expects it to stay withheld after the user turns it on, until the migration
  carries it over.
- Handoff notes: atomic writes; unreadable store → fail closed. Stage 3a-ii hashes with
  `plugin_records::look` (or moves its walk into `mod_install`'s shared hashing), because the
  gate hash leaves out only a top-level `.git` and `.imperium-source.json` and `tree_sha256` leaves
  out `.git` and `.imperium-source.json` at any depth (3a-ii must match the top-level-only rule): the install's recorded hash must be the
  one the gate computes. A record is written with `plugin_records::write_json` and the `Store`
  types. Stage 4's approve must write the approval with the hash and run-summary digest it
  challenged, and a skills-only plugin the user edits needs a way to be recorded again (the gate
  withholds any content change, and a skills-only plugin has no approval to renew). There is a
  window between the gate's hash and the session's file reads that only a same-user writer can
  use (U5).

## Stage 3a-ii: Inspect and install

- Objective: `plugins.inspect` and `plugins.install`. Inspect fetches the plugin (relative:
  copy from the clone; external: `extension_install::fetch` with `ref`/`sha`) into a `Staging`
  folder, writes `.claude-plugin/marketplace-entry.json`, parses and hashes it (reuse
  `mod_install::tree_sha256`). Install **fetches again**, writes the entry file again, refuses a
  different hash, a taken name, a manifest name unlike the entry's, or a link in the source; moves
  it to `<config>/plugins/<name>`; writes the record (`origin = marketplace`, no approval); journals
  a `config.changes` entry. A plugin with code is withheld, with a pointer to the desktop app.
- Depends on: stages 2 and 3a-i.
- Owned paths: `crates/imperium-core/src/plugin_install.rs` (new), `mod_install.rs` (shared
  hashing only), `config_changes.rs`, `rpc.rs`, protocol `methods/plugins.rs`, version, snapshot,
  generated types, docs 02 and 06.
- Acceptance checks: inspect then install the hook plugin → installed, recorded, withheld; a source
  changed between inspect and install refused; a taken name refused; marker files absent; a
  `strict: false` plugin installs, loads, and stays unmodified after a marketplace refresh; a
  skills-only plugin installs and loads; a stray `approve` field is ignored and noted.
- Risks: none beyond stage 3a-i's.
- Status: done in two pull requests: 3a-ii-1 (`plugins.inspect`, protocol 0.50) and 3a-ii-2
  (`plugins.install`, protocol 0.51, stacked on it). The split is the size limit's: both together
  came to about 1,270 changed lines.
- Outcome: `plugins.inspect` and `plugins.install` (`crates/imperium-core/src/plugin_install.rs`,
  `rpc/config.rs`, generated types, snapshot, docs 02 and 06). Both stage the plugin under
  `<data>/.staging` (`Staging::new(<data>)`), write the entry file, parse with
  `plugins::discover`, and hash with `plugin_records::measure` (the gate's walk, uncached), so an
  installed folder hashes as inspected. Install refuses a different hash, a taken name (also by
  letter case: any existing folder whose name is equal ignoring ASCII case), a manifest name
  unlike the entry's, and a link in the source; it moves the folder, writes `.imperium-source.json`
  (so `config.undo` can remove it) and the record through `plugin_records::add_record`, and
  journals an `install` change of kind `plugin`. The result's `withheld` and `reason` come from the
  gate itself. Choices: hashing reuses the gate's walk, not `mod_install::tree_sha256` (which skips
  `.git` and `.imperium-source.json` at any depth and is left as it is); the copy refuses links
  outright instead of dropping them (`extension_install::fetch`'s copy drops them silently) and
  leaves out only a top-level `.git` and `.imperium-source.json`; `plugins.install` also takes the
  `revision` the inspection was given (the plan lists `{id, sha256, environment?}`), because a
  hash from an inspection at another revision could never match; `environment` is accepted and
  ignored with a note until plugins are turned on per environment (stage 4); a stray `approve` is
  an explicit, unexported field that is ignored and noted; `add_record` refuses to run before the
  first-start migration (it would take its place) and never overwrites an unreadable store; a
  plugin inside the marketplace's repository is copied while holding the marketplace operation
  lock (a refresh cannot replace the clone under the copy), an external one is cloned outside it;
  a pinned `sha` that the clone's commit does not start with is refused, also when a `revision`
  is given; plugin names (the entry's, and `extension_install::check_name`) may not end with a dot,
  which Windows drops; the install folder is created exclusively (`create_dir`) and the content
  moved into it, so nothing is ever merged into or written over an existing folder (the install
  fails if the folder exists; on Windows `move_tree`'s copy fallback still always runs, but only
  into the empty folder this install just claimed); a relative source with a
  `revision` is refused; the preview shows hook and server commands with the install folder
  substituted for the staging folder.
- Handoff notes: stage 3b's `plugins.list` reads the same records; `config.undo` of a marketplace
  install removes the folder but leaves its record (a stale record for a path that no longer
  exists is harmless, and a new install replaces it), so 3b's uninstall should delete the record.
  Stage 4's update can reuse `stage` (fetch, entry file, parse, hash) and the `Staged` value; an
  update reads its source from the record, not from the folder. External sources (`github`,
  `url`) are tested at `fetch_git` only: a `Source::Url` must be `https`, so no test can point an
  entry at a local repository.

## Stage 3b: Uninstall, list and undo

- Objective: `plugins.uninstall` (to `config-trash`, record removed, secrets optional once stage 5
  lands), `plugins.list` (every folder under `<config>/plugins`, recorded or not, with the approval
  kind), `config.undo` of install and uninstall (with the record), `PluginInfo`'s `id`, `origin`
  and `withheld`, and agent installs (`install_extension`, `kind = "plugin"`) recorded with
  `origin = agent` and no approval.
- Depends on: stage 3a-ii.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `extension_install.rs`,
  `connectors.rs`, `config_changes.rs`, `rpc.rs`, protocol `methods/plugins.rs`,
  `methods/connectors.rs`, version, snapshot, generated types, docs 02 and 06.
- Acceptance checks: uninstall moves the folder and removes the record; undo restores both, or
  removes both for an install; `plugins.list` reports withheld plugins with reasons (unrecorded
  ones too) and shows `dev` approvals as `dev`; an agent install of a hook plugin is recorded and
  withheld; `mcp.list` carries `withheld`.
- Risks: undo must not restore a record over a folder replaced since.
- Status: in review
- Outcome: `plugins.uninstall {name}` and `plugins.list` (`plugin_install.rs`, `rpc/config.rs`,
  protocol 0.52, generated types, snapshot, docs 02 and 06). Uninstall resolves a folder name, a
  recorded name or a recorded `name@marketplace`, all ignoring letter case, and runs
  `config_changes::uninstall_folder`, which `uninstall_extension` shares: the folder goes to
  `<data>/config-trash`, the record is removed, and the journal entry keeps the folder's content
  hash (`ConfigChange.sha256`) and the record as JSON (`ConfigChange.record`). `config.undo` of a
  plugin install removes the folder and the record, and refuses (leaving both) when
  `plugin_records::look` gives another hash than the journalled one; undo of an uninstall refuses
  when the name is taken or the kept copy no longer hashes as journalled, and restores the record
  with the folder (moving the folder back if the record cannot be written). Installs journalled
  before this stage carry no hash and skip the check. `plugins.list` returns every plugin folder
  with `id`, `origin`, source, commit, approval kind (`dev` shown as `dev`), `enabled`, the gate's
  `withheld` and `reason`, and hook and server counts; unrecorded folders are listed; folders with
  no plugin manifest are `problems`. `mcp.list`'s `PluginInfo` gained `id`, `origin`, `withheld`
  and `withheld_reason`. `install_extension` of a plugin runs the first-start migration if the
  daemon start did not, refuses the install if the record file is not writable, and records
  `origin = agent`, no approval, with the gate's hash and run digest; a record failure removes the
  folder and turns the plugin back on in `disabled_plugins` if this install had turned it off.
  Choices: case-insensitive lookup also applies to `find_installed` for plugins; the dot rule was
  already in `check_name` and the entry parser; the `agent_config` test that expected a plugin
  with a server to load after the migration now expects it to stay withheld (the install made the
  marker, and an agent's plugin has no approval); `plugins.uninstall` takes no `secrets` option
  until stage 5; the uninstall hash is `None` for a folder holding a link, which then restores
  without the hash check (the gate withholds it either way).
- Handoff notes: stage 4's update must change the record's hash and the journalled hash only
  through `add_record`; an update after an install makes `config.undo` of that install refuse
  (the folder no longer hashes as installed), which is the intended outcome. Stage 4's approve
  writes `Record.approval`; undo of an uninstall restores whatever approval the record held, and
  the gate still checks it against the restored hash and run digest. `plugin_records::take_record`
  and `add_record_at` are the keyed store edits (key = `key_of`, the canonical path, which needs
  the folder to exist, so read it before moving). Stage 6's CLI can call `plugins.list` and
  `plugins.uninstall` as they are.

## Stage 4: Enable, disable, update and approve

- Objective: `plugins.set_enabled` (user: `extensions.disabled_plugins` through `config.set`'s
  path; environment: the record's `plugins` lists), `plugins.update`, the effective-state order,
  and the approval **write path** (record an approval of a given kind for a folder, hash and run
  summary; create an `origin = user` record for an unrecorded folder) behind `ApprovalVerifier`.
  Enabling a withheld plugin is refused with its reason. Update never approves, keeps the previous
  folder in `config-trash`, and reports "up to date" for an equal hash. `plugins.approve` itself
  lands in stage 8a.
- Depends on: stage 3b.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `plugin_records.rs`,
  `environments.rs`, `state/extensions.rs`, `rpc.rs`, protocol `methods/plugins.rs`, the file
  holding `EnvironmentInfo` (`grep -rn "pub struct EnvironmentInfo" crates/imperium-protocol`),
  version, snapshot, generated types, docs 02, 06, 14.
- Acceptance checks: disable for the user → not loaded in a new session; enable only for
  environment A → loaded in A, not B; a trusted repository file disabling it in A wins; a
  skill-only change to an approved hook plugin's update is withheld; an unchanged update is "up to
  date"; an update naming another marketplace refused; enabling a withheld plugin refused; with a
  test verifier, a written approval lets the plugin load (an unrecorded folder gets an
  `origin = user` record); with the production verifier nothing is written; off then on needs no
  new approval.
- Risks: two places hold on/off; `plugins.list` says which decided.
- Status: in review
- Outcome: `plugins.set_enabled`, `plugins.update`, `plugins.list`'s `environment` param and
  `decided_by`, `EnvironmentInfo.plugins` (`EnvironmentPlugins`), the approval write path, protocol
  0.53, docs 02, 06 and 14. The switch is `state::plugin_switch` (repository, then the
  environment's `disabled`, then the user's list unless the environment's `enabled` lists it, else
  on), applied by `DaemonState::extensions`, which now takes the environment id and the plugins a
  trusted repository disables. To know the latter even when the user's list holds the same name,
  `imperium-config`'s `EnvironmentSettings` gained `repo_disabled_plugins` (outside the stage's
  owned paths; six lines). `plugin_records::record_approval` writes an approval only if
  `ApprovalVerifier::may_write` allows the kind (new method; release: never, debug: `dev` only;
  tests use `TestVerifier`); it requires the hash and run digest to equal the folder's now, creates
  an `origin = user` record for an unrecorded folder, and takes the approved hash as the record's
  (so a skills-only plugin the user edited can be recorded again). `plugins.update` and its swap
  are in `plugin_install.rs`: old record out, old folder to `config-trash`, exclusive claim, fill,
  new record with no approval; every failure restores the old folder and record. Choices: update is
  not journalled (`config.undo` has no kind for it; the result carries `kept_at`); the hash is
  compared with the installed folder's own hash as well as the record's, so a folder edited by hand
  is replaced; a source that differs from the record's is refused (also when only the URL moved);
  a plugin turned on for an environment that the user did not turn off is a no-op in effect;
  `plugins.list` without `environment` shows the user's view; an environment's lists are matched by
  exact plugin name.
- Handoff notes: `extensions.disabled_plugins` stays the only user-level switch. Stage 8a's
  `plugins.approve` calls `record_approval` (remove its `allow(dead_code)`); the challenge
  must bind the hash and run digest it passes. Stage 5's secrets should hook the uninstall and
  update paths. Stage 7 fills `dismissed_suggestions` and `muted_suggestion_sources`. An
  environment's lists keep names of uninstalled plugins; they do nothing.

## Stage 5: Plugin secrets and child environments

- Objective: (a) required secrets from `runs`, `plugins.set_secret` and `plugins.delete_secret`,
  `${VAR}` of a downloaded plugin read from `plugin.<plugin>.<var>` in the secret store, missing
  secrets on `plugins.list`, servers with a missing secret not started (with a notice), and
  `delete_secrets` on uninstall. (b) **Child environment allowlist** for hooks and stdio servers
  of every plugin under `<config>/plugins`: a cleared environment plus `PATH`, `HOME` or
  `USERPROFILE`, `SYSTEMROOT`, `TEMP`, `TMP`, `LANG`, `LC_*`, `TERM`, and on Windows `APPDATA`,
  `LOCALAPPDATA`, `PATHEXT`, `COMSPEC`; the spawned-by-daemon mark; `CLAUDE_PLUGIN_ROOT`,
  `CLAUDE_PROJECT_DIR`, `IMPERIUM_PROJECT_DIR`; and the variables its server definition sets.
  User-configured hooks and servers keep today's environment.
- Depends on: stage 3b.
- Owned paths: `crates/imperium-extensions/src/plugins.rs`, `hooks.rs`, the stdio spawn in
  `imperium-mcp` (`grep -rn "Command::new" crates/imperium-mcp/src`),
  `crates/imperium-core/src/plugin_install.rs`, `state/extensions.rs`, `rpc.rs`, protocol
  `methods/plugins.rs`, version, snapshot, generated types, docs 02, 06, 10.
- Acceptance checks: with the in-memory secret store, a set secret reaches the server's
  environment and nowhere else (config file, plugin folder, store and journal checked); a missing
  one leaves the server unstarted with a notice and the skills available; a 65-character name
  refused; the value never in a result; a plugin hook and server do not see `SOME_DAEMON_SECRET`
  from the daemon's environment and do see `PATH` and the mark; a user hook still sees it.
- Risks: the allowlist may break plugins needing other variables (the server definition can set
  them); `FOO` and `foo` collide as secret names, refuse the second.
- Status: done, in two pull requests: (a) secrets (protocol 0.54) and (b) the child
  environment allowlist, stacked on it (no protocol change)
- Outcome (a): `plugins.set_secret`, `plugins.delete_secret`, `delete_secrets` on
  `plugins.uninstall`, and `required_secrets` / `missing_secrets` on `plugins.list`
  (`plugin_install.rs`, protocol 0.54, docs 02, 06 and 10). `imperium_extensions::plugins` gained
  `PluginSecrets` (a lookup of `(plugin, variable)`), `discover_with` and `Plugin.required_secrets`;
  `load_with_secrets` passes the lookup, which `DaemonState::extensions` backs with the secret
  store. A downloaded plugin's `${VAR}` reads `plugin.<plugin>.<var>` (lowercased); a server with a
  required variable (no `:-default`) and no secret is dropped with a notice that names the variable.
  Choices: the value is read when a session loads its extensions (the `ServerSpec` holds the
  expanded strings), not when the server starts; only a plugin whose record origin is
  `marketplace` or `agent` accepts a secret; of two variables that differ only by letter case the
  first in sorted order may be set and the other is refused; the check that the variable is one a
  server names stops a secret for an arbitrary name; the value is capped at 64 KiB;
  `PluginsSetSecretParams` has a `Debug` that hides the value; `delete_secrets` deletes the secrets
  of the variables the plugin's servers name (the keychain cannot be listed), and an undo of the
  uninstall does not bring them back.
- Review fixes (a): a secret variable is `[A-Za-z_][A-Za-z0-9_]*` (in `secret_name`, in
  `required_references`, and when a downloaded `${VAR}` resolves), so a `.` cannot name another
  plugin's secret; secrets expand only into a stdio server's `env` and an HTTP server's `headers`
  (a server naming a variable in its command, arguments or URL is dropped with a notice, and a
  default there is used as written); `Debug` of `Launch` (so `ServerSpec` and `Plugin`) shows
  names of `env` and headers but not values; origin `user` counts as downloaded, like
  `marketplace` and `agent`, and is eligible for `set_secret`, so its `${VAR}` never reads the
  daemon's environment, while `preexisting` plugins keep it (nothing that runs today stops).
- Outcome (b): hooks and stdio servers of plugins under `<config>/plugins` run with a cleared
  environment plus `imperium_mcp::servers::plugin_child_env` (the allowlist and the spawn mark),
  `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PROJECT_DIR`, `IMPERIUM_PROJECT_DIR` and the definition's own
  variables. `load_with_secrets` marks them: `Hook.plugin_env_root` for a hook, and a reserved
  `IMPERIUM_PLUGIN_CLEAN_ENV` entry in a stdio `ServerSpec`'s `env` (so no `Launch` or `ServerSpec`
  signature changed) that `servers::start` turns into `env_clear` and never passes on. Choices: the
  test is the plugin's canonical folder lying under `<config>/plugins`, so `plugin_dirs`
  plugins keep the daemon's environment; the definition's variables come last and win; HTTP hooks
  and servers have no child process.
- Handoff notes: stage 6's CLI can call `plugins.set_secret` with the value from standard input.
  A plugin that needs a variable from the daemon's environment now has to declare it itself; a
  user who wants it can set it in the server definition's `env` of their own plugin_dirs copy.

## Stage 6: CLI and the safety floor

- Objective: `imperium plugin marketplace add|list|refresh|remove`, `browse [query]
  [--marketplace m]`, `inspect <name@mp>`, `install <name@mp> [--env <env>] [--sha256 h]
  [--yes]`, `update <name> [--sha256 h] [--yes]`, `enable|disable <name> [--env <env>]`,
  `uninstall <name> [--delete-secrets]`, `list [--env]`, `secret set|delete <name> <VAR>`. **No
  approving command** (U4). Install and update print the trust prompt and ask `Install <id>?
  [y/N]` (`--yes` without a terminal), then say a plugin with code is withheld until approved in
  the desktop app. The floor: add `imperium plugin marketplace add`, `imperium plugin
  install|update|enable`, and `imperium config set|unset` of `extensions.disabled_plugins` and
  `extensions.plugin_dirs` to `crates/imperium-policy/src/floor.rs`; add `plugin-installs.json`,
  `plugin-migration.json` and `marketplaces.json` to `FLOOR_PATHS`; `protect_dir` on
  `<config>/plugins` and `<data>/marketplaces` (`grep -rn "protect_dir(" crates`).
- Depends on: stages 4 and 5.
- Owned paths: `crates/imperium-cli/src/plugins.rs` (new), `main.rs`, CLI snapshots and tests,
  `crates/imperium-policy/src/floor.rs`, `permissions.rs`, the daemon's `protect_dir` call, docs
  05 and 06.
- Acceptance checks: help snapshot has no `approve`; against a test daemon, install with `--yes`
  and no terminal → installed, withheld, with the desktop note; `enable` of it refused; `list`
  shows it; floor tests: `imperium plugin marketplace add x`, `imperium plugin install x@y --yes`,
  `imperium plugin enable x`, `imperium config set extensions.disabled_plugins []`,
  `imperium config set extensions.plugin_dirs '["/tmp/x"]'` all ask; file-tool writes to the store,
  the marker and `<config>/plugins/x/` ask; reads do not.
- Risks: the floor is a guard against mistakes, not a control (U5).
- Status: done
- Outcome: 6a: `imperium plugin marketplace add`, `plugin install|update|enable` and `config
  set|unset` of `extensions.disabled_plugins` or `extensions.plugin_dirs` ask
  (`imperium-policy/src/floor.rs`, with tests for every line the plan lists and for the read-only
  commands that must not ask); `plugin-installs.json`, `plugin-migration.json` and
  `marketplaces.json` are in `FLOOR_PATHS`; the daemon's `protect_extension_dirs` also protects
  `<data>/marketplaces` (`<config>/plugins` already was); docs 05. 6b: `imperium plugin ...`
  (`crates/imperium-cli/src/plugins.rs`, the help snapshot, docs 06) with no approving command.
  The trust prompt is rendered from `plugins.inspect`; `install` and `update` ask `Install
  <id>? [y/N]` / `Update <id>? [y/N]`, or need `--yes` without a terminal, and print that a
  plugin with code is withheld until approved in the desktop app. `secret set` reads the value
  from a no-echo prompt or from standard input and never takes or prints it. Tests run the CLI's
  dispatch against a daemon on loopback: a scripted install of the hook plugin lands withheld,
  `enable` is refused, `list` shows it; no terminal and no `--yes` installs nothing; a secret is
  saved and never printed; the command tree holds nothing named approve.
  Choices: `update` finds the plugin's `name@marketplace` in `plugins.list` and inspects that id
  (there is no inspect for an update), so the prompt shows the new version's content; when the
  folder already holds the inspected content nothing is asked and `plugins.update` is called, so
  the daemon alone decides "up to date" (it also checks the record's hash); `install` refuses a name that is
  already installed before asking; the floor's `config` rule looks for the verb `set` or `unset`
  and either key (case-insensitive prefix, so `key=value` also asks) anywhere after the
  `config` word, and its `plugin` rule for the verbs anywhere after `plugin`, so a global
  `--remote x` between the words is covered and a false alarm only asks; `plugin disable`,
  `uninstall` and `secret set|delete` do not ask (they cannot bring code in or start it);
  `--env` is passed to `plugins.install` (noted by the daemon) and used by `enable|disable|list`.
- Handoff notes: stage 8b/10 own approving; nothing in the CLI may gain an approving command
  (the unit test over the command tree fails if one named `approve` appears). Stage 7's
  `imperium plugin suggestions` (listed in the plan's CLI section) is not here: it lands with the
  suggestions it shows. Stage 9/10's desktop windows call the same methods.

## Stage 7: Repository suggestions

- Objective: U2. From a folder in `mcp.trusted_projects` only, the environment layer accepts
  `[plugins] suggest_marketplaces = [{ source, revision? }]` and `suggest_plugins =
  ["name@marketplace", ...]` (at most 20 each; one bad entry drops only itself). They become
  offers on `plugins.suggestions {environment}`, with the file they came from and whether each is
  already added or installed. `plugins.dismiss_suggestion` dismisses one offer, or with
  `source_folder` mutes every suggestion from that project ("no suggestions from this project").
  A suggestion adds, fetches, installs and enables nothing. `imperium plugin suggestions --env
  <env>` prints the offers, and prints the commands that would act on them **only when standard
  output is a terminal**.
- Depends on: stages 4 and 6.
- Owned paths: `crates/imperium-config/src/environment.rs`,
  `crates/imperium-core/src/plugin_suggestions.rs` (new), `environments.rs`, `rpc.rs`, protocol
  `methods/plugins.rs`, version, snapshot, generated types, `crates/imperium-cli/src/plugins.rs`,
  CLI snapshots, docs 02, 06, 14.
- Acceptance checks: an untrusted folder's suggestions refused and shown in `imperium env config`;
  a trusted folder's offered; offering changes no marketplace, record, config or
  `disabled_plugins`; a dismissed offer stays dismissed; a muted project offers nothing, even after
  its file changes; with standard output redirected the CLI prints no commands; marker files
  absent.
- Risks: suggestion text is repository data; it goes only to the user's surfaces, never into a
  prompt.
- Status: done
- Outcome: protocol 0.55: `plugins.suggestions` and `plugins.dismiss_suggestion`
  (`crates/imperium-core/src/plugin_suggestions.rs`, `environments.rs`, `rpc/config.rs`,
  `imperium-config/src/environment.rs`, generated types, snapshot, docs 02, 06 and 14).
  `EnvironmentConfig` takes `suggest_marketplaces` and `suggest_plugins` out of each file's
  `[plugins]` section while reading it (so each offer knows its file), validates each entry on its
  own (short plain text; `name@marketplace` of plain characters; at most 20 of each kind in all),
  and `apply` keeps them in `EnvironmentSettings.suggestions` for a trusted folder only; an
  untrusted folder gets one fixed line in `refused` with none of the repository's text (that
  list can become a session notice). Any other `[plugins]` key is still refused. The listing
  is read on every call and filtered by the environment record's `dismissed_suggestions` (keys
  `marketplace:<source>[@<revision>]` and `plugin:<id lowercased>`) and `muted_suggestion_sources`
  (canonical folder paths). Tests: a trusted folder's offers are listed and the config and data
  folders, the settings, the plugin list and the environment's lists are unchanged; an untrusted
  folder's offers are refused and shown in `environment_config`; a dismissed offer stays dismissed;
  a muted project offers nothing even after its file changes to a new offer; the CLI prints the
  commands only when told its output is a terminal, after `--` (`imperium plugin marketplace add
  --revision=<rev> -- <source>`, `imperium plugin install -- <id>`, so a word that starts with `-`
  is never an option; the parser is tested), quotes the repository's words as one shell word and
  escapes control, text-reordering and zero-width characters. Choices: `source_folder` must be the environment's own
  first folder (so the muted list cannot be grown with arbitrary paths) and at most 500
  dismissals and 500 muted folders are kept per environment; "already added" for a marketplace
  offer compares sources (case, `.git` and `https://github.com/` ignored) and for a plugin offer
  the marketplace's name; there is no un-mute yet (the lists are in `environments.json`; stage 9
  can add one); the CLI has no dismiss command (the desktop's stage 9 owns it); an environment on
  a remote runtime, or without a folder, has no offers.
- Handoff notes: stage 9's window calls `plugins.suggestions` and `plugins.dismiss_suggestion`
  with the keys and `source_folder` the listing gives; it must show offer text as plain text and
  never pass it to a model. Stage 10's install from an offer goes through `plugins.inspect` and the
  trust prompt like any install; an offer never skips it. Stage 11's agents must not read
  `plugins.suggestions`.

## Stage 8a: Daemon identification of the desktop app

- Objective: the production `ApprovalVerifier` checks 1 to 5 and 7 of
  [who may approve](#who-may-approve), and the methods `plugins.approval_challenge` and
  `plugins.approve`. Race-free peer identity: Windows `OpenProcess` at accept with the handle held
  and the creation time checked against the accept time, re-checked at approve; Linux
  `SO_PEERPIDFD`, or `pidfd_open` at accept with the start time read through it and refused when
  later than the accept; macOS the audit token. Peer path equal to the app
  installed beside `imperium-core`; Linux namespaces equal to the daemon's; macOS `sandbox_check`;
  the environment refusals. At approve, the daemon **recomputes the folder's hash and run summary
  and requires them to equal the challenged values**. U7: on Windows, count only in a release build
  whose compile-time signed-build constant the signing job set, running from under
  `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`. The debug-only development
  verifier (`dev`). **Counting stays off** (the verifier refuses with "presence check not built")
  until stage 8c.
- Depends on: stage 4; #240 merged.
- Owned paths: `crates/imperium-core/src/app_identity/` (new: `mod.rs`, `windows.rs`, `macos.rs`,
  `linux.rs`), the socket accept path that records peer identity (find with
  `grep -rn "GetNamedPipeClientProcessId\|SO_PEERCRED\|peer_cred" crates/imperium-core/src`),
  `plugin_records.rs`, `rpc.rs`, `crates/imperium-core/Cargo.toml`, protocol `methods/plugins.rs`,
  version, snapshot, generated types, `.github/workflows/build.yml` (the signed-build constant, set
  only in the signing job's build step),
  docs 02, 06, 11.
- Acceptance checks: per-platform unit tests in CI: a TCP connection refused before any process
  check; the test binary refused (wrong path); Windows: a handle whose process was created after
  the accept is refused, and a peer that exits and whose pid is reused before approve is refused;
  Linux: identity read through the pidfd survives pid reuse (the test forks, exits and reuses
  where it can, else asserts the pidfd path is used); Linux `pidfd_open` fallback: a process whose
  start time is later than the accept time is refused (the test injects the accept time); Linux: a
  peer in another mount or pid
  namespace (a `unshare` child) refused; peers with `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` or
  `LD_PRELOAD` set refused; a challenge used twice, expired, from another connection, or for
  another hash or run summary refused; a folder changed between challenge and approve refused; a
  Windows build without the compile-time signed constant counts nothing, and setting an
  environment variable or writing a file of that name at run time changes nothing; a daemon whose
  executable is outside `FOLDERID_ProgramFiles` counts nothing even with `ProgramFiles` pointed at
  its folder in its environment; the dev verifier exists only in debug
  builds; with counting off nothing is recorded.
- Risks: platform APIs new to the codebase, each needing its CI runner. Split if over 800 lines
  (8a-i shared and Linux, 8a-ii Windows and macOS).
- Status: done
- Outcome: **8a-i (shared and Linux), protocol 0.56.** `crates/imperium-core/src/app_identity/`:
  `mod.rs` holds check 1 (`local_only`, run before the plugin is looked up or any process is
  read), the `Identify` trait with the production `InstalledApp` (checks 1 to 4 against
  `imperium-desktop` beside the running executable), `refused_variable` (check 4's names, compared
  ignoring case (on Windows only since 8a-ii)), `Challenges` (check 5) and the `challenge` and `approve` flows; `linux.rs` the
  pidfd identity; `unsupported.rs` the Windows and macOS stub, which refuses with "not built for
  this platform yet". `server.rs` takes the identity at accept beside the existing pid and accept
  time (`PeerPid`), and `Connection` holds it with its own `Challenges`. `ApprovalVerifier` gained
  `approval_kind` (check 6's answer): release refuses with "presence check not built", the
  development `DebugVerifier`, now compiled only under `debug_assertions`, gives `dev`. Methods
  `plugins.approval_challenge` and `plugins.approve` (`record_approval` lost its `dead_code`
  allowance). Dependencies: `rustix` (already in the lock) for pidfds, `SO_PEERCRED`, `poll` and
  `/proc`, and `nix` 0.31 with only `socket` for `SO_PEERPIDFD`, which `rustix` lacks, so the
  module has no `unsafe` (the workspace forbids it). Choices: a challenge lives in the
  connection that asked (so another connection cannot know it), at most eight open per
  connection; a challenge is used up by any answer, refused or not; the challenge is refused
  unless the folder has the named hash and digest now; `name` is resolved as `plugins.update`
  does (folder name, plugin name or `name@marketplace`); approve re-runs checks 1 to 4 before
  the challenge; failures of `plugins.approve` are a result (`approved: false`, `reason`), a
  name that resolves to nothing is an error; the start-time check applies to the `pidfd_open`
  path only (a socket's own pidfd is the connecting process), and it refuses only a process
  started after the accept, so before 6.5 a pid reused between the peer's connect and the
  accept is not detected (the planned check is against the accept, not the connect); the
  fallback runs only when the kernel answers that it lacks `SO_PEERPIDFD` (`ENOPROTOOPT`,
  whatever the version says, since distributions backport it), and any other `SO_PEERPIDFD`
  error refuses the peer, since it means the peer has exited and its id may be another
  process's (review findings).
  Tests: TCP refused first (unit and through `Connection`); the test binary refused as the
  desktop app; a socket peer held by `SO_PEERPIDFD` on kernels from 6.5; an exited peer refused
  through its pidfd while `/proc/<pid>` still exists (reuse itself is not forced: `pid_max` is
  large and unprivileged tests cannot pick a pid); the `pidfd_open` fallback with an injected
  accept time; `LD_PRELOAD` and `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` in a real peer's
  environment; a peer in another mount and pid namespace made with `unshare` (unprivileged, else
  passwordless `sudo`; skipped with a `skipped:` line where neither works); challenge reuse,
  expiry, another connection, another hash, digest or folder; a folder changed between
  challenge and approve; nothing recorded with counting off; the dev verifier only in debug
  builds.
- Outcome (8a-ii, Windows): `imperium-ipc` gained `ClientProcess` in its `unsafe`-allowed
  Windows module (`windows/process.rs`): `OpenProcess` on `GetNamedPipeClientProcessId` at
  accept, an `OwnedHandle` closed on drop, `creation_time`, `image_path`
  (`QueryFullProcessImageNameW`) and `has_exited` (`WaitForSingleObject`), each `unsafe` block
  with a SAFETY comment; `imperium-core` still forbids `unsafe`. `app_identity/windows.rs` holds
  the handle and the accept time in `Peer` and checks, at the challenge and at approve: not
  exited, created no later than the accept, image path (canonical) equal to the app beside
  `imperium-core`, not exited again. After the accept a reused process id never stands in for
  it; a pid reused between the client opening the pipe and the accept is not detected (review
  finding). `has_exited` treats only `WAIT_TIMEOUT` as alive. Check 7 is `may_count`, called after the verifier gives a
  kind: a `dev` approval is exempt, anything else needs `option_env!("IMPERIUM_SIGNED_BUILD")`, a
  release build, and `current_exe` under `dirs_sys::known_folder(FOLDERID_ProgramFiles)`.
  Decisions: the peer's environment is not read on Windows (user decision; the app checks its
  own in stage 8b); **no workflow sets `IMPERIUM_SIGNED_BUILD`**, because no step Authenticode-signs
  Windows builds yet, and setting it in the shared "Build desktop bundles" step would mark
  unsigned per-machine MSI builds as signed; roadmap item 59's signing step sets it. Tests: the
  handle across exit; a process created after the accept; the image path and the test binary
  refused; `release_gate` cases; `may_count` refusing everything but `dev` in a test build; a child
  test run with `IMPERIUM_SIGNED_BUILD` set, a file of that name beside the binary and
  `ProgramFiles`/`ProgramW6432` pointed at its folder, which still counts nothing; the shared
  counting test refuses on Windows by check 7.
- Outcome (8a-ii, macOS): `app_identity/macos.rs` takes the audit token at accept (`nix`'s
  `LocalPeerToken`) and derives the rest from it: the code object by token (`security-framework`'s
  `SecCode::copy_guest_with_attribues`), whose path (the executable, or the bundle whose
  `Contents/MacOS/imperium-desktop` is the app) must be the app beside `imperium-core`;
  `imperium_ipc::is_sandboxed` (`sandbox_check_by_audit_token` in `imperium-ipc`'s
  `macos_sandbox.rs`, the one Unix-side module allowing `unsafe`) must answer "not sandboxed"; a
  `SecRequirement` of `entitlement["com.apple.security.app-sandbox"] exists` must not hold; and
  the executable and environment read by pid (`sysinfo`) are bracketed by the token, which is
  looked up again afterwards. Choices: the binding is declared variadic, as in `<sandbox.h>`,
  and called with no variadic argument (`SANDBOX_FILTER_NONE`), because declaring a variadic C
  function with a fixed arity is not ABI-safe on Apple arm64; the environment is checked before
  the executable; `dyld` strips `DYLD_*` from restricted processes, so a hardened release app
  shows none and library validation holds instead (stage 8b's entitlement check keeps it on).
  An environment that comes back empty (unreadable) is refused (review finding). `refused_variable`
  now ignores case only on Windows: `sysinfo` lists the kernel's lowercase `apple[]` strings
  (`dyld_file=` and the like) with a platform binary's environment, which matched `DYLD_*` when
  compared ignoring case. Tests: this process's token and path, the test binary refused;
  `/usr/bin/nc` connected to our socket passes the whole `identify` (sandbox answer "not
  sandboxed") and is refused at another path; a `python3` child with
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` or `DYLD_FALLBACK_LIBRARY_PATH` in its environment (the runner's `python3` is not restricted, so
  `dyld` leaves `DYLD_*` in place); a child under `sandbox-exec`; an exited child refused by its
  token. Not tested: a bundled app's path (CI runs no `.app`).
- Handoff notes: no change to `peer::may_grant` or to mod approvals (kept). **For 8a-ii:**
  replace `unsupported.rs` with `windows.rs` and `macos.rs` behind the same `Peer`,
  `at_accept` and `identify` (`identify(peer, app)` gets the canonical app path); Windows takes
  `GetNamedPipeClientProcessId` then `OpenProcess` at accept in `at_accept`, holds the handle in
  `Peer`, refuses a creation time later than the accept and re-checks the handle in `identify`,
  and adds check 7 (the signing job's `option_env!` constant and
  `SHGetKnownFolderPath(FOLDERID_ProgramFiles)`) with its `build.yml` change; macOS uses the
  audit token for the path and `sandbox_check`. `imperium-core` forbids `unsafe` through the
  workspace lints, so Win32 and macOS calls need either safe wrappers or the `imperium-ipc`
  pattern (crate-level `deny` with one module allowed), which is a decision for that stage.
  `refused_variable` already covers `DYLD_*` and Windows' case-insensitive names. Stage 8b's
  app sends `plugins.approve` on its own local-socket connection, the same one that asked for
  the challenge.

## Stage 8b: Desktop approval command and native dialog

- Objective: a Tauri command `plugin_approve { folder or name, action: install | update | approve,
  environment? }`: it reads the preview, requests a challenge, shows a native dialog with
  `tauri-plugin-dialog` (plugin id, hook and server command lines, flagged launchers, hash), and
  on *Approve* sends `plugins.approve` with the challenge over its own `imperium-client`
  connection on the **local socket only**. The command is allowed only from the app's own windows.
  The app calls `SetDefaultDllDirectories` at start on Windows. CI checks the macOS release
  entitlements leave out `allow-dyld-environment-variables`, `disable-library-validation` and
  WebKit inspection.
- Depends on: stage 8a.
- Owned paths: `apps/desktop/src-tauri/src/plugin_approval.rs` (new), `main.rs`,
  `apps/desktop/src-tauri/capabilities/`, `apps/desktop/src-tauri/tauri.conf.json` (entitlements),
  `.github/workflows/build.yml` (the entitlement check), doc 07.
- Acceptance checks: Rust unit tests in CI for the request it builds and for refusing a TCP-only
  endpoint; the entitlement check fails on a deliberately bad entitlements file in a test; by hand
  with a debug daemon and debug app: the dialog appears, the approval is recorded as `dev`; the
  webview has no way to call `plugins.approve` with a challenge.
- Risks: until stage 8c, approvals are recorded only by debug daemons.
- Status: done; protocol unchanged (0.56)
- Outcome: `apps/desktop/src-tauri/src/plugin_approval.rs` holds the command
  `plugin_approve(name, action, environment?)`. It is allowed only from `main` and `panel-*`
  (`own_window`, plus the access control list: `build.rs` lists every app command through
  `tauri_build::AppManifest::commands`, so each needs an `allow-<command>` permission, and
  `capabilities/default.json` gives all of them to `["main", "panel-*"]` and nothing remote; a test
  keeps `build.rs`, `generate_handler!` and the capability equal and checks no capability gives
  `plugin_approve` to another window, a remote page or a webview list). It connects with
  `Client::connect_socket` on the endpoint's local socket only (`local_socket` refuses an
  endpoint with none), asks `plugins.inspect` for the preview, shows the `tauri-plugin-dialog`
  message dialog built from that preview alone (id, source, commit, hash, every hook and server
  command line, `LAUNCHER` flags, notes), and on *Approve* installs or updates when the action says so,
  then `plugins.approval_challenge` and `plugins.approve` for the previewed hash and digest on the
  same connection. The `refused_variable` list moved from `app_identity` to `imperium-ipc`
  (`loader_env.rs`, which both the daemon and the app depend on); `app_identity` re-exports it. The
  app captures its environment at start (`Guard`); a refused variable makes `plugin_approve` answer
  `approved: false` naming it and send nothing. macOS: `entitlements.plist` (empty, named in
  `tauri.conf.json`), `scripts/ci/check-entitlements.py`, a bad fixture in `scripts/ci/fixtures/`, and
  a "Check macOS release entitlements" step in `build.yml` that checks the configured file and
  requires the check to fail on the fixture naming all three keys (it also forbids
  `get-task-allow`, which attaching WebKit's inspector needs, and `cs.debugger`). Choices the plan
  did not fix: the challenge is requested after the *Approve* click, not before the dialog (it
  lives two minutes, and an install has no folder to challenge before it); the dialog refuses a
  plugin whose text exceeds 40 lines or has a line over 200 characters instead of cutting it (naming what is too long and pointing to `imperium plugin inspect`), lists each server's environment, header and variable names, and writes control and
  bidirectional characters out; the plugin's own description is not shown; the webview passes
  `name`, `action` and `environment` and nothing else, and gets `{approved, kind, reason,
  cancelled, installed}`; *approve* and *update* resolve a plain name through `plugins.list` to the
  plugin's `name@marketplace`, and a plugin with no marketplace id (pre-existing, user) is
  refused because `plugins.inspect` cannot preview it; *approve* requires the installed copy to
  equal the marketplace's current content. **`SetDefaultDllDirectories`** (user decision): no safe wrapper exists in the lock, so the one
  `unsafe` call lives in `imperium-ipc`'s allowed Windows module (`windows/dll.rs`, with a SAFETY comment) as
  `harden_dll_search() -> io::Result<()>` using `LOAD_LIBRARY_SEARCH_DEFAULT_DIRS`; the app calls it first in
  `main` on Windows and, on error, prints and continues. The linker flag was rejected. Windows-only test: it returns Ok.
  Tests (Rust, in CI): the request parameters carry the previewed hash and digest and an install
  never carries `approve`; a TCP-only endpoint is refused; the windows allowed; what each action
  accepts; the dialog's content, its escaping and its refusal when too long; a refused environment
  variable refuses with its name; the capability and `build.rs` checks above; the interface names
  neither approval method; `refused_variable` moved with its test. In CI: the entitlements check on
  the real file and on the bad fixture (macOS bundle job). Not verified: the by-hand check below, that the call succeeds in the shipped app (the test covers the call), and that Tauri's access control list rejects a mod frame's call (the
  frame's origin handling is Tauri's, tested here only through the capability file).
  By hand, with a debug daemon and the debug app (also try one large plugin, with more than 40 hook lines or a 200-character command: the dialog must refuse it, not cut it) from the same cargo target folder: in a
  scratch config folder add a marketplace holding a plugin with a hook, run
  `imperium plugin install` for it (withheld), then in the app's devtools console run
  `window.__TAURI_INTERNALS__.invoke("plugin_approve", {name: "p@m", action: "approve"})`; the dialog
  appears with the id, hook command and hash, *Approve* returns `{approved: true, kind: "dev"}`
  and `imperium plugin list` shows `dev`; *Cancel* returns `cancelled: true`; start the app with
  `LD_PRELOAD=x` (or `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=x` on Windows) and the answer names the
  variable; with `daemon.socket` off the answer says there is no local socket.
- Handoff notes: from 8a-ii, the daemon does not read the peer's environment on Windows. At
  start the app checks its own environment for `WEBVIEW2_*`, `LD_PRELOAD`, `LD_LIBRARY_PATH`,
  `LD_AUDIT` and `DYLD_*` (`app_identity::refused_variable`'s list) and, if any is set, refuses
  the approval flow (`plugin_approve` answers with the variable's name and sends nothing). Item
  59's Windows signing step must set `IMPERIUM_SIGNED_BUILD` in that step only. **From 8b:** stage 10's
  *Approve* calls `invoke("plugin_approve", {name, action, environment?})` and reads
  `{approved, kind, reason, cancelled, installed}`; show `reason` when `approved` is false, and note
  that `installed: true` with `approved: false` means the plugin is now installed withheld. A
  plugin with no marketplace id cannot be approved until the daemon can preview a folder (a new
  method, so a protocol change, for *Approve for withheld plugins (unrecorded and pre-existing
  too)*). Any new app command needs its name in `build.rs`, the `generate_handler!` list and the
  capability (a test fails otherwise); a new window label that must call commands goes in the
  capability's `windows`, and `plugin_approve` stays limited to `main` and `panel-*`. The signed-app entitlement check (reading the built app's signature with `codesign`, not only the configured file) is left for item 59's signing step. Stage 8c
  adds no app code: the presence prompt is the daemon's, so the approve call may take long
  (the app waits up to five minutes).

## Stage 8c: OS presence check

- Objective: U6. First a prototype per OS of the mechanism in
  [presence check per OS](#presence-check-per-os), run from `imperium-core` itself, recorded in
  this plan's Outcome (works, works through a spawned helper, or infeasible). Then: after a valid
  challenge, the daemon raises the check; success records `counted`; the check unavailable records
  `dialog-only`; a refusal or cancel records nothing. Ship the polkit action file in the `.deb` and
  `.rpm`. Turn counting on.
- Depends on: stages 8a and 8b.
- Owned paths: `crates/imperium-core/src/app_identity/presence/` (new, per OS), `Cargo.toml`,
  `apps/desktop/src-tauri/` packaging for the polkit action (find the Linux bundle settings in
  `tauri.conf.json`), docs 06 and 11, and this plan.
- Acceptance checks: unit tests with an injected presence provider: success → `counted`;
  unavailable → `dialog-only`; cancel → nothing recorded; a presence result sent by a client is
  not a field the protocol accepts. By hand: on a signed macOS release candidate, approving shows
  the system prompt and records `counted`; on a Linux desktop with the package installed, polkit
  prompts and records `counted`, and with the action file removed the approval is `dialog-only`;
  Windows: the prototype's result is recorded (counting there still waits for U7).
- Risks: macOS may require a bundled helper; Windows Hello may be infeasible from the daemon (then
  Windows is `dialog-only` once U7 holds).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 9: Desktop Plugins window: marketplaces, browse and suggestions

- Objective: a Plugins window on the Connectors pattern from #223 (shared `Dialog`, `Tabs`
  *Installed*, *Browse*, *Marketplaces*, search, `EmptyState`, `LoadingState`, `Notice`,
  `.card.interactive` cards, `--row-h` rows). *Browse* (cards with name, marketplace, description,
  category, component counts, *Installed* / *Update available*, and a detail view), *Marketplaces*
  (the official one marked built-in; add, refresh, remove, problems), and repository suggestions
  with *Dismiss* and *No suggestions from this project*. No install button yet. Opened from the
  title bar, the command palette, Settings, and a *Manage plugins* link in Connectors. A separate
  window, because Connectors already has its own *Installed* and *Browse* for MCP servers.
- Depends on: stages 2 and 7 (stage 3b for installed badges).
- Owned paths: `apps/desktop/src/plugins/` (new), where `ConnectorsWindow` is mounted and opened
  (`grep -rn "ConnectorsWindow" apps/desktop/src`), `apps/desktop/src/connectors/InstalledView.tsx`
  (the link only), the command palette entry, `apps/desktop/src/styles/` (a new `plugins.css`
  only if needed), `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests with a mocked client for
  filtering, an external-source card, marketplace add, refresh and remove, dismissing and muting;
  by hand at 1280, 1024 and 420 px, light and dark, by keyboard, against
  [07](../07-desktop-ui.md#theming) and [accessibility](../07-desktop-ui.md#accessibility-implemented).
- Risks: stay on the shared components.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 10: Desktop install, trust and update

- Objective: the trust view (the full [trust prompt](#the-trust-prompt-shows-exactly-what-runs),
  hooks and servers first) for install, update and approve, whose *Approve* invokes
  `plugin_approve`; *Install without approving*; the *Installed* tab with on/off for the user and
  per environment, update, *Approve* for withheld plugins (unrecorded and pre-existing too),
  uninstall with "also delete saved secrets", and secret fields; the approval kind shown
  (`dialog-only`, `carried-over`, `dev`); Connectors' rows show "needs approval". Where the
  platform cannot approve (Windows before U7), *Approve* is replaced by the reason.
- Depends on: stages 5, 8c and 9.
- Owned paths: `apps/desktop/src/plugins/` (`InstalledView.tsx`, `TrustView.tsx`,
  `SecretsForm.tsx`), `apps/desktop/src/connectors/InstalledView.tsx`, `apps/desktop/test/`,
  doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests: every hook and server
  command from a mocked `PluginPreview` is listed; *Approve* invokes `plugin_approve` and the
  webview never calls `plugins.approve`; an update with changed hooks shows the diff; saved
  secrets are never shown; by hand on the signed macOS release candidate: install, approve
  (presence prompt), enable, disable for one environment, update (withheld until approved again),
  uninstall, undo.
- Risks: none beyond stage 8's.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 11: Agents, docs and close-out

- Objective: `install_extension` with `kind = "plugin"` accepts `source = "name@marketplace"`
  (recorded with `origin = agent`, withheld with code), with its schema text; a final pass over
  05, 06, 07, 10, 11 and 14, including the [threat model](#threat-model) and
  [remaining gaps](#remaining-gaps); roadmap item 51 marked implemented; the official marketplace
  browsed live and one plugin inspected, installed and approved on a platform that can approve,
  recorded with the marketplace's commit.
- Depends on: stages 6 and 10.
- Owned paths: `crates/imperium-core/src/extension_install.rs`,
  `crates/imperium-protocol/src/tools.rs`, `crates/imperium-protocol/schemas/tools/install_extension.json`,
  docs 05, 06, 07, 10, 11, 12, 14, and this plan.
- Acceptance checks: an agent install of `x@fixture` with a hook is recorded, withheld and
  unapproved; `python3 scripts/ci/check-links.py .` passes; **U1 check:** the built-in default
  still matches the official marketplace's live source (`.claude-plugin/marketplace.json` present,
  `name` matching), recorded with its commit; **threat-model check:** before this stage closes,
  threat-model case 3, [remaining gaps](#remaining-gaps) and
  [risks across stages](#risks-across-stages) are re-checked against #240 as merged and against
  [05 Isolation tiers](../05-tools-and-isolation.md#isolation-tiers), and any drift is fixed in
  this plan.
- Risks: a real marketplace may use source types left out here; record them as follow-ups.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Risks across stages

- **Approvals are not a boundary against unconfined code** (U5). With agents at tier 0, the
  default, approvals protect against mistakes and protocol-only callers only. Docs and UI must not
  claim more.
- **An approved plugin runs as the user, unconfined.** The inspection makes the decision informed,
  not safe; stage 5's allowlist limits what the code is handed, not what it can do.
- **The OS tier holds only within #240's limits.**
  [#240](https://github.com/Densetsu152637/Imperium-AI/pull/240) (merged at `a58b0efa`) closed the
  daemon escape; the OS tier still holds only within its limits (abstract unix sockets such as X11, other user files,
  the macOS runtime folder and keychain).
- **`dialog-only` approvals** where no presence check exists rest on a click.
- **Platforms that cannot approve.** Windows until [item 59](../12-roadmap.md#next-in-order), and
  machines without the desktop app; plugins with code stay withheld there and the UI says why.
- **Launchers and scripts outside the pin** make the hash cover only what is on disk.
- **`config.set extensions.plugin_dirs`** from any token holder adds an ungated folder.
- **A same-size edit with a restored modification time** is caught only at daemon restart.
- **Format drift**: unknown fields ignored and unknown sources listed, so drift degrades to
  "unsupported".
- **Protocol minors**: concurrent PRs each take the next free minor at merge and update 02 by
  merging, not force-pushing.

## Open questions for the user

None. U1 to U7 are recorded above. The one unverified point, Windows Hello from a windowless
daemon, is resolved by U6's rule (if infeasible, Windows approvals are `dialog-only` once U7
holds) and is prototyped in [stage 8c](#stage-8c-os-presence-check). Item 30's decision on
main-context mods is referenced, not reopened.
