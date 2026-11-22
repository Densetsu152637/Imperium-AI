# Plan: plugin marketplace

Revision: 2026-11-22, revised after the user's decisions and plan review round 1; written against
`experimental` at `3af43954` (last updated). Roadmap [item 51](../12-roadmap.md#next-in-order).
Status: planned; no stage started.

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
`plugin.json`. Tests never use the network.

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
   [3a](#stage-3a-install-records-inspect-install-and-the-load-time-gate),
   [3b](#stage-3b-uninstall-list-and-undo) and
   [4](#stage-4-enable-disable-update-and-approve).
4. **Three surfaces.** All of the above works from the protocol (`plugins.*`), the CLI
   (`imperium plugin …`) and a desktop Plugins window. Check: the protocol method snapshot
   lists the methods in [protocol additions](#protocol-additions); `imperium plugin --help`;
   the desktop checks of stages [9](#stage-9-desktop-plugins-window-marketplaces-browse-and-suggestions)
   and [10](#stage-10-desktop-install-trust-and-update).
5. **Nothing runs at add, refresh, browse, inspect or install.** Check: a fixture plugin whose
   hook and MCP server each write a marker file; after every one of those operations the marker
   is absent (stage 2 and 3a tests).
6. **Code needs a counted approval, bound to content, for new sessions.** For every plugin the
   daemon installs (marketplace and agent installs from stage 3b on), a new session loads its
   hooks or MCP servers only with a **counted** approval (made by the user's own process on the
   local socket, [who may approve](#who-may-approve)) of its exact content hash and of what it
   runs. A change of content (an update, an edit, a rename, a link, a rewritten source file, a new
   kind of component) withholds the whole plugin from new sessions until a fresh counted
   approval, whatever `extensions.disabled_plugins` says. Running sessions keep what they loaded.
   Plugins the user placed by hand (no install record) are out of this goal, as today. Check:
   stage 3a and 4 tests.
7. **Claude Code compatibility.** An unmodified Claude Code marketplace and its plugins load:
   relative, `github` and `url` plugin sources, `plugin.json` component paths and inline
   `mcpServers`, and marketplace entries with `strict: false`. Check: stage 1 parser tests,
   stage 3a's `strict: false` install test; by hand in
   [stage 11](#stage-11-agents-docs-and-close-out), the official marketplace.
8. **Secrets in the keychain.** A value a plugin's MCP server needs is stored in the secret
   store and never written to a config file, the plugin folder, the journal or a log. Check:
   stage 5 tests.

## User decisions

Made by the user on 2026-11-22; they replace the plan's earlier open questions.

- **U1. The official marketplace is pre-added.** Anthropic's official Claude Code plugin
  marketplace is a built-in entry of `plugins.marketplaces`. Its exact source is not recorded in
  this repository, so the plan names it **"the official marketplace source, confirmed at
  implementation"**; [stage 2](#stage-2-marketplaces-in-the-daemon) confirms it from Anthropic's
  Claude Code documentation and records the URL in this plan, and
  [stage 11](#stage-11-agents-docs-and-close-out) checks it against the live repository.
  Pre-adding it grants no trust: its plugins (some are third-party) need the same approval as any
  other.
- **U2. A trusted project may suggest marketplaces and plugins.** A project folder in
  `mcp.trusted_projects` may list, in its `.imperium/config.toml`, marketplaces and plugins to
  **offer** the user. Offers are shown only; they never add a marketplace, install, approve or
  enable anything. Accepting one runs the user's normal flow
  ([stage 7](#stage-7-repository-suggestions)).
- **U3. Plugin hooks block, never rewrite.** For every daemon-installed plugin, hooks may block
  but never rewrite a tool call or prompt, including after a terminal approval, for now.

## Scope and non-goals

In scope: the marketplace format and the plugin manifest as Claude Code publishes them (the
public format, from Imperium's existing parser and public documentation; no external site was
fetched for this plan), marketplace storage, inspection and approval, install and lifecycle,
plugin secrets and child environments, repository suggestions, CLI, protocol and desktop
surfaces (including a native approval path in the desktop app), and the agent tool
`install_extension` taking `name@marketplace`.

Non-goals (each with what would reopen it):

- **A hosted Imperium registry** or search across marketplaces the user has not added (the
  roadmap's "plugin registry" candidate). Reopen when there is a service to host it.
- **Automatic updates.** Refresh reports what changed; updating is always an explicit action.
  Reopen if the user asks for scheduled updates (they would still need a counted approval).
- **Signature verification** (see [provenance](#provenance-and-signatures-declined-for-now)).
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
  or enable a marketplace plugin; it may only suggest (U2). A trusted repository can still add
  `extensions.plugin_dirs` whose plugins load as user-placed plugins, as today
  ([14](../14-projects-and-automation.md#environment-configuration-implemented)); that path has no
  install record, so the marketplace gate is unaffected by it.
- **Applying plugin changes to running sessions or without a daemon restart.** Today's rule
  stands: new sessions see changes, and the Connectors notice about restarting stays where it
  applies.
- **Imperium-native `imperium-plugin.toml` manifests and plugin permission manifests**
  ([06](../06-extensibility.md#permissions-model-for-extensions)).
- **Hook rewrites for daemon-installed plugins** (U3). Reopen only by a user decision.
- **The trusted main-context mod loader.** This plan builds the desktop's local-socket approval
  path that roadmap [item 30](../12-roadmap.md#next-in-order) names as one of its mechanisms, and
  uses it for plugins; whether that path is strong enough for main-context mods stays item 30's
  open decision, blocked on the user, and is not resolved here.

## Assumptions

Checked against the source at `3af43954`:

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
  that can write there) control and which content hashing would exclude. This plan replaces it
  with a daemon-owned record ([stage 3a](#stage-3a-install-records-inspect-install-and-the-load-time-gate)).
- **Today's switch is uncounted.** The Connectors switch turns a plugin on by editing
  `extensions.disabled_plugins` through `config.set`, which any holder of the daemon token can
  call, including over loopback TCP.
- `imperium-core::mod_install` has the inspect-then-install pattern with a content hash
  (`tree_sha256`), a daemon-owned approval store (`mod-approvals.json`), a `modified` state, and
  the caller check `crate::peer::may_grant` (local socket, the user's own process, not a
  descendant of the daemon, not marked as spawned by it, not over SSH;
  [06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)). A loopback TCP
  connection never makes a grant.
- The desktop app's Rust side talks to the daemon with `imperium-client`, which tries the local
  socket first (`apps/desktop/src-tauri/src/daemon.rs`), and already depends on
  `tauri-plugin-dialog` for native dialogs.
- Plugin MCP servers have an empty `allow` list, so each of their tool calls asks unless a
  permission rule allows it.
- **Hooks and stdio MCP servers are not confined and inherit the daemon's whole environment.**
  [06 Hooks](../06-extensibility.md#hooks) says hooks run inside the session's isolation tier, but
  `hooks.rs` runs them with `imperium_tools::shell` (plain `cmd /C` or `sh -c`, adding the
  spawned-by-daemon mark), and local MCP servers are not confined either (listed as planned in
  [05](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented)'s
  section). Both get every variable of the daemon's environment; only `${VAR}` **expansion** in a
  downloaded plugin's `.mcp.json` is restricted today. Stage 3a fixes the 06 line; stage 5 adds an
  environment allowlist.
- `extensions.disabled_plugins` is read when the daemon starts.
- Environments are user-owned records in `<data>/environments.json`. A repository's
  `.imperium/config.toml` may only narrow, except that a folder in `mcp.trusted_projects` may add
  `extensions.plugin_dirs` and `[mcp.servers]` and disable plugins
  ([14](../14-projects-and-automation.md#environment-configuration-implemented)).
- Secret names are 1 to 64 characters of lowercase letters, digits, `.`, `_` and `-`
  ([10](../10-configuration.md#secrets)).
- The protocol is at 0.46 on `experimental`; #236 takes 0.47 if it merges first. Every stage
  that adds protocol takes **the next minor at its merge**.
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
- `apps/desktop/src-tauri/src/` (a new approval command), `apps/desktop/src/connectors/` (the
  pattern), new `apps/desktop/src/plugins/`

## Related docs

[06 Extensibility](../06-extensibility.md), [05 Tools and isolation](../05-tools-and-isolation.md),
[10 Configuration](../10-configuration.md), [14 Environments](../14-projects-and-automation.md#environment-configuration-implemented),
[07 Desktop](../07-desktop-ui.md#connectors-implemented), [02 Protocol](../02-architecture.md#protocol).
For behaviour already built, those docs win; for anything planned here, this plan wins until its
stage merges and updates them.

## Decision

**Chosen: a marketplace layer on the existing fetch, with a daemon-owned install record and a
counted approval gate.** Marketplaces are git clones cached in the data folder; installs reuse
`extension_install`'s fetch and land in `<config>/plugins/<name>`, where today's discovery finds
them; the daemon keeps, in its data folder, a record per installed folder (origin, source,
commit, content hash, and the counted approval), and the session loader withholds any recorded
folder that does not match its record and approval, by folder path, whatever the plugin's own
files or `extensions.disabled_plugins` say.

Rejected:

- **Only extending `install_extension` with more source forms.** No browsing, no catalogue
  refresh, no update path, and no approval: it fails goals 2, 3 and 6.
- **Driving Claude Code's own `claude plugin` CLI or sharing `~/.claude/plugins`.** Imperium must
  work without Claude Code installed, and Claude Code's trust decisions are not Imperium's. A
  user can still point `extensions.plugin_dirs` at Claude Code's plugin cache today, under the
  existing warning in [10](../10-configuration.md).
- **Allowing two installed plugins of the same name from different marketplaces** (Claude Code
  does). Skills, commands (`/<plugin>:<name>`), server names (`<plugin>_<server>`),
  `extensions.disabled_plugins` and agent profiles all key on the plugin name. An install whose
  name is taken is refused, and the folder name must equal the manifest name. Revisit if a real
  collision is reported.
- **Approving only the hook and server command lines.** Hook commands usually run scripts inside
  the plugin (`${CLAUDE_PLUGIN_ROOT}/scripts/check.py`), so a same-command update can change all
  the code that runs. The approval covers the content hash **and** the run summary.
- **Trusting `.imperium-source.json` or the manifest name for gating.** Both are plugin-controlled.
  The record is daemon-owned and keyed by folder.
- **Approvals from any token holder** (today's switch). An agent's shell that reads the endpoint
  token could approve its own download. Only `may_grant` callers on the local socket count.

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
- A plugin with hooks or MCP servers is installed **withheld** unless the install carries a
  counted approval ([who may approve](#who-may-approve)). Skills-and-commands-only plugins load
  once installed, as today, but are still bound to their recorded hash.

### The trust prompt shows exactly what runs

`plugins.inspect` returns, and every surface shows before installing, updating or approving:

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
  ([stage 3a](#stage-3a-install-records-inspect-install-and-the-load-time-gate)).
- **For an update:** a diff of hooks and servers (added, removed, changed lines) and a statement
  that other files changed, when they did.

### Pinning

- An install pins the plugin to the commit fetched (a marketplace entry's `sha` or `ref` is
  honoured; otherwise the default branch's head at inspect time). The marketplace clone is pinned
  to the commit of its last refresh. Neither moves without an explicit refresh or update.
- `--sha256` (CLI) and `sha256` (protocol) pin the content hash a previous inspection showed.

### Updates re-ask

- An update inspects the new version and compares its hash with the approved one. Any content
  change of a plugin with hooks or servers in either version needs a fresh counted approval;
  without one the new content installs **withheld**. A plugin with neither installs and loads.
- An update reads the source from the daemon's install record, never from the plugin folder, and
  refuses a different marketplace or source: to move, uninstall and install afresh.

### The load-time gate

At session start, for every folder under `<config>/plugins` that has a daemon install record, the
daemon withholds the **whole plugin** (skills and commands too) from the new session, whatever
`extensions.disabled_plugins` says, when any of these holds:

1. the folder name, the recorded name and the manifest name are not all equal;
2. the folder contains a symbolic link or a reparse point (junction) anywhere;
3. its content hash differs from the recorded one;
4. it has hooks or servers and there is no counted approval, or the approval's hash or run
   summary (`runs()`, [stage 1](#stage-1-marketplace-and-manifest-parsers)) differs from what is
   there now, so a new kind of component also needs a new approval.

Gating is by canonical folder path, not by name. A withheld plugin is reported with the reason
(`plugins.list`, `mcp.list`'s `withheld`). The hash is computed for every recorded folder when
the daemon starts and kept in memory with its cache key: the sorted list of every file's relative
path, size and modification time. A session start re-walks that list (metadata only) and rehashes
a folder whose list differs, so a rename, an added or removed file, or a changed size or time is
caught.

### Provenance and signatures (declined for now)

Claude Code marketplaces carry no signatures, and verifying git commit signatures needs an
allowed-signers list nobody publishes for marketplaces. This plan **declines signature
verification** and shows provenance instead (sources, commits, hash, unverified author claims).
Revisit when the Claude Code format adds signatures or the user wants `git verify-commit`
against a signers file they maintain.

### Who may approve

- **Only a counted approval turns on hooks or servers.** An approval counts when the call
  (`plugins.install` or `plugins.update` with `approve`, or `plugins.approve`) arrives on the local
  socket from a caller that passes `crate::peer::may_grant`. Anything else, including any call
  over loopback TCP, **records nothing**: the result says `approved: false` with the reason, and
  the plugin stays withheld.
- **CLI:** `imperium plugin install|update|approve` at the user's own terminal, which reaches the
  daemon over the local socket.
- **Desktop:** the webview never approves. The Plugins window asks the app's Rust side through a
  Tauri command (`plugin_approve`), which shows a **native, non-webview dialog**
  (`tauri-plugin-dialog`) with the plugin id, what it runs and the hash, and on *Approve* calls the
  daemon itself over the local socket with `imperium-client`; the app's process is a caller
  `may_grant` accepts (an ancestor or a stranger of the daemon, not a descendant). This is the
  mechanism roadmap [item 30](../12-roadmap.md#next-in-order) names; same-user input automation
  can still click a dialog on the user's desktop, which item 30 also records, and that open
  decision stays the user's. The Connectors switch keeps working for user-level on/off, but turning
  on a gated plugin with no counted approval shows "needs approval" and does nothing.
- **Agents:** `install_extension` (any source, including `name@marketplace`) installs exactly as
  today: withheld when the plugin has hooks or servers. The safety floor asks about an agent's
  shell running `imperium plugin install|update|enable|approve` and
  `imperium config set|unset extensions.disabled_plugins`, and about file-tool writes into
  `<config>/plugins`, `<data>/marketplaces` and `plugin-installs.json`
  ([stage 6](#stage-6-cli-and-the-safety-floor)). The floor is a guard against mistakes, not a
  boundary ([05](../05-tools-and-isolation.md#permissions)); the counted-approval rule is what
  stops an agent approving its own download.
- **Turning a plugin off and on again** does not revoke its approval; `plugins.set_enabled` may
  turn an approved plugin back on from any caller, since the user already approved that content.

### What an approved plugin still cannot do

- **Permission rules, the safety floor and agent profiles are unchanged.** A plugin carries no
  permission rules; its MCP tools (`mcp__<plugin>_<server>__<tool>`) ask unless a user rule allows
  them; floor-covered calls ask every time; an agent's `plugins` allowlist and local-files-only
  lock ([14](../14-projects-and-automation.md#agent-permission-profiles-implemented)) still keep a
  plugin's servers and hooks out of that agent's sessions.
- **Hooks block but never rewrite** (U3): `may_rewrite = false` for every daemon-installed plugin.
- **No daemon environment through `${VAR}`.** `${VAR}` in a daemon-installed plugin's `.mcp.json`
  expands only `CLAUDE_PLUGIN_ROOT` and the plugin's own secrets, never the daemon's environment,
  so a remote server cannot be handed a token the user keeps in an environment variable. From
  stage 5, the **processes** (hooks and stdio servers) get only an allowlisted environment too.
  Until then they inherit the daemon's whole environment, and the docs say so.
- **A repository cannot install or enable one.** A trusted project's `.imperium/config.toml` may
  suggest (U2) and may disable plugins; it cannot add marketplaces, install, approve or enable.

### Secrets

A server's `${VAR}` that is not `CLAUDE_PLUGIN_ROOT` is a **required secret** of the plugin. The
user sets it with `imperium plugin secret set <plugin> <VAR>` (value from standard input or a
prompt without echo), the desktop's password field, or `plugins.set_secret`; it is stored in the
secret store as `plugin.<plugin>.<var>` (lowercased; a name over 64 characters is refused with
the reason) and read only when that plugin's server starts. A plugin with a missing required
secret is listed as such and its server does not start; the rest of the plugin works.
Uninstalling offers to delete its secrets (CLI flag `--delete-secrets`, a checkbox in the
desktop), as `mcp.remove` does.

### Mod trust is separate

UI mods keep their own install, permissions and trust ([06](../06-extensibility.md#installing-and-updating-mods-implemented)).
A Claude Code plugin cannot contain a mod in this plan; a marketplace entry that is a mod
(`mod.json`) is listed as unsupported.

## Storage layout and configuration

| What | Where | Owner |
| --- | --- | --- |
| Marketplaces | built-in default: the official marketplace (U1); user-added: `[plugins.marketplaces.<name>]` in `<config>/config.toml` (`source`, optional `revision`, `enabled = false` to remove a built-in one), written with comments kept, as `[mcp.servers]` entries are | defaults, user config |
| Marketplace clone | `<data>/marketplaces/<name>/`, made on first browse or refresh (never at daemon start), replaced atomically through a staging folder | daemon cache, recreatable |
| Marketplace state | `<data>/marketplaces.json`: per marketplace the commit read, refresh time, and problems | daemon |
| Installed plugin | `<config>/plugins/<name>/`; folder name = manifest name; `.claude-plugin/marketplace-entry.json` for a marketplace install; `.imperium-source.json` kept for display only | as today |
| Install records and approvals | `<data>/plugin-installs.json`, keyed by canonical folder path: name, origin (`marketplace` or `agent`), id (`name@marketplace`), source and commit, content hash, and an optional counted approval (hash, run summary, time) | daemon |
| On/off for the user | `extensions.disabled_plugins` (unchanged key; it can only turn off, never past the gate) | user config |
| On/off per environment | `plugins: { enabled: [...], disabled: [...], dismissed_suggestions: [...] }` on the environment record in `<data>/environments.json` | user-owned |
| Suggestions | `[plugins] suggest_marketplaces` and `suggest_plugins` in a trusted project's `.imperium/config.toml` (U2) | repository, offers only |
| Secrets | secret store, `plugin.<plugin>.<var>` | keychain |
| Removed plugins | `<data>/config-trash/`, with a `config.changes` entry so `config.undo` restores it (with its record) | as today |

**Effective state for a new session**, in order: passes the [load-time gate](#the-load-time-gate)
(recorded folders only); not in `extensions.disabled_plugins`, unless the session's environment
record lists it in `enabled`; not in the environment record's `disabled`; not disabled by a
trusted repository's `.imperium/config.toml`; allowed by the agent profile.

**Configuration layers.** `plugins.marketplaces` is read from built-in defaults (the official
marketplace), the user config and environment variables (per [10](../10-configuration.md#layers);
stage 2 checks how that layer spells a table keyed by name and documents it); an environment
variable can add a marketplace to browse, never an install or approval. The environment layer
accepts only `[plugins] suggest_marketplaces` and `suggest_plugins`, and only from a trusted
folder; every other `[plugins]` key is refused, and `imperium env config` shows what it accepted
and refused.

## Protocol additions

A new `crates/imperium-protocol/src/methods/plugins.rs`, with TypeScript types generated for the
desktop. A paired phone cannot call `plugins.*` (as with `mods.*`). Each stage that adds methods
takes **the next protocol minor at its merge** and updates `PROTOCOL_VERSION`, the version line
in [02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method | Stage | Does |
| --- | --- | --- |
| `plugins.marketplace_add` | 2 | `{source, revision?}` → `MarketplaceInfo`. Clones, parses, records; refuses a name already added |
| `plugins.marketplace_list` | 2 | → `MarketplaceInfo[]` (name, source, built-in, commit, refreshed time, plugin count, problems) |
| `plugins.marketplace_refresh` | 2 | `{name?}` → per marketplace the commit before and after and plugins added, removed and changed |
| `plugins.marketplace_remove` | 2 | `{name}`; installed plugins stay installed, their updates report the marketplace missing |
| `plugins.browse` | 2 | `{marketplace?, query?}` → `PluginListing[]` (id, metadata, components or `null` for an external source, installed version and `update_available`) |
| `plugins.inspect` | 3a | `{id, revision?}` → `PluginPreview` ([trust prompt](#the-trust-prompt-shows-exactly-what-runs) contents, `sha256`, installed comparison) |
| `plugins.install` | 3a | `{id, sha256, approve, environment?}` → `{name, path, approved, withheld, notes}`; `approved` is true only when counted |
| `plugins.uninstall` | 3b | `{name, delete_secrets}` → the trash location and change id |
| `plugins.list` | 3b | `{environment?}` → installed plugins with id, origin, source, commit, `withheld` and its reason, effective on/off and what decided it, required and missing secrets |
| `plugins.update` | 4 | `{name, sha256, approve}` → result like install, with `replaced` |
| `plugins.approve` | 4 | `{name, sha256}`: approve the content installed now (a withheld marketplace or agent install); counted only, else `approved: false` |
| `plugins.set_enabled` | 4 | `{name, enabled, environment?}`; enabling a gated plugin without a counted approval is refused |
| `plugins.set_secret`, `plugins.delete_secret` | 5 | `{name, variable, value?}`; the value never returns |
| `plugins.suggestions` | 7 | `{environment}` → offers from the environment's trusted project files, with the file they came from and whether each is already added or installed; `plugins.dismiss_suggestion {environment, offer}` |
| `EnvironmentInfo.plugins` | 4 | the per-environment lists, also on `environments.update` |

`mcp.list`'s `PluginInfo` gains optional `id`, `origin` and `withheld` (stage 3b) so the
Connectors window keeps working and can say "needs approval".

## Stages

Ordered by dependency; each is one pull request of about 800 changed lines or fewer, tests
included. Every stage: branch from `experimental`, update the docs its behaviour changes, run
`cargo fmt --all --check` locally, and rely on CI for `bash scripts/check-rust.sh` (clippy,
tests, generated types and snapshots) and `bash scripts/check-web.sh`. A stage that grows past
the limit splits at the seam its handoff notes name.

## Stage 1: Marketplace and manifest parsers

- Objective: pure parsing in `imperium-extensions`, no daemon or protocol change.
  (a) `marketplace.rs`: parse `.claude-plugin/marketplace.json` (`name`, `owner`, `metadata`
  including `pluginRoot`, `plugins[]` with `name`, `source`, `description`, `version`, `author`,
  `homepage`, `repository`, `license`, `keywords`, `category`, `tags`, `strict`, and the
  component fields). Sources: a relative path (resolved under `pluginRoot`, refused if it leaves
  the repository), `{source: "github", repo, ref?, sha?}` (→ `https://github.com/<repo>.git`),
  `{source: "url", url, ref?, sha?}`; anything else is an `Unsupported(type)` entry, not an error.
  Unknown fields are ignored. Names must pass the existing `check_name` rules. A function writes
  an entry's **canonical JSON** (sorted keys, no insignificant whitespace) for stage 3a to store.
  (b) `plugins.rs`: read the full `plugin.json` (`version`, `author`, `homepage`, `repository`,
  `license`, `keywords`, and `commands`, `agents`, `hooks`, `mcpServers` as a path, a list of
  paths, or for `hooks` and `mcpServers` an inline object), honour those paths (inside the
  plugin root only; a path through a link is refused) in addition to the default folders. A
  folder is also a plugin when it has `.claude-plugin/marketplace-entry.json` and no
  `plugin.json`: the entry's fields are its manifest (`strict: false`); with both files,
  `plugin.json` wins and the entry's component fields add to it.
  (c) `fn runs(&Plugin) -> RunSummary`: each hook (event, matcher, expanded command or URL) and
  each server (command, args, URL, header names, `${VAR}` references), serialisable and
  comparable, with the fetches-code-at-start flag for the launchers listed in the security model.
  (d) Replace `installed_by_agent` (which reads the plugin-controlled `.imperium-source.json`) with
  a `downloaded` set of folders passed in `Sources` by the caller: a plugin in that set never
  expands daemon `${VAR}` and its hooks get `may_rewrite = false`. Until stage 3a fills the set,
  the caller keeps today's behaviour by passing the folders `installed_by_agent` would have
  named, so this stage changes no behaviour.
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
  clone and stage 3a a staging copy with the same functions.

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
  official marketplace as a built-in default** (U1), cloned lazily on first browse or refresh;
  removing it writes `enabled = false`. Make the environment layer refuse `[plugins]` (stage 7
  later accepts the two suggestion keys).
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
  the built-in official entry is listed without any network access at daemon start, and removing
  it writes `enabled = false`; an environment `.imperium/config.toml` with `[plugins]` is refused
  (`imperium-config` test); the methods snapshot lists the five methods; protocol minor bumped to
  the next at merge. **U1 check:** the implementer confirms the official marketplace's source from
  Anthropic's Claude Code documentation, records the URL and the documentation page in this
  plan's Outcome, and a unit test pins the built-in default to that recorded value; if the
  official marketplace is not a git repository with `.claude-plugin/marketplace.json`, stop and
  report to the coordinator instead of guessing.
- Risks: `git clone` can take minutes; the RPC must not block other requests (run on a task, as
  `mods.inspect` does).
- Status: pending
- Outcome:
- Handoff notes: if over 800 lines, split protocol and config (2a) from the daemon (2b).

## Stage 3a: Install records, inspect, install and the load-time gate

- Objective: `plugins.inspect` and `plugins.install`, the install record store, and the gate.
  Inspect fetches the plugin (relative source: copy from the marketplace clone; external:
  `extension_install::fetch` with the entry's `ref`/`sha`) into a `Staging` folder, writes the
  entry's canonical JSON to `.claude-plugin/marketplace-entry.json`, parses it with stage 1, and
  hashes it (reuse `mod_install::tree_sha256`). Install **fetches again**, writes the entry file
  again, refuses a different hash, a taken name, a manifest name that differs from the entry name,
  or a symbolic link or reparse point in the source; moves it to `<config>/plugins/<name>`; writes
  the record in `<data>/plugin-installs.json` keyed by canonical folder path; and records a
  counted approval only when `approve` is set **and** `peer::may_grant` passes on the local
  socket (otherwise `approved: false`, and with hooks or servers it is withheld). Journal a
  `config.changes` entry. The gate ([load-time gate](#the-load-time-gate)): in
  `crates/imperium-core/src/state/extensions.rs`, compute `withheld` folders and pass them, with
  the recorded `downloaded` folders, in `Sources` so `imperium_extensions::load_trusting` drops
  withheld folders by canonical path; keep the in-memory digest cache (sorted per-file path, size,
  mtime), filled for every recorded folder at daemon start. Fix the 06 Hooks line about the
  isolation tier and state the inherited environment ([assumptions](#assumptions)).
- Depends on: stage 2.
- Owned paths: `crates/imperium-core/src/plugin_install.rs` (new), `mod_install.rs` (only to
  share the hashing helper), `state/extensions.rs`, `config_changes.rs` (the new change kind),
  `rpc.rs`, `crates/imperium-extensions/src/lib.rs` (`Sources.withheld`), protocol
  `methods/plugins.rs`, version, snapshot, generated types, docs 02 and 06.
- Acceptance checks: daemon tests:
  - install with `approve` and the caller check passing, and failing (drive
    `peer::may_grant_from` with a fake daemon pid, as the `mod_install` tests do): passing → loaded
    with an approval record; failing → `approved: false` and withheld;
  - a call over loopback TCP with `approve` records nothing;
  - a source changed between inspect and install is refused; a taken name is refused;
  - marker files absent after inspect and install;
  - removing the plugin from `extensions.disabled_plugins` does not load an unapproved hook plugin;
  - rewriting its `.imperium-source.json` (dropping the session or marketplace) does not change
    its gate or its `${VAR}` and rewrite restrictions;
  - renaming the manifest's `name`, or renaming the folder, withholds it;
  - adding a symbolic link (on Windows a junction, where the test can make one) withholds it;
  - renaming a file inside it (same count, same newest mtime) withholds it on the next session;
  - an edit to a skill file withholds the whole plugin; a new hook added to `hooks.json` with the
    hash recomputed into a forged record entry is still withheld because the run summary differs
    from the approval;
  - a `strict: false` fixture plugin installs, loads, and stays unmodified after its marketplace
    is refreshed;
  - a skills-only plugin installs and loads without approval.
- Risks: hashing every recorded plugin at daemon start (an estimate: the digest walk of 1,000
  files is measured in a test, not benchmarked). The gate must run for every way a session loads
  extensions (`load_trusting` is the one path; check `grep -rn "load_with\|load_trusting\|fn load(" crates`).
- Status: pending
- Outcome:
- Handoff notes: an unreadable `plugin-installs.json` withholds every recorded folder it cannot
  confirm and the list says why (as an unreadable `mod-approvals.json` does for mods).

## Stage 3b: Uninstall, list and undo

- Objective: `plugins.uninstall` (to `config-trash`, record removed, optional secret deletion
  once stage 5 lands), `plugins.list`, `config.undo` of a plugin install and uninstall (restoring
  the record), `PluginInfo` gains `id`, `origin` and `withheld`; and the agent install path
  (`install_extension` for `kind = "plugin"`) writes an install record with `origin = agent`, so
  agent installs fall under the gate from this stage on. Plugins agents installed before this
  stage have no record and stay as they are (documented).
- Depends on: stage 3a.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `extension_install.rs` (record on
  agent install), `connectors.rs` (`PluginInfo` fields), `config_changes.rs`, `rpc.rs`, protocol
  `methods/plugins.rs`, `methods/connectors.rs`, version, snapshot, generated types, docs 02 and 06.
- Acceptance checks: tests: uninstall moves the folder and removes the record; `config.undo` of
  the uninstall restores both, and of an install removes both; `plugins.list` reports withheld
  plugins with the reason; an agent install of a hook plugin is recorded and withheld, and removing
  it from `disabled_plugins` does not load it; `mcp.list` carries `withheld`.
- Risks: undo must not restore a record over a folder replaced since (refuse, as config undo
  already does for extensions).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 4: Enable, disable, update and approve

- Objective: `plugins.set_enabled` (user: edits `extensions.disabled_plugins` through the path
  `config.set` uses; environment: edits the environment record's `plugins` lists),
  `plugins.update` and `plugins.approve` (counted only), and the effective-state order in
  [storage](#storage-layout-and-configuration). Enabling a gated plugin without a counted approval
  is refused with "approve first". Update follows the [re-ask rules](#updates-re-ask), keeps the
  previous folder in `config-trash`, and reports "up to date" for an equal hash.
- Depends on: stage 3b.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `environments.rs` (record field),
  `state/extensions.rs` (environment lists), `rpc.rs`, protocol `methods/plugins.rs`, the file
  holding `EnvironmentInfo` (find with `grep -rn "pub struct EnvironmentInfo" crates/imperium-protocol`),
  version, snapshot, generated types, docs 02, 06, 14.
- Acceptance checks: tests: disable for the user → not loaded in a new session; enable only for
  environment A → loaded in A, not in B; a trusted repository file disabling it in A wins; an
  update that changes only a skill file of a hook plugin is withheld without a counted approval;
  an update that changes nothing is "up to date"; an update whose record names another marketplace
  is refused; enabling an unapproved hook plugin is refused; `plugins.approve` over TCP records
  nothing, over the local socket with the check passing records the approval and the plugin loads;
  disabling then enabling an approved plugin needs no new approval.
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
  of a downloaded plugin start with a cleared environment plus an allowlist (`PATH`, `HOME` or
  `USERPROFILE`, `SYSTEMROOT`, `TEMP`, `TMP`, `LANG`, `LC_*`, `TERM`, and on Windows `APPDATA`,
  `LOCALAPPDATA`, `PATHEXT`, `COMSPEC`), the spawned-by-daemon mark, `CLAUDE_PLUGIN_ROOT`,
  `CLAUDE_PROJECT_DIR`, `IMPERIUM_PROJECT_DIR`, and the variables its server definition sets
  (with secrets resolved). User-configured hooks and servers keep today's environment.
- Depends on: stage 3b (stage 4 not required).
- Owned paths: `crates/imperium-extensions/src/plugins.rs` (resolver input), `hooks.rs` (the
  environment of a downloaded plugin's hook), the place `imperium-mcp` spawns a stdio server
  (find with `grep -rn "Command::new" crates/imperium-mcp/src`), `crates/imperium-core/src/plugin_install.rs`,
  `state/extensions.rs` (pass a secret lookup), `rpc.rs`, protocol `methods/plugins.rs`, version,
  snapshot, generated types, docs 02, 06, 10.
- Acceptance checks: tests with the in-memory secret store: a set secret expands into the
  server's env and nowhere else (assert the config file, plugin folder, `plugin-installs.json`
  and journal do not contain the value); a missing one leaves the server unstarted with a notice
  and the plugin's skills available; a 65-character secret name is refused; the value is never in
  a protocol result; a downloaded plugin's hook and stdio server, with `SOME_DAEMON_SECRET` set in
  the daemon's environment, do not see it and do see `PATH` and the mark; a user hook still sees
  it.
- Risks: the allowlist may break plugins that expect other variables (for example a proxy
  setting); the plugin's server definition can still set what it needs, and `plugins.list`
  explains the allowlist. Secret names are lowercase, so `FOO` and `foo` collide; refuse the second.
- Status: pending
- Outcome:
- Handoff notes: if over 800 lines, split (a) and (b).

## Stage 6: CLI and the safety floor

- Objective: `imperium plugin marketplace add|list|refresh|remove`, `imperium plugin browse
  [query] [--marketplace m]`, `imperium plugin inspect <name@mp>`, `imperium plugin install
  <name@mp> [--env <env>] [--sha256 h] [--yes]`, `update <name> [--yes]`, `approve <name>`,
  `enable|disable <name> [--env <env>]`, `uninstall <name> [--delete-secrets]`, `list [--env]`,
  `secret set|delete <name> <VAR>`. Install, update and approve print the trust prompt and ask
  `Approve <id>? [y/N]`; without a terminal they refuse unless `--yes`, and then install without
  approving. The CLI reports `approved` as the daemon counted it (follow
  `crates/imperium-cli/src/mods.rs`). The floor: add those subcommands (install, update, enable,
  approve) and `imperium config set|unset extensions.disabled_plugins` to
  `crates/imperium-policy/src/floor.rs` beside `imperium mods install|update`; add
  `plugin-installs.json` and `marketplaces.json` to `FLOOR_PATHS`; protect `<config>/plugins` and
  `<data>/marketplaces` with `protect_dir` where the daemon protects its other folders (find with
  `grep -rn "protect_dir(" crates`).
- Depends on: stages 4 and 5.
- Owned paths: `crates/imperium-cli/src/plugins.rs` (new), `main.rs`, `crates/imperium-cli/snapshots/`,
  `crates/imperium-cli/tests/`, `crates/imperium-policy/src/floor.rs`, `permissions.rs`, the
  daemon line that calls `protect_dir`, docs 05 (floor list) and 06.
- Acceptance checks: CLI help snapshot updated; CLI tests against a test daemon: add the fixture
  marketplace, browse, install with `--yes` and no terminal → installed, `approved: false`,
  withheld with the note; `imperium plugin list` shows it; floor tests:
  `imperium plugin install x@y --yes`, `imperium plugin approve x`, `imperium plugin enable x`,
  `imperium config set extensions.disabled_plugins []` all ask; file-tool writes to
  `plugin-installs.json` and into `<config>/plugins/x/` ask; reads do not.
- Risks: the floor reads command lines and is easy to sidestep; the counted-approval rule, not the
  floor, is the control.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 7: Repository suggestions

- Objective: U2. The environment layer accepts, from a folder in `mcp.trusted_projects` only,
  `[plugins] suggest_marketplaces = [{ source = "...", revision = "..." }]` and
  `suggest_plugins = ["name@marketplace", ...]` (at most 20 of each; each checked on its own, one
  bad entry drops only itself); everything else under `[plugins]` stays refused. They become
  offers on `plugins.suggestions {environment}`, with the file they came from and whether each is
  already added or installed; `plugins.dismiss_suggestion` stores a dismissal on the environment
  record. Nothing is added, fetched, installed or enabled by a suggestion; a suggested plugin from
  a marketplace not yet added is shown as needing that marketplace first. CLI:
  `imperium plugin suggestions --env <env>` prints the offers and the commands that would act on
  them.
- Depends on: stages 4 and 6.
- Owned paths: `crates/imperium-config/src/environment.rs`, `crates/imperium-core/src/plugin_install.rs`
  (or a new `plugin_suggestions.rs`), `environments.rs` (dismissals), `rpc.rs`, protocol
  `methods/plugins.rs`, version, snapshot, generated types, `crates/imperium-cli/src/plugins.rs`,
  CLI snapshots, docs 02, 06, 14 (the environment layer table).
- Acceptance checks: tests: an untrusted folder's suggestions are refused and listed in
  `imperium env config`; a trusted folder's are offered; offering touches no marketplace,
  install record, config file or `disabled_plugins` (assert unchanged); a dismissed offer is not
  offered again; the marker-file plugin has not run.
- Risks: suggestion text is repository data; show sources and names verbatim, never as
  instructions to an agent (offers go only to the user's surfaces, not into any prompt).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 8: Desktop approval over the local socket

- Objective: the desktop's counted approval path ([who may approve](#who-may-approve)). A Tauri
  command `plugin_approve { name or id, sha256, action: install | update | approve, environment? }`
  in `apps/desktop/src-tauri/src/`: it asks the daemon for the preview (`plugins.inspect` or
  `plugins.list`), shows a native dialog with `tauri-plugin-dialog` (plugin id, hook and server
  command lines, flagged launchers, hash), and on *Approve* opens its own `imperium-client`
  connection over the **local socket only** (refusing loopback TCP) and sends the call with
  `approve`. It returns the daemon's result; the webview never sends `approve` itself. The
  command is allowed only from the app's own windows (Tauri capability scoped to them).
- Depends on: stage 4.
- Owned paths: `apps/desktop/src-tauri/src/` (a new `plugin_approval.rs`, registration in
  `main.rs`), `apps/desktop/src-tauri/capabilities/` (the command's permission), `apps/desktop/src-tauri/Cargo.toml`
  only if a feature flag is needed, doc 07.
- Acceptance checks: Rust unit tests in CI for the request it builds and for refusing a TCP-only
  endpoint; by hand against a dev daemon with the fixture marketplace: approving through the
  command records a counted approval (`plugins.list` shows it), and the same `plugins.install`
  call sent from the webview's TCP connection records nothing.
- Risks: on Windows the named-pipe peer check must see the app's process, not a helper; test both
  a packaged and a dev build. Same-user input automation can click the dialog (item 30's known
  limit).
- Status: pending
- Outcome:
- Handoff notes: no daemon change; if the daemon cannot see the app as a valid peer, report it
  rather than loosening `may_grant`.

## Stage 9: Desktop Plugins window: marketplaces, browse and suggestions

- Objective: a Plugins window built on the Connectors pattern from #223: the shared `Dialog`,
  `Tabs` (*Installed*, *Browse*, *Marketplaces*), search, `EmptyState`, `LoadingState`, `Notice`,
  `.card.interactive` cards and `--row-h` rows. This stage ships *Browse* (cards with name,
  marketplace, description, category, component counts, *Installed* / *Update available* badges,
  and a detail view listing components), *Marketplaces* (the official one marked built-in; add by
  URL, folder or `owner/repo`, refresh, remove, last refresh and problems), and the environment's
  repository suggestions as dismissible offers. Installing is stage 10, so this stage has no
  install button. Opened from the title bar, the command palette, Settings, and a *Manage
  plugins* link in Connectors' plugin section. **Why a separate window and not a Connectors tab:**
  Connectors is about MCP servers and already has its own *Installed* and *Browse*; a marketplace
  browser, a marketplace list and a trust flow inside it would give it two meanings of *Browse*.
  The window reuses Connectors' components and structure instead.
- Depends on: stages 2 and 7 (stage 3b for installed badges).
- Owned paths: `apps/desktop/src/plugins/` (new: `PluginsWindow.tsx`, `BrowseView.tsx`,
  `MarketplacesView.tsx`, `Suggestions.tsx`, `common.ts`), the place `ConnectorsWindow` is mounted
  and opened (find with `grep -rn "ConnectorsWindow" apps/desktop/src`),
  `apps/desktop/src/connectors/InstalledView.tsx` (the link only), the command palette entry,
  `apps/desktop/src/styles/` (only a new `plugins.css` if needed, tokens only),
  `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes (including the style guards); component
  tests with a mocked client for browse filtering, an external-source card showing "components
  after inspection", add, refresh and remove of a marketplace, and dismissing an offer; checked by
  hand at 1280, 1024 and 420 px, light and dark, and by keyboard, against the rules in
  [07](../07-desktop-ui.md#theming) and [accessibility](../07-desktop-ui.md#accessibility-implemented).
- Risks: the window must stay on the shared components.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 10: Desktop install, trust and update

- Objective: the trust view (everything in [the trust prompt](#the-trust-prompt-shows-exactly-what-runs),
  hooks and servers first, launchers flagged with the "not a guarantee" note, provenance claims
  labelled unverified, and the hash) for install, update and approve, whose *Approve* calls stage
  8's `plugin_approve` command; *Install without approving* for a withheld install; the
  *Installed* tab with on/off for the user, a per-environment control, update, approve for withheld
  plugins, uninstall (with "also delete saved secrets"), and password fields for required
  secrets; Connectors' plugin rows show "needs approval" for withheld plugins.
- Depends on: stages 5, 8 and 9.
- Owned paths: `apps/desktop/src/plugins/` (`InstalledView.tsx`, `TrustView.tsx`,
  `SecretsForm.tsx`), `apps/desktop/src/connectors/InstalledView.tsx` (the withheld label),
  `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests: the view lists every
  hook command and server command from a mocked `PluginPreview`; *Approve* invokes `plugin_approve`
  with the shown `sha256` and never sends `approve` over the daemon client; an update with changed
  hooks shows the diff; a secret field never re-displays a saved value; by hand against a dev
  daemon with the fixture marketplace: install with approval, enable, disable for one environment,
  update (asks again), uninstall, undo.
- Risks: none beyond stage 8's.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 11: Agents, docs and close-out

- Objective: `install_extension` with `kind = "plugin"` accepts `source = "name@marketplace"` for
  an added marketplace (recorded with `origin = agent` and withheld with hooks or servers, as other
  agent installs), and its schema text says so; a final pass over 05, 06, 07, 10 and 14 for the
  finished feature; roadmap item 51 marked implemented; the official marketplace (U1) browsed
  live, one of its plugins inspected and installed with a counted approval, recorded in this plan's
  Outcome with the marketplace's commit.
- Depends on: stages 6 and 10.
- Owned paths: `crates/imperium-core/src/extension_install.rs` (source parsing),
  `crates/imperium-protocol/src/tools.rs`, `crates/imperium-protocol/schemas/tools/install_extension.json`,
  docs 05, 06, 07, 10, 12, 14, and this plan.
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
  informed; it does not make it safe. Confinement is a non-goal here; stage 5's environment
  allowlist limits what the code is handed, not what it can do.
- **Launchers and scripts outside the pin** (`npx …@latest`, a hook script that downloads code)
  make the hash cover only what is on disk; the prompt flags the common launchers and says the
  flag is not a guarantee.
- **Desktop approvals rest on the app's process passing `may_grant` and a native dialog**, which
  same-user input automation can click (item 30's recorded limit).
- **Hand-placed plugins and pre-existing agent installs are not gated.** Anything that can write
  into `<config>/plugins` can still place an unrecorded plugin; the floor asks about agent writes
  there, and this is today's exposure, unchanged.
- **Format drift.** Claude Code's format changes; unknown fields are ignored and unknown source
  types listed, so drift degrades to "unsupported" rather than failures.
- **Protocol minors.** Several stages bump the minor; concurrent PRs on `experimental` must each
  take the next free minor at merge and update the version line in 02 by merging, not
  force-pushing.

## Open questions for the user

None. The user's decisions are recorded as [U1 to U3](#user-decisions); item 30's open decision
on main-context mods is referenced, not reopened.
