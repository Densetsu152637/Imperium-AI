# Plan: plugin marketplace

Revision: 2026-11-23, revision 3 (the user's decision U4 and plan review round 2); written
against `experimental` at `6000dde5` (last updated). Roadmap
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
[who may approve](#who-may-approve).

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
   goal, as today ([gaps](#remaining-gaps)). Check: stage 3a-i and 4 tests.
7. **Claude Code compatibility.** An unmodified Claude Code marketplace and its plugins load:
   relative, `github` and `url` plugin sources, `plugin.json` component paths and inline
   `mcpServers`, and marketplace entries with `strict: false`. Check: stage 1 parser tests,
   stage 3a-ii's `strict: false` install test; by hand in
   [stage 11](#stage-11-agents-docs-and-close-out), the official marketplace.
8. **Secrets in the keychain.** A value a plugin's MCP server needs is stored in the secret
   store and never written to a config file, the plugin folder, the journal or a log. Check:
   stage 5 tests.

## User decisions

Made by the user on 2026-11-22 (U1 to U3) and 2026-11-23 (U4).

- **U1. The official marketplace is pre-added.** Anthropic's official Claude Code plugin
  marketplace is a built-in entry of `plugins.marketplaces`. Its exact source is not recorded in
  this repository, so the plan names it **"the official marketplace source, confirmed at
  implementation"**; [stage 2](#stage-2-marketplaces-in-the-daemon) confirms it from Anthropic's
  Claude Code documentation, records the URL in this plan and pins it in the build, and
  [stage 11](#stage-11-agents-docs-and-close-out) checks it against the live repository. Its name
  is reserved for that pinned source, even while the user has it turned off, and "built-in" is
  decided by source, not by name. Pre-adding it grants no trust: its plugins (some are
  third-party) need the same approval as any other.
- **U2. A trusted project may suggest marketplaces and plugins.** A project folder in
  `mcp.trusted_projects` may list, in its `.imperium/config.toml`, marketplaces and plugins to
  **offer** the user. Offers are shown only; they never add a marketplace, install, approve or
  enable anything. Accepting one runs the user's normal flow
  ([stage 7](#stage-7-repository-suggestions)).
- **U3. Plugin hooks block, never rewrite.** For every plugin under `<config>/plugins`, hooks
  may block but never rewrite a tool call or prompt, including after an approval, for now.
- **U4. Only the installed desktop app's native dialog approves.** An approval that lets a
  plugin's hooks or servers run counts only when it comes from the native dialog of the
  **installed, release-signed** desktop app, verified by the daemon as described in
  [who may approve](#who-may-approve). CLI approvals do not count, whatever terminal they come
  from, until roadmap [item 30](../12-roadmap.md#next-in-order)'s stronger caller check exists.
  The reason is a demonstrated bypass of `peer::may_grant`: a process detached with a clean
  environment (started through WMI, `schtasks` or Explorer on Windows; `setsid` with a double
  fork and `env -i` on Unix) passes it and could send an approval with no dialog at all, a case
  `crates/imperium-core/src/peer.rs` documents.

## Scope and non-goals

In scope: the marketplace format and the plugin manifest as Claude Code publishes them (the
public format, from Imperium's existing parser and public documentation; no external site was
fetched for this plan), marketplace storage, inspection and approval, install and lifecycle,
plugin secrets and child environments, repository suggestions, CLI, protocol and desktop
surfaces (including the daemon's verification of the desktop app), and the agent tool
`install_extension` taking `name@marketplace`.

Non-goals (each with what would reopen it):

- **A hosted Imperium registry** or search across marketplaces the user has not added (the
  roadmap's "plugin registry" candidate). Reopen when there is a service to host it.
- **Automatic updates.** Refresh reports what changed; updating is always an explicit action.
  Reopen if the user asks for scheduled updates (they would still need a counted approval).
- **Signature verification of plugins** (see
  [provenance](#provenance-and-signatures-declined-for-now)). The desktop app's own code
  signature is checked (U4); plugins' are not.
- **A marketplace served as a bare `marketplace.json` URL**, and **npm or pip plugin sources**.
  They are reported as unsupported with the source type named. Reopen when a marketplace the user
  wants is published only that way (including the official one, if stage 2 finds it is).
- **Sandboxing plugin hooks and MCP servers.** They run as today: as the user, outside the
  isolation tier ([assumptions](#assumptions)). The approval, and stage 5's environment
  allowlist, are the controls. Reopen with roadmap work on running local servers and hooks inside
  the isolation tier ([05](../05-tools-and-isolation.md#mcp)).
- **Claude Code command features that run code**: `!` shell lines in command templates and
  `allowed-tools` front matter. Imperium's commands stay text; the inspection lists such commands
  with a warning.
- **Repository-scoped installs** (Claude Code's project scope writes enablement into the
  repository). A repository's `.imperium/config.toml` cannot add marketplaces, install, approve
  or enable a plugin; it may only suggest (U2). A trusted repository can still add
  `extensions.plugin_dirs` whose plugins load ungated, as today
  ([14](../14-projects-and-automation.md#environment-configuration-implemented)); see
  [gaps](#remaining-gaps).
- **Applying plugin changes to running sessions or without a daemon restart.** Today's rule
  stands: new sessions see changes, and the Connectors notice about restarting stays where it
  applies.
- **Imperium-native `imperium-plugin.toml` manifests and plugin permission manifests**
  ([06](../06-extensibility.md#permissions-model-for-extensions)).
- **Hook rewrites** (U3). Reopen only by a user decision.
- **CLI approvals** (U4). Reopen when item 30's stronger check exists.
- **Approving on a machine without the installed desktop app** (headless, SSH-only, phone).
  Plugins with hooks or servers stay withheld there. Reopen with item 30.
- **The trusted main-context mod loader.** This plan builds a daemon-side check of the desktop
  app's identity for plugin approvals; whether that check is enough for main-context mods stays
  item 30's open decision, blocked on the user, and is not resolved here.

## Assumptions

Checked against the source at `6000dde5`:

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
  `.imperium-source.json` inside the plugin folder, which the plugin's own files (and anything
  that can write there) control. This plan replaces it with a daemon-owned record
  ([stage 3a-i](#stage-3a-i-record-store-and-the-load-time-gate)).
- **Today's switch is uncounted.** The Connectors switch turns a plugin on by editing
  `extensions.disabled_plugins` through `config.set`, which any holder of the daemon token can
  call, including over loopback TCP. So can `extensions.plugin_dirs`.
- `imperium-core::mod_install` has the inspect-then-install pattern with a content hash
  (`tree_sha256`), a daemon-owned approval store (`mod-approvals.json`) and a `modified` state.
  `crate::peer::may_grant` (local socket, the user's own process, not a descendant of the daemon,
  not marked as spawned by it, not over SSH;
  [06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)) is **not a boundary**:
  its module documentation says a process that detaches and clears its environment passes it.
- The desktop app's Rust side talks to the daemon with `imperium-client`, which tries the local
  socket first (`apps/desktop/src-tauri/src/daemon.rs`); it depends on `tauri-plugin-dialog`.
  Installers ship `imperium-core` beside the app.
- **Release signing today** ([11](../11-ci-cd.md#secrets)): macOS builds are signed and notarised
  when the Apple secrets are set; **Windows builds are not code-signed** (Azure Trusted Signing
  "to be decided"); Linux packages are not signed. The Windows installer uses Tauri's NSIS
  defaults (a per-user install unless configured otherwise), so its folder is writable by the
  user's own processes.
- Plugin MCP servers have an empty `allow` list, so each of their tool calls asks unless a
  permission rule allows it.
- **Hooks and stdio MCP servers are not confined and inherit the daemon's whole environment.**
  [06 Hooks](../06-extensibility.md#hooks) says hooks run inside the session's isolation tier, but
  `hooks.rs` runs them with `imperium_tools::shell` (plain `cmd /C` or `sh -c`, adding the
  spawned-by-daemon mark), and local MCP servers are not confined either (listed as planned in
  [05](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented)'s
  section). Both get every variable of the daemon's environment; only `${VAR}` **expansion** in a
  downloaded plugin's `.mcp.json` is restricted today. Stage 3a-i fixes the 06 line; stage 5 adds
  an environment allowlist.
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

- `crates/imperium-extensions/src/plugins.rs`, `lib.rs` (plugin discovery and session loading)
- `crates/imperium-extensions/src/hooks.rs`, `crates/imperium-mcp/` (child processes)
- `crates/imperium-core/src/extension_install.rs` (fetch, staging, provenance, uninstall)
- `crates/imperium-core/src/mod_install.rs` (inspect, approvals, content hash)
- `crates/imperium-core/src/connectors.rs` (`mcp.list`'s `PluginInfo`), `state/extensions.rs`
  (the `Sources` a session loads), `environments.rs`, `peer.rs`, `config_changes.rs`, `rpc.rs`
- `crates/imperium-config/src/settings.rs`, `environment.rs`, `mcp_servers.rs` (comment-keeping
  config edits)
- `crates/imperium-protocol/src/methods/` (new `plugins.rs`), `lib.rs` (`PROTOCOL_VERSION`),
  `snapshots/methods.txt`, `packages/protocol/src/generated/`
- `crates/imperium-policy/src/permissions.rs` (`FLOOR_PATHS`, `protect_dir`), `floor.rs`
- `crates/imperium-cli/src/main.rs`, `mods.rs` (prompt pattern), new `plugins.rs`
- `apps/desktop/src-tauri/src/` (the approval command), `apps/desktop/src-tauri/tauri.conf.json`
  (install mode), `apps/desktop/src/connectors/` (the pattern), new `apps/desktop/src/plugins/`
- `.github/workflows/build.yml` (signing)

## Related docs

[06 Extensibility](../06-extensibility.md), [05 Tools and isolation](../05-tools-and-isolation.md),
[10 Configuration](../10-configuration.md), [14 Environments](../14-projects-and-automation.md#environment-configuration-implemented),
[07 Desktop](../07-desktop-ui.md#connectors-implemented), [11 CI/CD](../11-ci-cd.md#secrets),
[02 Protocol](../02-architecture.md#protocol). For behaviour already built, those docs win; for
anything planned here, this plan wins until its stage merges and updates them.

## Decision

**Chosen: a marketplace layer on the existing fetch, a daemon-owned record per plugin folder,
and approvals that count only from the verified installed desktop app.** Marketplaces are git
clones cached in the data folder; installs reuse `extension_install`'s fetch and land in
`<config>/plugins/<name>`, where today's discovery finds them; the daemon keeps, in its data
folder, a record per folder (origin, source, commit, content hash, and the counted approval), and
the session loader withholds any folder under `<config>/plugins` with code that does not match a
record with a counted approval, by folder path, whatever the plugin's own files or
`extensions.disabled_plugins` say.

Rejected:

- **Only extending `install_extension` with more source forms.** No browsing, no catalogue
  refresh, no update path, and no approval: it fails goals 2, 3 and 6.
- **Driving Claude Code's own `claude plugin` CLI or sharing `~/.claude/plugins`.** Imperium must
  work without Claude Code installed, and Claude Code's trust decisions are not Imperium's.
- **Allowing two installed plugins of the same name from different marketplaces** (Claude Code
  does). Skills, commands (`/<plugin>:<name>`), server names (`<plugin>_<server>`),
  `extensions.disabled_plugins` and agent profiles all key on the plugin name. An install whose
  name is taken is refused, and the folder name must equal the manifest name. Revisit if a real
  collision is reported.
- **Approving only the hook and server command lines.** Hook commands usually run scripts inside
  the plugin, so a same-command update can change all the code that runs. The approval covers the
  content hash **and** the run summary.
- **Trusting `.imperium-source.json` or the manifest name for gating.** Both are plugin-controlled.
- **Gating only recorded folders.** Moving a recorded folder (or copying one in) would escape the
  gate; every folder under `<config>/plugins` with code is gated, recorded or not.
- **Approvals from any token holder, or from `may_grant` callers.** Both are bypassable by a
  process of the user's (U4).

## Security model

Installing a plugin is installing code from the internet: once enabled, its hooks run on every
matching event and its stdio MCP servers run as processes of the user, outside any isolation
tier, with the daemon's whole environment until stage 5 narrows it. Its skills and commands are
text that steers agents. The controls below are what stands between a marketplace and that code.

### Nothing runs before approval

- Adding or refreshing a marketplace clones it (the existing safe `git` invocation) and parses
  JSON. Browsing reads the cached clone. Inspecting fetches the plugin to a staging folder and
  parses it. Installing **fetches again**, checks that the content still has the inspected hash,
  and moves that copy into place. None of these runs a hook, a server, a build step, an install
  script, or a git hook.
- A plugin with hooks or MCP servers is installed **withheld**; it loads only after a counted
  approval ([who may approve](#who-may-approve)). Skills-and-commands-only plugins load once
  installed, as today, but are still bound to their recorded hash.

### The trust prompt shows exactly what runs

`plugins.inspect` returns, every surface shows before installing or updating, and the desktop's
native approval dialog repeats in short:

- **Identity and provenance:** `name@marketplace`, version, the marketplace's source and the
  commit it was read at, the plugin's own source (same repository as the marketplace, or a
  third-party repository) and the exact commit fetched, and author, homepage and licence
  **labelled as the plugin's own unverified claims**.
- **Every hook:** event, matcher, and the full command line or URL, with `${CLAUDE_PLUGIN_ROOT}`
  expanded; all can block, none can rewrite (U3).
- **Every MCP server:** stdio command and arguments, or remote URL and header names; the
  environment variables and `${VAR}` references it uses, and which of them it needs as secrets.
- **Launchers that fetch more code at start** (`npx`, `uvx`, `bunx`, `pipx run`, `docker run`
  with an image not pinned by digest, any `@latest`): flagged as "downloads code each start that
  the commit pin does not cover". **A missing flag is not a guarantee:** any hook script or server
  can download and run code on its own; the flag only catches the common launchers.
- **Text components:** commands, agents and skills by name; commands with `!` shell lines or
  `allowed-tools` flagged as not run by Imperium.
- **Content hash:** SHA-256 over the plugin folder's files (paths and bytes, without `.git` and
  Imperium's own `.imperium-*.json`), including `.claude-plugin/marketplace-entry.json`, the
  canonical JSON of its marketplace entry that the install writes
  ([stage 3a-ii](#stage-3a-ii-inspect-and-install)).
- **For an update:** a diff of hooks and servers (added, removed, changed lines) and a statement
  that other files changed, when they did.

### Pinning

- An install pins the plugin to the commit fetched (a marketplace entry's `sha` or `ref` is
  honoured; otherwise the default branch's head at inspect time). The marketplace clone is pinned
  to the commit of its last refresh. Neither moves without an explicit refresh or update.
- `--sha256` (CLI) and `sha256` (protocol) pin the content hash a previous inspection showed.

### Updates re-ask

- An update inspects the new version and compares its hash with the approved one. Any content
  change of a plugin with hooks or servers in either version installs the new content
  **withheld** until a fresh counted approval. A plugin with neither installs and loads.
- An update reads the source from the daemon's record, never from the plugin folder, and refuses
  a different marketplace or source: to move, uninstall and install afresh.

### The load-time gate

At session start the daemon looks at every folder under `<config>/plugins`. It withholds the
**whole plugin** (skills and commands too) from the new session, whatever
`extensions.disabled_plugins` says, when any of these holds:

1. the folder has hooks or servers and **no record**: reason "not installed by Imperium: approve
   to load". Approving it in the desktop app writes a record with `origin = user`;
2. the folder name, the recorded name and the manifest name are not all equal;
3. the folder contains a symbolic link or a reparse point (junction) anywhere;
4. its content hash differs from the recorded one;
5. it has hooks or servers and there is no counted approval, or the approval's hash or run
   summary (`runs()`, [stage 1](#stage-1-marketplace-and-manifest-parsers)) differs from what is
   there now, so a new kind of component also needs a new approval.

An unrecorded folder with neither hooks nor servers loads as today. Gating is by canonical folder
path, not by name. A withheld plugin is reported with the reason (`plugins.list`, `mcp.list`'s
`withheld`). The hash is computed for every folder when the daemon starts and kept in memory with
its cache key: the sorted list of every file's relative path, size and modification time. A
session start re-walks that list (metadata only) and rehashes a folder whose list differs, so a
rename, an added or removed file, or a changed size or time is caught (an edit that keeps the size
and restores the time is caught at the next daemon start: [gaps](#remaining-gaps)).

**Migration at stage 3a-i's first daemon start.** Every folder already in `<config>/plugins` is
recorded once with `origin = preexisting` and its current hash and run summary. A folder that is
not in `extensions.disabled_plugins` gets a **carried-over** approval (marked as such), so nothing
the user runs today stops working; a folder that is in it gets no approval and needs the desktop
dialog to be turned on. The store records that the migration ran, so a folder that appears later
is unrecorded.

### Provenance and signatures (declined for now)

Claude Code marketplaces carry no signatures, and verifying git commit signatures needs an
allowed-signers list nobody publishes for marketplaces. This plan **declines signature
verification of plugins** and shows provenance instead (sources, commits, hash, unverified author
claims). Revisit when the Claude Code format adds signatures or the user wants `git verify-commit`
against a signers file they maintain.

### Who may approve

**Only the installed desktop app's native dialog (U4).** The daemon has to tell a click in that
dialog apart from a script on its socket. The options weighed:

| Option | What it stops | What it does not stop |
| --- | --- | --- |
| (a) **Verify the peer process is the installed, signed app**: its executable path is the app installed beside `imperium-core`, and its code signature is Imperium's (Authenticode on Windows, the designated requirement with Imperium's team id on macOS) | Any other program on the socket: a script, the CLI, an agent's detached process, a renamed or rebuilt copy of the app | Code running **inside** the genuine app (Windows same-user process injection; Linux `ptrace` by the process that launched it or `LD_PRELOAD`); replacing `imperium-core` itself where the install folder is writable by the user; input automation clicking the dialog |
| (b) **A per-approval challenge** the daemon issues and the app returns | Replay, and an approval for other content than the dialog showed (the challenge is bound to the action, plugin, hash and run summary, single-use, two minutes, same connection) | Anything (a) does not stop. A challenge *signed with an app-held key* adds nothing on Windows and Linux, where any process of the user can read the key (DPAPI and the Secret Service are per user, not per app); on macOS a keychain item restricted to the signed app would work, but (a) already gives that assurance |
| (c) **An OS user-presence prompt** the daemon itself raises (Windows Hello consent, macOS Local Authentication with Touch ID or the password, Linux polkit with an installed action) | Input automation and scripted clicks: these prompts take a fingerprint, PIN or password, or run where other processes cannot reach | Nothing on its own about what was approved (the native dialog shows that); unavailable where Hello is not set up, on headless Linux, or without a polkit agent |

**Chosen: (a) and (b) together, required; (c) per [open question 1](#open-questions-for-the-user).**
An approval is **counted** only when all of these hold, checked by the daemon on the call:

1. It arrives on the **local socket** (never loopback TCP, never a paired phone, never a mod frame).
2. The **peer process** (from the socket's peer credentials; on macOS its audit token, so a reused
   process id cannot pass) is the **installed desktop app**:
   - **Windows:** its image path (`QueryFullProcessImageNameW`) is the app executable in the
     daemon's own install folder; the file's Authenticode signature is valid and chains to the
     publisher certificate pinned into release daemons; the install folder is not writable by the
     user's standard token (a per-machine install); the process is not being debugged
     (`CheckRemoteDebuggerPresent`).
   - **macOS:** `SecCodeCopyGuestWithAttributes` on the audit token, checked against the
     designated requirement (Apple-anchored, Imperium's team id, the app's identifier) pinned into
     release daemons; the app is built with the hardened runtime and without `get-task-allow`, so
     other processes cannot attach to or inject into it, and macOS's App Management protection
     guards the signed bundle.
   - **Linux:** no code signature exists to check. The image path (`/proc/<pid>/exe`) is the
     packaged app (a `.deb` or `.rpm` install), and it and every folder above it are owned by root
     and not writable by group or others; the process is not traced (`TracerPid` is 0) and its
     environment has no `LD_PRELOAD`, `LD_LIBRARY_PATH` or `LD_AUDIT`. An AppImage or any other
     user-writable location **never counts**.
3. It carries the **challenge** (b) the daemon issued to that same connection for that action,
   plugin, hash and run summary, unused and unexpired.
4. Where (c) is in force, the OS prompt succeeded.

The desktop's flow: the Plugins window asks the app's Rust side through a Tauri command
(`plugin_approve`); the Rust side requests a challenge, shows a **native dialog**
(`tauri-plugin-dialog`, not a webview) with the plugin id, what it runs and the hash, and on
*Approve* sends the approval with the challenge over its own local-socket connection. The webview
never sends an approval, and could not make one count if it did.

**Dev and unsigned builds.** A release daemon never counts a debug, unsigned or differently
signed app. A debug daemon (`cfg(debug_assertions)`) uses a development verifier that accepts the
app built in the same cargo target folder; approvals it records are marked `dev`, and a release
daemon treats `dev` approvals as absent. Tests inject the verifier.

**Where nothing counts today.** Until Windows releases are Authenticode-signed and installed
per-machine, no approval counts on Windows release builds; on Linux, only root-owned package
installs count; on any machine without the installed app (headless, SSH-only), nothing counts.
Plugins with hooks or servers stay withheld there (carried-over approvals keep working). See
[open question 2](#open-questions-for-the-user) for Windows in the meantime.

**The CLI records nothing that counts.** The CLI has **no approving command**, and an approval
sent by the CLI or any other caller that fails the checks records nothing (`approved: false`,
with the reason). CLI commands that remain, none of which can make code run that the user has not
approved in the desktop: `plugin marketplace add|list|refresh|remove`, `plugin browse`,
`plugin inspect`, `plugin install` (a plugin with code installs withheld and the CLI says to
approve it in the desktop app), `plugin update` (likewise), `plugin enable|disable` (enabling
works only for a plugin with a counted approval), `plugin uninstall`, `plugin list`,
`plugin secret set|delete`, and `plugin suggestions`.

**Agents.** `install_extension` (any source, including `name@marketplace`) installs as today:
withheld when the plugin has hooks or servers. The safety floor asks about an agent's shell
running `imperium plugin marketplace add`, `imperium plugin install|update|enable` and
`imperium config set|unset` of `extensions.disabled_plugins` or `extensions.plugin_dirs`, and
about file-tool writes into `<config>/plugins`, `<data>/marketplaces` and `plugin-installs.json`
([stage 6](#stage-6-cli-and-the-safety-floor)). The floor is a guard against mistakes, not a
boundary ([05](../05-tools-and-isolation.md#permissions)). What stops an agent approving its own
download is that it cannot pass check 2; what it can still do is in [remaining gaps](#remaining-gaps).

**Turning a plugin off and on again** does not revoke its approval; `plugins.set_enabled` may
turn an approved plugin back on from any caller, since the user already approved that content.

### What an approved plugin still cannot do

- **Permission rules, the safety floor and agent profiles are unchanged.** A plugin carries no
  permission rules; its MCP tools (`mcp__<plugin>_<server>__<tool>`) ask unless a user rule allows
  them; floor-covered calls ask every time; an agent's `plugins` allowlist and local-files-only
  lock ([14](../14-projects-and-automation.md#agent-permission-profiles-implemented)) still keep a
  plugin's servers and hooks out of that agent's sessions.
- **Hooks block but never rewrite** (U3): `may_rewrite = false` for every plugin under
  `<config>/plugins`.
- **No daemon environment through `${VAR}`.** `${VAR}` in such a plugin's `.mcp.json` expands only
  `CLAUDE_PLUGIN_ROOT` and the plugin's own secrets, never the daemon's environment. From stage 5,
  the **processes** (hooks and stdio servers) get only an allowlisted environment too. Until then
  they inherit the daemon's whole environment, and the docs say so.
- **A repository cannot install or enable one.** A trusted project's `.imperium/config.toml` may
  suggest (U2) and may disable plugins; it cannot add marketplaces, install, approve or enable.

### Remaining gaps

Stated plainly, so no surface claims more:

- **Input automation.** A process of the user's can click the native dialog (`SendInput`, UI
  Automation, `xdotool`) unless (c) is in force; the dialog alone does not stop it.
- **Code inside the genuine app.** On Windows a same-user process can inject into the running
  app; on Linux the process that launched the app may `ptrace` it after the check. macOS's
  hardened runtime blocks both.
- **A writable install.** Where the app and `imperium-core` sit in a folder the user's processes
  can write, an attacker can replace the daemon itself, and no check inside it means anything;
  that is why such installs never count.
- **`extensions.plugin_dirs` is ungated.** Any holder of the daemon token can `config.set`
  `extensions.plugin_dirs` to add a folder whose plugins load with hooks and servers and no
  record; a trusted repository can add one too. The floor asks about an agent's shell doing it,
  which is not a boundary. Gating `plugin_dirs` is a follow-up outside this plan.
- **Same-size edits with a restored modification time** are caught only at the next daemon
  start, not at the next session start.
- **Hooks and servers are unconfined.** An approved plugin runs as the user.

### Secrets

A server's `${VAR}` that is not `CLAUDE_PLUGIN_ROOT` is a **required secret** of the plugin. The
user sets it with `imperium plugin secret set <plugin> <VAR>` (value from standard input or a
prompt without echo), the desktop's password field, or `plugins.set_secret`; it is stored in the
secret store as `plugin.<plugin>.<var>` (lowercased; a name over 64 characters is refused with
the reason) and read only when that plugin's server starts. A plugin with a missing required
secret is listed as such and its server does not start; the rest of the plugin works.
Uninstalling offers to delete its secrets (CLI flag `--delete-secrets`, a checkbox in the
desktop), as `mcp.remove` does. Setting a secret is not an approval and runs nothing.

### Mod trust is separate

UI mods keep their own install, permissions and trust ([06](../06-extensibility.md#installing-and-updating-mods-implemented)).
A Claude Code plugin cannot contain a mod in this plan; a marketplace entry that is a mod
(`mod.json`) is listed as unsupported.

## Storage layout and configuration

| What | Where | Owner |
| --- | --- | --- |
| Marketplaces | built-in default: the official marketplace (U1), its name reserved for its pinned source; user-added: `[plugins.marketplaces.<name>]` in `<config>/config.toml` (`source`, optional `revision`, `enabled = false` to turn the built-in one off), written with comments kept, as `[mcp.servers]` entries are | defaults, user config |
| Marketplace clone | `<data>/marketplaces/<name>/`, made on first browse or refresh (never at daemon start), replaced atomically through a staging folder | daemon cache, recreatable |
| Marketplace state | `<data>/marketplaces.json`: per marketplace the commit read, refresh time, and problems | daemon |
| Installed plugin | `<config>/plugins/<name>/`; folder name = manifest name; `.claude-plugin/marketplace-entry.json` for a marketplace install; `.imperium-source.json` kept for display only | as today |
| Records and approvals | `<data>/plugin-installs.json`, keyed by canonical folder path: name, origin (`marketplace`, `agent`, `user` or `preexisting`), id (`name@marketplace` when there is one), source and commit, content hash, run summary, and an optional approval (hash, run summary, time, kind: `counted`, `carried-over` or `dev`); and whether the migration ran | daemon |
| On/off for the user | `extensions.disabled_plugins` (unchanged key; it can only turn off, never past the gate) | user config |
| On/off per environment | `plugins: { enabled: [...], disabled: [...], dismissed_suggestions: [...], muted_suggestion_sources: [...] }` on the environment record in `<data>/environments.json` | user-owned |
| Suggestions | `[plugins] suggest_marketplaces` and `suggest_plugins` in a trusted project's `.imperium/config.toml` (U2) | repository, offers only |
| Secrets | secret store, `plugin.<plugin>.<var>` | keychain |
| Removed plugins | `<data>/config-trash/`, with a `config.changes` entry so `config.undo` restores it (with its record) | as today |

**Effective state for a new session**, in order: passes the [load-time gate](#the-load-time-gate);
not in `extensions.disabled_plugins`, unless the session's environment record lists it in
`enabled`; not in the environment record's `disabled`; not disabled by a trusted repository's
`.imperium/config.toml`; allowed by the agent profile.

**Configuration layers.** `plugins.marketplaces` is read from built-in defaults (the official
marketplace), the user config and environment variables (per [10](../10-configuration.md#layers);
stage 2 checks how that layer spells a table keyed by name and documents it); an environment
variable can add a marketplace to browse, never an install or approval, and never takes the
built-in name with another source. The environment layer accepts only `[plugins]
suggest_marketplaces` and `suggest_plugins`, and only from a trusted folder; every other
`[plugins]` key is refused, and `imperium env config` shows what it accepted and refused.

## Protocol additions

A new `crates/imperium-protocol/src/methods/plugins.rs`, with TypeScript types generated for the
desktop. A paired phone cannot call `plugins.*` (as with `mods.*`). Each stage that adds methods
takes **the next protocol minor at its merge** and updates `PROTOCOL_VERSION`, the version line
in [02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method | Stage | Does |
| --- | --- | --- |
| `plugins.marketplace_add` | 2 | `{source, revision?}` → `MarketplaceInfo`. Clones, parses, records; refuses a name already added or the built-in name with another source |
| `plugins.marketplace_list` | 2 | → `MarketplaceInfo[]` (name, source, `built_in` by source, commit, refreshed time, plugin count, problems) |
| `plugins.marketplace_refresh` | 2 | `{name?}` → per marketplace the commit before and after and plugins added, removed and changed |
| `plugins.marketplace_remove` | 2 | `{name}`; the built-in one is turned off, not freed; installed plugins stay installed |
| `plugins.browse` | 2 | `{marketplace?, query?}` → `PluginListing[]` (id, metadata, components or `null` for an external source, installed version and `update_available`) |
| `plugins.inspect` | 3a-ii | `{id, revision?}` → `PluginPreview` ([trust prompt](#the-trust-prompt-shows-exactly-what-runs) contents, `sha256`, run summary, installed comparison) |
| `plugins.install` | 3a-ii | `{id, sha256, environment?}` → `{name, path, withheld, reason, notes}`; never approves |
| `plugins.uninstall` | 3b | `{name, delete_secrets}` → the trash location and change id |
| `plugins.list` | 3b | `{environment?}` → plugins under `<config>/plugins` with id, origin, source, commit, approval kind, `withheld` and its reason, effective on/off and what decided it, required and missing secrets |
| `plugins.update` | 4 | `{name, sha256}` → result like install, with `replaced`; never approves |
| `plugins.set_enabled` | 4 | `{name, enabled, environment?}`; enabling a plugin the gate withholds is refused with the reason |
| `plugins.approval_challenge` | 8a | `{folder or name, sha256, runs_digest}` → `{challenge, expires}`; answered only to a verified app peer |
| `plugins.approve` | 8a | `{folder or name, sha256, runs_digest, challenge}` → `{approved, reason}`; counts only under [who may approve](#who-may-approve), records nothing otherwise |
| `plugins.set_secret`, `plugins.delete_secret` | 5 | `{name, variable, value?}`; the value never returns |
| `plugins.suggestions` | 7 | `{environment}` → offers from the environment's trusted project files, with the file they came from and whether each is already added or installed; `plugins.dismiss_suggestion {environment, offer or source_folder}` |
| `EnvironmentInfo.plugins` | 4 | the per-environment lists, also on `environments.update` |

`mcp.list`'s `PluginInfo` gains optional `id`, `origin` and `withheld` (stage 3b) so the
Connectors window keeps working and can say "needs approval".

## Stages

Ordered by dependency; each is one pull request of about 800 changed lines or fewer, tests
included. Every stage: branch from `experimental`, update the docs its behaviour changes, run
`cargo fmt --all --check` locally, and rely on CI for `bash scripts/check-rust.sh` (clippy,
tests, generated types and snapshots) and `bash scripts/check-web.sh`. A stage that grows past
the limit splits at the seam its handoff notes name. Until [stage 8a](#stage-8a-daemon-verification-of-the-desktop-app)
merges, the daemon's production verifier counts nothing, so no new approval can be made; earlier
stages test counted approvals through an injected verifier or records written directly.

## Stage 1: Marketplace and manifest parsers

- Objective: pure parsing in `imperium-extensions`, no daemon or protocol change.
  (a) `marketplace.rs`: parse `.claude-plugin/marketplace.json` (`name`, `owner`, `metadata`
  including `pluginRoot`, `plugins[]` with `name`, `source`, `description`, `version`, `author`,
  `homepage`, `repository`, `license`, `keywords`, `category`, `tags`, `strict`, and the
  component fields). Sources: a relative path (resolved under `pluginRoot`, refused if it leaves
  the repository), `{source: "github", repo, ref?, sha?}` (→ `https://github.com/<repo>.git`),
  `{source: "url", url, ref?, sha?}`; anything else is an `Unsupported(type)` entry, not an error.
  Unknown fields are ignored. Names must pass the existing `check_name` rules. A function writes
  an entry's **canonical JSON** (sorted keys, no insignificant whitespace) for stage 3a-ii.
  (b) `plugins.rs`: read the full `plugin.json` (`version`, `author`, `homepage`, `repository`,
  `license`, `keywords`, and `commands`, `agents`, `hooks`, `mcpServers` as a path, a list of
  paths, or for `hooks` and `mcpServers` an inline object), honour those paths (inside the
  plugin root only; a path through a link is refused) in addition to the default folders. A
  folder is also a plugin when it has `.claude-plugin/marketplace-entry.json` and no
  `plugin.json`: the entry's fields are its manifest (`strict: false`); with both files,
  `plugin.json` wins and the entry's component fields add to it.
  (c) `fn runs(&Plugin) -> RunSummary`: each hook (event, matcher, expanded command or URL) and
  each server (command, args, URL, header names, `${VAR}` references), serialisable, comparable
  and digestible, with the fetches-code-at-start flag for the launchers listed in the security
  model.
  (d) Replace `installed_by_agent` (which reads the plugin-controlled `.imperium-source.json`) with
  a `downloaded` set of folders passed in `Sources` by the caller: a plugin in that set never
  expands daemon `${VAR}` and its hooks get `may_rewrite = false`. Until stage 3a-i fills the set,
  the caller passes the folders `installed_by_agent` would have named, so this stage changes no
  behaviour.
- Depends on: none.
- Owned paths: `crates/imperium-extensions/src/marketplace.rs` (new), `plugins.rs`, `lib.rs`
  (module, re-exports, `Sources.downloaded`), `crates/imperium-core/src/state/extensions.rs` (only
  to fill `downloaded` as before), `crates/imperium-extensions/Cargo.toml` if a dev-dependency is
  needed.
- Acceptance checks: unit tests (in CI, `cargo test -p imperium-extensions`) for: each source
  form; `pluginRoot`; a relative source with `..` refused; an unsupported source listed;
  `plugin.json` custom paths and inline `mcpServers` and `hooks` loaded; a path outside the root
  or through a link refused; a folder with only `marketplace-entry.json` discovered and loaded;
  canonical JSON stable across key order; `runs` flags `npx -y pkg@latest` and two summaries of
  the same plugin compare equal; a plugin in `downloaded` never reads the daemon environment in
  `${VAR}` and its hooks get `may_rewrite = false`; existing `plugins.rs` tests unchanged and
  passing.
- Risks: the public format has more fields than listed; unknown ones must stay ignored. Component
  path semantics follow Claude Code's documented "adds to the default folder"; record the choice
  in the module doc.
- Status: pending
- Outcome:
- Handoff notes: keep parsing free of I/O beyond reading the given folder, so stage 2 can parse a
  clone and stage 3a-ii a staging copy with the same functions.

## Stage 2: Marketplaces in the daemon

- Objective: add, list, refresh and remove marketplaces, and browse them, through the protocol.
  `source` accepts a git URL, a local folder (absolute, or `~`), or `owner/repo` (exactly two
  name segments and no scheme → GitHub HTTPS; a folder that exists wins, and the result says which
  was chosen). Clone with `extension_install`'s helpers, made `pub(crate)` where they are private
  (`git_fetch`, `git`, `copy_tree`) without moving or changing them. A local folder is read in
  place, not copied, and re-read on refresh. Write `[plugins.marketplaces.<name>]` with
  comment-keeping edits (follow `crates/imperium-config/src/mcp_servers.rs`), and
  `<data>/marketplaces.json`. `plugins.browse` reads components from the clone for relative
  sources, `null` for external ones. Add `plugins` settings to `imperium-config` with **the
  official marketplace as a built-in default** (U1): its source pinned in the build, cloned lazily
  on first browse or refresh; `built_in` is true only for an entry whose source equals the pinned
  one; its name is reserved for that source in every layer, even while `enabled = false`; removing
  it writes `enabled = false`. Make the environment layer refuse `[plugins]` (stage 7 later accepts
  the two suggestion keys).
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/marketplaces.rs` (new), `extension_install.rs`
  (visibility only), `rpc.rs` (dispatch), `crates/imperium-config/src/settings.rs`,
  `environment.rs` (refusal), `crates/imperium-protocol/src/methods/plugins.rs` (new),
  `methods/mod.rs`, `lib.rs` (version), `snapshots/methods.txt`,
  `packages/protocol/src/generated/`, docs 02 (version line), 06 (a new "Plugin marketplaces"
  section: marketplaces), 10 (`plugins.marketplaces`), and this plan (U1's confirmed source).
- Acceptance checks: daemon tests with a fixture marketplace in a temporary git repository and a
  folder: add (each source form; `owner/repo` asserted by URL without fetching), list, refresh
  after a new commit reports the changed entry, remove; a marketplace whose name is taken is
  refused; a malformed `marketplace.json` is reported in `problems` without failing the list;
  the marker-file plugin's hook and server have not run after add, refresh and browse (goal 5);
  the built-in official entry is listed without any network access at daemon start; removing it
  writes `enabled = false`; adding a marketplace with the built-in name and another source is
  refused, both while it is on and while it is off, from the user config and from an environment
  variable; an entry with the pinned source under another name is not marked `built_in`; an
  environment `.imperium/config.toml` with `[plugins]` is refused (`imperium-config` test); the
  methods snapshot lists the five methods; protocol minor bumped to the next at merge. **U1
  check:** the implementer confirms the official marketplace's source from Anthropic's Claude Code
  documentation, records the URL and the documentation page in this plan's Outcome, and a unit
  test pins the built-in default to that recorded value; if the official marketplace is not a git
  repository with `.claude-plugin/marketplace.json`, stop and report to the coordinator instead
  of guessing.
- Risks: `git clone` can take minutes; the RPC must not block other requests (run on a task, as
  `mods.inspect` does).
- Status: pending
- Outcome:
- Handoff notes: if over 800 lines, split protocol and config (2a) from the daemon (2b).

## Stage 3a-i: Record store and the load-time gate

- Objective: `<data>/plugin-installs.json` (records keyed by canonical folder path, as in
  [storage](#storage-layout-and-configuration)), the one-time migration, the digest cache, and the
  [load-time gate](#the-load-time-gate). In `crates/imperium-core/src/state/extensions.rs`,
  compute the `withheld` folders and the recorded `downloaded` folders and pass both in `Sources`,
  so `imperium_extensions::load_trusting` drops withheld folders by canonical path; hash every
  folder under `<config>/plugins` at daemon start and keep the cache key (sorted per-file relative
  path, size, mtime) in memory. Define the `ApprovalVerifier` trait whose production
  implementation (until stage 8a) counts nothing. No protocol method and no install path in this
  stage: tests write records directly. Fix the 06 Hooks line about the isolation tier and state
  the inherited environment ([assumptions](#assumptions)).
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/plugin_records.rs` (new), `state/extensions.rs`,
  `crates/imperium-extensions/src/lib.rs` (`Sources.withheld`), docs 06.
- Acceptance checks: daemon tests with records written directly:
  - every folder with hooks or servers and no counted, carried-over or (debug only) dev approval
    is withheld; removing it from `extensions.disabled_plugins` does not load it;
  - an unrecorded folder with a hook (copied in, or a recorded folder moved to a new name) is
    withheld with "not installed by Imperium: approve to load"; an unrecorded skills-only folder
    loads;
  - rewriting its `.imperium-source.json` (dropping the session or marketplace) does not change
    its gate or its `${VAR}` and rewrite restrictions;
  - renaming the manifest's `name`, or renaming the folder, withholds it;
  - adding a symbolic link (on Windows a junction, where the test can make one) withholds it;
  - renaming a file inside it (same count, same newest mtime) withholds it on the next session;
  - an edit to a skill file withholds the whole plugin; a new hook added to `hooks.json` with the
    hash recomputed into a forged record is still withheld because the run summary differs from
    the approval;
  - migration: on first start, folders not in `disabled_plugins` get a carried-over approval and
    load; folders in it are recorded without one; a second start does not re-migrate, and a
    folder added after it is unrecorded;
  - a `dev` approval is ignored when the test builds the release verifier;
  - an unreadable `plugin-installs.json` withholds every folder with code and the reason says so.
- Risks: hashing every plugin at daemon start (an estimate: the walk of 1,000 files is measured in
  a test, not benchmarked). The gate must cover every way a session loads extensions
  (`load_trusting` is the one path; check `grep -rn "load_with\|load_trusting\|fn load(" crates`).
  Migration runs once and could carry over an approval to a folder added between the upgrade and
  the first start of the new daemon (accepted: that window is the same as today's exposure).
- Status: pending
- Outcome:
- Handoff notes: the store mirrors `mod-approvals.json`'s handling (atomic writes, unreadable →
  fail closed).

## Stage 3a-ii: Inspect and install

- Objective: `plugins.inspect` and `plugins.install`. Inspect fetches the plugin (relative source:
  copy from the marketplace clone; external: `extension_install::fetch` with the entry's
  `ref`/`sha`) into a `Staging` folder, writes the entry's canonical JSON to
  `.claude-plugin/marketplace-entry.json`, parses it with stage 1, and hashes it (reuse
  `mod_install::tree_sha256`). Install **fetches again**, writes the entry file again, refuses a
  different hash, a taken name, a manifest name that differs from the entry name, or a link or
  reparse point in the source; moves it to `<config>/plugins/<name>`; writes the record with
  `origin = marketplace` and no approval; and journals a `config.changes` entry. A plugin with
  hooks or servers is withheld, and the result says to approve it in the desktop app.
- Depends on: stages 2 and 3a-i.
- Owned paths: `crates/imperium-core/src/plugin_install.rs` (new), `mod_install.rs` (only to
  share the hashing helper), `config_changes.rs` (the new change kind), `rpc.rs`, protocol
  `methods/plugins.rs`, version, snapshot, generated types, docs 02 and 06.
- Acceptance checks: daemon tests: inspect then install the fixture's hook plugin → installed,
  recorded, withheld; a source changed between inspect and install is refused; a taken name is
  refused; marker files absent after inspect and install; a `strict: false` fixture plugin
  installs, loads, and stays unmodified after its marketplace is refreshed; a skills-only plugin
  installs and loads; `plugins.install` has no approval field (a stray `approve` is ignored and
  noted).
- Risks: none beyond stage 3a-i's.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 3b: Uninstall, list and undo

- Objective: `plugins.uninstall` (to `config-trash`, record removed, optional secret deletion
  once stage 5 lands), `plugins.list` (every folder under `<config>/plugins`, recorded or not),
  `config.undo` of a plugin install and uninstall (restoring the record), `PluginInfo` gains `id`,
  `origin` and `withheld`; and the agent install path (`install_extension` for `kind = "plugin"`)
  writes a record with `origin = agent` and no approval.
- Depends on: stage 3a-ii.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `extension_install.rs` (record on
  agent install), `connectors.rs` (`PluginInfo` fields), `config_changes.rs`, `rpc.rs`, protocol
  `methods/plugins.rs`, `methods/connectors.rs`, version, snapshot, generated types, docs 02 and 06.
- Acceptance checks: tests: uninstall moves the folder and removes the record; `config.undo` of
  the uninstall restores both, and of an install removes both; `plugins.list` reports withheld
  plugins with the reason, including unrecorded ones; an agent install of a hook plugin is
  recorded and withheld, and removing it from `disabled_plugins` does not load it; `mcp.list`
  carries `withheld`.
- Risks: undo must not restore a record over a folder replaced since (refuse, as config undo
  already does for extensions).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 4: Enable, disable, update and approve

- Objective: `plugins.set_enabled` (user: edits `extensions.disabled_plugins` through the path
  `config.set` uses; environment: edits the environment record's `plugins` lists),
  `plugins.update`, and the effective-state order in [storage](#storage-layout-and-configuration).
  Enabling a plugin the gate withholds is refused with its reason. Update follows the
  [re-ask rules](#updates-re-ask), never approves, keeps the previous folder in `config-trash`, and
  reports "up to date" for an equal hash. The approval *write* path (record an approval of kind
  `counted` for a folder, its hash and run summary, creating an `origin = user` record for an
  unrecorded folder) is built here behind `ApprovalVerifier`; the `plugins.approve` method that
  reaches it lands in stage 8a.
- Depends on: stage 3b.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `plugin_records.rs`,
  `environments.rs` (record field), `state/extensions.rs` (environment lists), `rpc.rs`, protocol
  `methods/plugins.rs`, the file holding `EnvironmentInfo` (find with
  `grep -rn "pub struct EnvironmentInfo" crates/imperium-protocol`), version, snapshot, generated
  types, docs 02, 06, 14.
- Acceptance checks: tests: disable for the user → not loaded in a new session; enable only for
  environment A → loaded in A, not in B; a trusted repository file disabling it in A wins; an
  update that changes only a skill file of an approved hook plugin is withheld; an update that
  changes nothing is "up to date"; an update whose record names another marketplace is refused;
  enabling a withheld plugin is refused; with a test verifier that counts, an approval written
  through the write path lets the plugin load (and an unrecorded folder gets an `origin = user`
  record), and with the production verifier nothing is written; disabling then enabling an
  approved plugin needs no new approval.
- Risks: two places hold on/off (config and the environment record); `plugins.list` says which
  decided.
- Status: pending
- Outcome:
- Handoff notes: `extensions.disabled_plugins` stays the only user-level switch, so the Connectors
  window needs no change beyond showing `withheld`.

## Stage 5: Plugin secrets and child environments

- Objective: (a) required secrets from stage 1's `runs`, `plugins.set_secret` and
  `plugins.delete_secret`, the resolver change (a downloaded plugin's `${VAR}` reads
  `plugin.<plugin>.<var>` from the secret store, never the environment), missing secrets on
  `plugins.list`, servers with a missing secret not started (with a session notice), and
  `delete_secrets` on uninstall. (b) **Child environment allowlist:** hooks and stdio MCP servers
  of every plugin under `<config>/plugins` start with a cleared environment plus an allowlist
  (`PATH`, `HOME` or `USERPROFILE`, `SYSTEMROOT`, `TEMP`, `TMP`, `LANG`, `LC_*`, `TERM`, and on
  Windows `APPDATA`, `LOCALAPPDATA`, `PATHEXT`, `COMSPEC`), the spawned-by-daemon mark,
  `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PROJECT_DIR`, `IMPERIUM_PROJECT_DIR`, and the variables its server
  definition sets (with secrets resolved). User-configured hooks and servers keep today's
  environment.
- Depends on: stage 3b (stage 4 not required).
- Owned paths: `crates/imperium-extensions/src/plugins.rs` (resolver input), `hooks.rs` (the
  environment of a plugin's hook), the place `imperium-mcp` spawns a stdio server (find with
  `grep -rn "Command::new" crates/imperium-mcp/src`), `crates/imperium-core/src/plugin_install.rs`,
  `state/extensions.rs` (pass a secret lookup), `rpc.rs`, protocol `methods/plugins.rs`, version,
  snapshot, generated types, docs 02, 06, 10.
- Acceptance checks: tests with the in-memory secret store: a set secret expands into the
  server's env and nowhere else (assert the config file, plugin folder, `plugin-installs.json`
  and journal do not contain the value); a missing one leaves the server unstarted with a notice
  and the plugin's skills available; a 65-character secret name is refused; the value is never in
  a protocol result; a plugin's hook and stdio server, with `SOME_DAEMON_SECRET` set in the
  daemon's environment, do not see it and do see `PATH` and the mark; a user hook still sees it.
- Risks: the allowlist may break plugins that expect other variables (for example a proxy
  setting); the plugin's server definition can still set what it needs, and `plugins.list`
  explains the allowlist. Secret names are lowercase, so `FOO` and `foo` collide; refuse the second.
- Status: pending
- Outcome:
- Handoff notes: if over 800 lines, split (a) and (b).

## Stage 6: CLI and the safety floor

- Objective: `imperium plugin marketplace add|list|refresh|remove`, `imperium plugin browse
  [query] [--marketplace m]`, `imperium plugin inspect <name@mp>`, `imperium plugin install
  <name@mp> [--env <env>] [--sha256 h] [--yes]`, `update <name> [--sha256 h] [--yes]`,
  `enable|disable <name> [--env <env>]`, `uninstall <name> [--delete-secrets]`, `list [--env]`,
  `secret set|delete <name> <VAR>`. **No approving command** (U4). Install and update print the
  trust prompt and ask `Install <id>? [y/N]` (`--yes` without a terminal); for a plugin with code
  they then say it is withheld until approved in the desktop app's Plugins window. The floor: add
  `imperium plugin marketplace add`, `imperium plugin install|update|enable`, and
  `imperium config set|unset` of `extensions.disabled_plugins` and `extensions.plugin_dirs` to
  `crates/imperium-policy/src/floor.rs` beside `imperium mods install|update`; add
  `plugin-installs.json` and `marketplaces.json` to `FLOOR_PATHS`; protect `<config>/plugins` and
  `<data>/marketplaces` with `protect_dir` where the daemon protects its other folders (find with
  `grep -rn "protect_dir(" crates`).
- Depends on: stages 4 and 5.
- Owned paths: `crates/imperium-cli/src/plugins.rs` (new), `main.rs`, `crates/imperium-cli/snapshots/`,
  `crates/imperium-cli/tests/`, `crates/imperium-policy/src/floor.rs`, `permissions.rs`, the
  daemon line that calls `protect_dir`, docs 05 (floor list) and 06.
- Acceptance checks: CLI help snapshot updated and has no `approve`; CLI tests against a test
  daemon: add the fixture marketplace, browse, install with `--yes` and no terminal → installed,
  withheld, with the desktop note; `imperium plugin enable` of it is refused with the reason;
  `imperium plugin list` shows it; floor tests: `imperium plugin marketplace add x`,
  `imperium plugin install x@y --yes`, `imperium plugin enable x`,
  `imperium config set extensions.disabled_plugins []` and
  `imperium config set extensions.plugin_dirs '["/tmp/x"]'` all ask; file-tool writes to
  `plugin-installs.json` and into `<config>/plugins/x/` ask; reads do not.
- Risks: the floor reads command lines and is easy to sidestep; it is not the control.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 7: Repository suggestions

- Objective: U2. The environment layer accepts, from a folder in `mcp.trusted_projects` only,
  `[plugins] suggest_marketplaces = [{ source = "...", revision = "..." }]` and
  `suggest_plugins = ["name@marketplace", ...]` (at most 20 of each; each checked on its own, one
  bad entry drops only itself); everything else under `[plugins]` stays refused. They become
  offers on `plugins.suggestions {environment}`, with the file they came from and whether each is
  already added or installed. `plugins.dismiss_suggestion` dismisses one offer, or with
  `source_folder` mutes **every** suggestion from that project folder ("no suggestions from this
  project"), stored on the environment record. Nothing is added, fetched, installed or enabled by
  a suggestion; a suggested plugin from a marketplace not yet added is shown as needing that
  marketplace first. CLI: `imperium plugin suggestions --env <env>` prints the offers; it prints
  the commands that would act on them **only when standard output is a terminal**, never to a
  pipe or file, so an agent reading the output is not handed ready-to-run commands.
- Depends on: stages 4 and 6.
- Owned paths: `crates/imperium-config/src/environment.rs`, `crates/imperium-core/src/plugin_suggestions.rs`
  (new), `environments.rs` (dismissals), `rpc.rs`, protocol `methods/plugins.rs`, version,
  snapshot, generated types, `crates/imperium-cli/src/plugins.rs`, CLI snapshots, docs 02, 06, 14
  (the environment layer table).
- Acceptance checks: tests: an untrusted folder's suggestions are refused and listed in
  `imperium env config`; a trusted folder's are offered; offering touches no marketplace, record,
  config file or `disabled_plugins` (assert unchanged); a dismissed offer is not offered again; a
  muted project offers nothing, including entries added to its file later; the CLI with standard
  output redirected prints offers without commands; the marker-file plugin has not run.
- Risks: suggestion text is repository data; show sources and names verbatim, never as
  instructions to an agent (offers go only to the user's surfaces, not into any prompt).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 8a: Daemon verification of the desktop app

- Objective: the production `ApprovalVerifier` and the two methods. `plugins.approval_challenge`
  answers only a peer that passes check 2 of [who may approve](#who-may-approve) and issues a
  single-use challenge bound to the connection, folder, hash and run-summary digest, valid two
  minutes. `plugins.approve` counts only when checks 1 to 3 (and 4 once decided) hold, and writes
  through stage 4's write path; otherwise it records nothing and returns the reason. Platform
  checks: Windows (`QueryFullProcessImageNameW`, `WinVerifyTrust` and the pinned publisher,
  folder writability for the standard token, `CheckRemoteDebuggerPresent`); macOS
  (`SecCodeCopyGuestWithAttributes` with the audit token and the pinned designated requirement);
  Linux (`/proc/<pid>/exe`, root ownership and modes up the path, `TracerPid`, loader variables).
  The pinned publisher and team id come from build-time environment variables set only in the
  release workflow; a build without them counts nothing. The debug-build development verifier
  ([dev builds](#who-may-approve)) is compiled only under `cfg(debug_assertions)`.
- Depends on: stage 4.
- Owned paths: `crates/imperium-core/src/app_identity/` (new: `mod.rs`, `windows.rs`, `macos.rs`,
  `linux.rs`), `plugin_records.rs`, `rpc.rs`, `crates/imperium-core/Cargo.toml` (platform
  dependencies), protocol `methods/plugins.rs`, version, snapshot, generated types,
  `.github/workflows/build.yml` (the pinned identity variables only), docs 02, 06, 11.
- Acceptance checks: unit tests per platform in CI: the current test binary is refused (wrong
  path or unsigned); a TCP connection is refused before any process check; a challenge used twice,
  expired, from another connection, or for another hash or run summary is refused; a build without
  the pinned identity counts nothing; on Linux a path in a user-writable folder and a traced peer
  are refused; the dev verifier exists only in debug builds (a `cfg` test). By hand, on a signed
  macOS release candidate: the app's challenge is answered and another program's is not.
- Risks: the platform APIs are new to the codebase and each needs a CI runner of its OS; Windows
  cannot pass until releases are signed and installed per-machine (see
  [open question 2](#open-questions-for-the-user)). Split per platform if over 800 lines (8a-i
  shared code and macOS, 8a-ii Windows and Linux).
- Status: pending
- Outcome:
- Handoff notes: no change to `peer::may_grant` or to mod approvals.

## Stage 8b: Desktop approval command and native dialog

- Objective: a Tauri command `plugin_approve { folder or name, action: install | update | approve,
  environment? }` in `apps/desktop/src-tauri/src/`: it reads the preview (`plugins.inspect` or
  `plugins.list`), requests a challenge, shows a native dialog with `tauri-plugin-dialog` (plugin
  id, hook and server command lines, flagged launchers, hash), and on *Approve* sends
  `plugins.approve` with the challenge over its own `imperium-client` connection on the **local
  socket only**. The command is allowed only from the app's own windows (Tauri capability scoped to
  them). Release builds keep the hardened runtime and drop `get-task-allow` on macOS, and the
  Windows bundle is configured for a per-machine install once signing exists (U4's preconditions).
- Depends on: stage 8a.
- Owned paths: `apps/desktop/src-tauri/src/plugin_approval.rs` (new), `main.rs` (registration),
  `apps/desktop/src-tauri/capabilities/`, `apps/desktop/src-tauri/tauri.conf.json` (macOS
  entitlements, Windows install mode), doc 07.
- Acceptance checks: Rust unit tests in CI for the request it builds and for refusing a TCP-only
  endpoint; by hand on a signed macOS release candidate against the fixture marketplace: approving
  through the dialog records a counted approval (`plugins.list` shows it) and the plugin loads in a
  new session; a script asking for a challenge is refused; a debug app against a release daemon
  records nothing.
- Risks: Windows and Linux AppImage users cannot approve until their preconditions hold.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 9: Desktop Plugins window: marketplaces, browse and suggestions

- Objective: a Plugins window built on the Connectors pattern from #223: the shared `Dialog`,
  `Tabs` (*Installed*, *Browse*, *Marketplaces*), search, `EmptyState`, `LoadingState`, `Notice`,
  `.card.interactive` cards and `--row-h` rows. This stage ships *Browse* (cards with name,
  marketplace, description, category, component counts, *Installed* / *Update available* badges,
  and a detail view listing components), *Marketplaces* (the official one marked built-in; add by
  URL, folder or `owner/repo`, refresh, remove, last refresh and problems), and the environment's
  repository suggestions as offers with *Dismiss* and *No suggestions from this project*.
  Installing is stage 10, so this stage has no install button. Opened from the title bar, the
  command palette, Settings, and a *Manage plugins* link in Connectors' plugin section. **Why a
  separate window and not a Connectors tab:** Connectors is about MCP servers and already has its
  own *Installed* and *Browse*; a marketplace browser, a marketplace list and a trust flow inside it
  would give it two meanings of *Browse*. The window reuses Connectors' components and structure.
- Depends on: stages 2 and 7 (stage 3b for installed badges).
- Owned paths: `apps/desktop/src/plugins/` (new: `PluginsWindow.tsx`, `BrowseView.tsx`,
  `MarketplacesView.tsx`, `Suggestions.tsx`, `common.ts`), the place `ConnectorsWindow` is mounted
  and opened (find with `grep -rn "ConnectorsWindow" apps/desktop/src`),
  `apps/desktop/src/connectors/InstalledView.tsx` (the link only), the command palette entry,
  `apps/desktop/src/styles/` (only a new `plugins.css` if needed, tokens only),
  `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes (including the style guards); component
  tests with a mocked client for browse filtering, an external-source card showing "components
  after inspection", add, refresh and remove of a marketplace, dismissing an offer and muting a
  project; checked by hand at 1280, 1024 and 420 px, light and dark, and by keyboard, against the
  rules in [07](../07-desktop-ui.md#theming) and
  [accessibility](../07-desktop-ui.md#accessibility-implemented).
- Risks: the window must stay on the shared components.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 10: Desktop install, trust and update

- Objective: the trust view (everything in [the trust prompt](#the-trust-prompt-shows-exactly-what-runs),
  hooks and servers first, launchers flagged with the "not a guarantee" note, provenance claims
  labelled unverified, and the hash) for install, update and approve, whose *Approve* invokes stage
  8b's `plugin_approve`; *Install without approving*; the *Installed* tab with on/off for the user,
  a per-environment control, update, *Approve* for withheld plugins (including unrecorded and
  pre-existing ones), uninstall (with "also delete saved secrets"), and password fields for
  required secrets; Connectors' plugin rows show "needs approval" for withheld plugins. Where the
  platform cannot approve yet, *Approve* is replaced by the reason.
- Depends on: stages 5, 8b and 9.
- Owned paths: `apps/desktop/src/plugins/` (`InstalledView.tsx`, `TrustView.tsx`,
  `SecretsForm.tsx`), `apps/desktop/src/connectors/InstalledView.tsx` (the withheld label),
  `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests: the view lists every
  hook command and server command from a mocked `PluginPreview`; *Approve* invokes
  `plugin_approve` and the webview never calls `plugins.approve`; an update with changed hooks
  shows the diff; a secret field never re-displays a saved value; by hand on the signed macOS
  release candidate with the fixture marketplace: install, approve, enable, disable for one
  environment, update (withheld until approved again), uninstall, undo.
- Risks: none beyond stage 8's.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 11: Agents, docs and close-out

- Objective: `install_extension` with `kind = "plugin"` accepts `source = "name@marketplace"` for
  an added marketplace (recorded with `origin = agent` and withheld with hooks or servers), and its
  schema text says so; a final pass over 05, 06, 07, 10, 11 and 14 for the finished feature,
  including the [remaining gaps](#remaining-gaps); roadmap item 51 marked implemented; the
  official marketplace (U1) browsed live, one of its plugins inspected, installed and approved in
  the desktop on a platform that can approve, recorded in this plan's Outcome with the
  marketplace's commit.
- Depends on: stages 6 and 10.
- Owned paths: `crates/imperium-core/src/extension_install.rs` (source parsing),
  `crates/imperium-protocol/src/tools.rs`, `crates/imperium-protocol/schemas/tools/install_extension.json`,
  docs 05, 06, 07, 10, 11, 12, 14, and this plan.
- Acceptance checks: daemon test: an agent install of `x@fixture` with a hook is recorded,
  withheld and has no approval; `python3 scripts/ci/check-links.py .` passes; **U1 check:** the
  built-in default still matches the official marketplace's live source (the clone has
  `.claude-plugin/marketplace.json` and its `name` matches), recorded with its commit.
- Risks: a real marketplace may use source types this plan leaves out; record them as follow-ups.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Risks across stages

- **The approval is the main control on code that runs unconfined.** A user who approves a
  malicious plugin is compromised, as in Claude Code. The inspection makes that decision
  informed; it does not make it safe. Stage 5's environment allowlist limits what the code is
  handed, not what it can do.
- **The approval itself has [gaps](#remaining-gaps):** input automation can click the dialog
  unless an OS prompt is required; code injected into the genuine app on Windows, or a `ptrace`
  by the launching process on Linux, acts as the app; a user-writable install is not protected.
- **Platforms that cannot approve.** Windows releases until they are signed and installed
  per-machine, Linux outside root-owned packages, and machines without the desktop app: plugins
  with code stay withheld there, which users may read as broken. The UI and CLI say why.
- **Launchers and scripts outside the pin** (`npx …@latest`, a hook script that downloads code)
  make the hash cover only what is on disk; the prompt flags the common launchers and says the
  flag is not a guarantee.
- **`config.set extensions.plugin_dirs`** from any token holder adds an unrecorded, ungated
  folder of plugins; the floor asks only about an agent's shell doing it.
- **A same-size edit with a restored modification time** is caught only at daemon restart.
- **Format drift.** Claude Code's format changes; unknown fields are ignored and unknown source
  types listed, so drift degrades to "unsupported" rather than failures.
- **Protocol minors.** Several stages bump the minor; concurrent PRs on `experimental` must each
  take the next free minor at merge and update the version line in 02 by merging, not
  force-pushing.

## Open questions for the user

Each has a safe default that the stages follow until you decide.

1. **An OS user-presence prompt after the dialog** (option (c): Windows Hello consent, macOS
   Touch ID or password, Linux polkit). It is the only option that stops input automation from
   clicking the dialog. Default: **required where the platform offers it without setup**
   (macOS always; Windows when Hello is set up; Linux when a polkit agent and Imperium's action are
   installed), and approvals elsewhere count on the dialog alone, labelled as such in
   `plugins.list`. The stricter alternative is to require it everywhere, so those machines cannot
   approve.
2. **Windows until releases are signed.** Windows builds are not Authenticode-signed today and
   install per-user. Default: **no approval counts on Windows release builds** until both are
   fixed (carried-over approvals keep working), and Windows signing and a per-machine installer
   become prerequisites of stage 8a on Windows. The alternative, counting a path-only check of an
   unsigned per-user install, is not offered as a default: anything that can write that folder can
   replace the app and the daemon.
