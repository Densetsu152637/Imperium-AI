# Plan: plugin marketplace

Revision: 2026-11-22, written against `experimental` at `f1e096c8` (last updated). Roadmap
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
path (commands, an agent, a skill, a `tool.before` hook and a stdio MCP server) and one plugin by
a `url` source pointing at a second temporary repository. Tests never use the network.

1. **Marketplaces.** A marketplace can be added from a git URL, a local folder, or a GitHub
   `owner/repo`; listed; refreshed to its source's newest commit; and removed. Check: the
   daemon tests of [stage 2](#stage-2-marketplaces-in-the-daemon) and, by hand,
   `imperium plugin marketplace add <fixture>` / `list` / `refresh` / `remove`.
2. **Browse.** Every plugin of every added marketplace is listed with its name, description,
   version, author, category and keywords, and the components it contains (commands, agents,
   skills, hook events, MCP servers), or a statement that the components are known only after
   inspecting an external source. Check: `plugins.browse` test over the fixture;
   `imperium plugin browse`.
3. **Lifecycle, per user and per environment.** A plugin is installed as `name@marketplace`,
   enabled or disabled for the user or for one environment, updated, and uninstalled (to
   `<data>/config-trash/`, undoable). Check: the tests of stages
   [3](#stage-3-inspect-approve-install-and-uninstall) and
   [4](#stage-4-enable-disable-and-update-per-user-and-per-environment).
4. **Three surfaces.** All of the above works from the protocol (`plugins.*`), the CLI
   (`imperium plugin …`) and a desktop Plugins window. Check: the protocol method snapshot
   lists the methods in [protocol additions](#protocol-additions); `imperium plugin --help`;
   the desktop checks of stages [7](#stage-7-desktop-plugins-window-marketplaces-and-browse) and
   [8](#stage-8-desktop-install-trust-and-update).
5. **Nothing runs at add, refresh, browse, inspect or install.** Check: a fixture plugin whose
   hook and MCP server each write a marker file; after every one of those operations the marker
   is absent (stage 2 and 3 tests).
6. **Code needs the user's approval, bound to content.** A plugin with hooks or MCP servers is
   enabled only with an approval recorded for its exact content hash; any change of content
   (an update, or files edited in place) needs a fresh approval before its hooks or servers
   run again. Check: stage 3 and 4 tests (edit a file of an approved plugin: it is reported
   `modified` and its hooks and servers do not load).
7. **Claude Code compatibility.** An unmodified Claude Code marketplace and its plugins load:
   relative, `github` and `url` plugin sources, `plugin.json` component paths and inline
   `mcpServers`, and marketplace entries with `strict: false`. Check: stage 1 parser tests;
   by hand in [stage 9](#stage-9-agents-docs-and-close-out), one public marketplace the user
   names.
8. **Secrets in the keychain.** A value a plugin's MCP server needs is stored in the secret
   store and never written to a config file, the plugin folder, the journal or a log. Check:
   stage 5 tests.

## Scope and non-goals

In scope: the marketplace format and the plugin manifest as Claude Code publishes them (the
public format, from Imperium's existing parser and public documentation; no external site was
fetched for this plan), marketplace storage, inspection and approval, install and lifecycle,
plugin secrets, CLI, protocol and desktop surfaces, and the agent tool `install_extension`
taking `name@marketplace`.

Non-goals (each with what would reopen it):

- **A hosted Imperium registry** or search across marketplaces the user has not added (the
  roadmap's "plugin registry" candidate). Reopen when there is a service to host it.
- **Automatic updates.** Refresh reports what changed; updating is always an explicit action.
  Reopen if the user asks for scheduled updates (they would still re-ask per goal 6).
- **Signature verification** (see [provenance](#provenance-and-signatures-declined-for-now)).
- **A marketplace served as a bare `marketplace.json` URL**, and **npm or pip plugin sources**.
  They are reported as unsupported with the source type named. Reopen when a marketplace the user
  wants is published only that way.
- **Sandboxing plugin hooks and MCP servers.** They run as today: as the user, unconfined
  ([assumptions](#assumptions)). The approval is the control. Reopen with roadmap work on
  running local servers and hooks inside the isolation tier ([05](../05-tools-and-isolation.md#mcp)).
- **Claude Code command features that run code**: `!` shell lines in command templates and
  `allowed-tools` front matter. Imperium's commands stay text; the inspection lists such commands
  with a warning.
- **Repository-scoped installs** (Claude Code's project scope writes enablement into the
  repository). A repository file can only narrow ([10](../10-configuration.md#layers)); see
  [open question 2](#open-questions-for-the-user).
- **Applying plugin changes to running sessions or without a daemon restart.** Today's rule
  stands: new sessions see changes, and the Connectors notice about restarting stays where it
  applies.
- **Imperium-native `imperium-plugin.toml` manifests and plugin permission manifests**
  ([06](../06-extensibility.md#permissions-model-for-extensions)).
- **The trusted main-context mod loader** and its stronger caller check. This plan references
  that open item and does not resolve it ([security model](#who-may-approve)).

## Assumptions

Checked against the source at `f1e096c8`:

- `imperium-extensions::plugins` loads a plugin from a fixed layout (`skills/`, `commands/`,
  `agents/`, `hooks/hooks.json`, `.mcp.json`) and reads only `name` and `description` from
  `plugin.json`; custom component paths and inline `mcpServers` are ignored today.
- `imperium-core::extension_install` already does a safe fetch: shallow `git clone` without
  submodules, LFS, prompts or the `ext::` transport, a 5-minute limit, a 256 MiB copy without
  `.git` or symbolic links, an unpredictable staging folder, and `.imperium-source.json`
  provenance. It refuses to install over an existing name. A plugin with hooks or servers lands
  in `extensions.disabled_plugins`.
- `imperium-core::mod_install` has the inspect-then-install pattern with a content hash
  (`tree_sha256`), an approval store in the data folder (`mod-approvals.json`), a
  `modified` state, and the caller check `crate::peer::may_grant` for approvals claimed at a
  terminal ([06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)).
- A plugin installed by an agent (its source file names a session) never expands `${VAR}` from
  the daemon's environment in `.mcp.json` and its hooks may block but not rewrite.
- Plugin MCP servers have an empty `allow` list, so each of their tool calls asks unless a
  permission rule allows it.
- **Hooks are not run inside the isolation tier.** [06 Hooks](../06-extensibility.md#hooks) says
  they are, but `hooks.rs` runs them with `imperium_tools::shell` (plain `cmd /C` or `sh -c`
  with the spawned-by-daemon mark). Local MCP servers are not confined either (listed as planned
  in [05](../05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented)'s
  section). The security model below is written for that reality; fixing the doc line is left to
  the stage that next edits 06 (stage 3).
- `extensions.disabled_plugins` is read when the daemon starts; the Connectors window already
  toggles it through `config.set`.
- Environments are user-owned records in `<data>/environments.json`; a repository's
  `.imperium/config.toml` may only narrow, and may disable plugins only for a folder in
  `mcp.trusted_projects` ([14](../14-projects-and-automation.md#environment-configuration-implemented)).
- Secret names are 1 to 64 characters of lowercase letters, digits, `.`, `_` and `-`
  ([10](../10-configuration.md#secrets)).
- The protocol is at 0.46 on `experimental`; #236 takes 0.47 if it merges first. Every stage
  that adds protocol takes **the next minor at its merge**.
- Rust is built and tested in CI only (`bash scripts/check-rust.sh`); locally run only
  `cargo fmt --all --check`. Web checks: `bash scripts/check-web.sh` after `pnpm install`.

## Relevant paths

- `crates/imperium-extensions/src/plugins.rs`, `lib.rs` (plugin discovery and session loading)
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
- `apps/desktop/src/connectors/` (the pattern), new `apps/desktop/src/plugins/`

## Related docs

[06 Extensibility](../06-extensibility.md), [05 Tools and isolation](../05-tools-and-isolation.md),
[10 Configuration](../10-configuration.md), [07 Desktop](../07-desktop-ui.md#connectors-implemented),
[02 Protocol](../02-architecture.md#protocol). For behaviour already built, those docs win; for
anything planned here, this plan wins until its stage merges and updates them.

## Decision

**Chosen: a marketplace layer on the existing fetch, with the mod install's inspect-and-approve
model applied to plugins.** Marketplaces are git clones cached in the data folder; installs
reuse `extension_install::fetch` and land in `<config>/plugins/<name>`, where today's discovery
already finds them; approvals live in a daemon-owned store keyed by plugin name and bound to a
content hash, as `mod-approvals.json` is for mods.

Rejected:

- **Only extending `install_extension` with more source forms.** No browsing, no catalogue
  refresh, no update path, and no approval: it fails goals 2, 3 and 6.
- **Driving Claude Code's own `claude plugin` CLI or sharing `~/.claude/plugins`.** Imperium must
  work without Claude Code installed, and Claude Code's trust decisions are not Imperium's. A
  user can still point `extensions.plugin_dirs` at Claude Code's plugin cache today, under the
  existing warning in [10](../10-configuration.md).
- **Allowing two installed plugins of the same name from different marketplaces** (Claude Code
  does). Skills, commands (`/<plugin>:<name>`), server names (`<plugin>_<server>`),
  `extensions.disabled_plugins` and agent profiles all key on the plugin name; namespacing them
  all is a wide change for a rare case. An install whose name is taken is refused, as
  `install_extension` already refuses. Revisit if a real collision is reported.
- **Approving only the hook and server command lines** (so an update that keeps them needs no
  new approval). Hook commands usually run scripts inside the plugin
  (`${CLAUDE_PLUGIN_ROOT}/scripts/check.py`), so a same-command update can change all the code
  that runs. Any content change of a plugin with hooks or servers re-asks.

## Security model

Installing a plugin is installing code from the internet: once enabled, its hooks run on every
matching event and its stdio MCP servers run as processes of the user, neither inside an
isolation tier ([assumptions](#assumptions)). Its skills and commands are text that steers
agents. The controls below are what stands between a marketplace and that code.

### Nothing runs before approval

- Adding or refreshing a marketplace clones it (the existing safe `git` invocation) and parses
  JSON. Browsing reads the cached clone. Inspecting fetches the plugin to a staging folder and
  parses it. Installing moves the inspected copy into place. None of these runs a hook, a
  server, a build step, an install script, or a git hook.
- A plugin with hooks or MCP servers is installed **off** unless the install carries an
  approval the daemon counts ([who may approve](#who-may-approve)). Skills-and-commands-only
  plugins install on, as today.

### The trust prompt shows exactly what runs

`plugins.inspect` returns, and every surface shows before installing or updating:

- **Identity and provenance:** `name@marketplace`, version, the marketplace's source and the
  commit it was read at, the plugin's own source (same repository as the marketplace, or a
  third-party repository) and the exact commit fetched, and author, homepage and licence
  **labelled as the plugin's own unverified claims**.
- **Every hook:** event, matcher, and the full command line or URL, with `${CLAUDE_PLUGIN_ROOT}`
  expanded, and whether it can block (all can) or rewrite (none, see below).
- **Every MCP server:** stdio command and arguments, or remote URL and header names; the
  environment variables and `${VAR}` references it uses, and which of them it needs as secrets.
- **Launchers that fetch more code at start** (`npx`, `uvx`, `bunx`, `pipx run`, `docker run`
  with an image not pinned by digest, any `@latest`): flagged as "downloads code each start that
  the commit pin does not cover".
- **Text components:** commands, agents and skills by name; commands with `!` shell lines or
  `allowed-tools` flagged as not run by Imperium.
- **Content hash:** SHA-256 over the plugin's files (paths and bytes, without `.git` and
  Imperium's `.imperium-*.json`) plus the canonical JSON of its marketplace entry (an entry with
  `strict: false` supplies manifest fields). The install is refused if the content fetched again
  at install time has another hash.
- **For an update:** a diff of hooks and servers (added, removed, changed lines) and a statement
  that other files changed, when they did.

### Pinning

- An install pins the plugin to the commit fetched (a marketplace entry's `sha` or `ref` is
  honoured; otherwise the default branch's head at inspect time). The marketplace clone is pinned
  to the commit of its last refresh. Neither moves without an explicit refresh or update.
- `--sha256` (CLI) and `sha256` (protocol) pin the content hash a previous inspection showed.

### Updates re-ask

- An update inspects the new version and compares its hash with the approved one. For a plugin
  with hooks or servers in either version, **any** content change needs a fresh approval; without
  one the update installs the new content **off** and says so. For a plugin with neither, the
  update shows the summary and installs on.
- An update reads the source from the approval record, never from the plugin folder's
  `.imperium-source.json` (which the plugin or an agent could rewrite), and refuses a different
  marketplace or source: to move, uninstall and install afresh.

### Content changed in place

At session start the daemon checks each marketplace-installed plugin against its approval (a
cached tree digest, re-hashed when the folder's file count or newest modification time
changes). A mismatch marks it `modified`: none of its components load (its skills and commands
too, as a `modified` mod withholds everything), `plugins.list` says so, and only a fresh
approval of what is there turns it back on.

### Provenance and signatures (declined for now)

Claude Code marketplaces carry no signatures, and verifying git commit signatures needs an
allowed-signers list nobody publishes for marketplaces. This plan **declines signature
verification** and shows provenance instead (sources, commits, hash, unverified author claims).
Revisit when the Claude Code format adds signatures or the user wants `git verify-commit`
against a signers file they maintain.

### Who may approve

- **CLI:** the approval counts as given only when the caller passes `crate::peer::may_grant`
  (a process of the user's on the local socket, not a descendant of the daemon, not marked as
  spawned by it, not over SSH), exactly as for `mods.install`'s `interactive`. Otherwise the
  plugin installs off and the result's `notes` say why.
- **Desktop:** the Plugins window's trust dialog counts as an approval at the same standing as
  today's Connectors switch, which already turns on agent-installed plugins with hooks: a
  call from a holder of the daemon token. This is the same weakness the open item on
  roadmap [item 30](../12-roadmap.md#next-in-order) records for the mod main-context loader (a
  stronger check: a job object or subreaper, or a native non-webview dialog). That item is
  blocked on the user; this plan does not resolve it. When it is resolved, plugin approvals from
  the desktop adopt the same mechanism (a follow-up, not a stage here).
- **Agents:** `install_extension` with `name@marketplace` installs exactly as an agent install
  does today: off when the plugin has hooks or servers, with no approval. The safety floor asks
  about an agent's shell running `imperium plugin install|update|enable|approve` and
  `imperium config set|unset extensions.disabled_plugins`, and about file-tool writes into
  `<config>/plugins`, `<data>/marketplaces` and `plugin-approvals.json`
  ([stage 6](#stage-6-cli-and-the-safety-floor)). The floor is a guard against mistakes, not a
  boundary ([05](../05-tools-and-isolation.md#permissions)).

### What an approved plugin still cannot do

- **Permission rules, the safety floor and agent profiles are unchanged.** A plugin carries no
  permission rules; its MCP tools (`mcp__<plugin>_<server>__<tool>`) ask unless a user rule allows
  them; floor-covered calls ask every time; an agent's `plugins` allowlist and local-files-only
  lock ([14](../14-projects-and-automation.md#agent-permission-profiles-implemented)) still keep a
  plugin's servers and hooks out of that agent's sessions.
- **Hooks block but do not rewrite.** Marketplace plugins are treated like agent-installed ones:
  `may_rewrite = false` ([open question 3](#open-questions-for-the-user)).
- **No daemon environment.** `${VAR}` in a marketplace plugin's `.mcp.json` expands only
  `CLAUDE_PLUGIN_ROOT` and the plugin's own secrets (below), never the daemon's environment, so
  an approved remote server cannot be handed a token the user keeps in an environment variable.
  This is stricter than Claude Code and is reported in the inspection.
- **A repository cannot enable one.** An environment's `.imperium/config.toml` may still disable
  plugins (trusted folders only); it cannot add marketplaces or enable plugins.

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
| Marketplaces the user added | `[plugins.marketplaces.<name>]` in `<config>/config.toml` (`source`, optional `revision`), written with comments kept, as `[mcp.servers]` entries are | user config |
| Marketplace clone | `<data>/marketplaces/<name>/` (replaced atomically on refresh through a staging folder) | daemon cache, recreatable |
| Marketplace state | `<data>/marketplaces.json`: per marketplace the commit read, refresh time, and problems | daemon |
| Installed plugin | `<config>/plugins/<name>/`, with `.imperium-source.json` gaining `marketplace` and `plugin` fields | as today |
| Approvals | `<data>/plugin-approvals.json`, keyed by plugin name: `id` (`name@marketplace`), approved source and commit, content hash, the hooks-and-servers summary approved, time, and whether counted as interactive | daemon |
| On/off for the user | `extensions.disabled_plugins` (unchanged key and meaning) | user config |
| On/off per environment | `plugins: { enabled: [...], disabled: [...] }` on the environment record in `<data>/environments.json` | user-owned |
| Secrets | secret store, `plugin.<plugin>.<var>` | keychain |
| Removed plugins | `<data>/config-trash/`, with a `config.changes` entry so `config.undo` restores it | as today |

**Effective state for a session**, in order: installed and not `modified`; not in
`extensions.disabled_plugins`, unless the session's environment record lists it in `enabled`; not
in the environment record's `disabled`; not disabled by a trusted repository's
`.imperium/config.toml`; allowed by the agent profile. A plugin with hooks or servers is on only
when its approval matches.

**Configuration layers.** `plugins.marketplaces` is read from built-in defaults (none), the user
config and environment variables (`IMPERIUM_PLUGINS__MARKETPLACES='{…}'` read as TOML, per
[10](../10-configuration.md#layers); stage 2 checks how that layer spells a table keyed by name and
documents it); an environment variable can add a marketplace to browse, never
an approval. The environment layer refuses `[plugins]` entirely (a repository cannot add
marketplaces), and `imperium env config` shows that refusal.

## Protocol additions

A new `crates/imperium-protocol/src/methods/plugins.rs`, with TypeScript types generated for the
desktop. A paired phone cannot call `plugins.*` (as with `mods.*`). Each stage that adds methods
takes **the next protocol minor at its merge** and updates `PROTOCOL_VERSION`, the version line
in [02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method | Stage | Does |
| --- | --- | --- |
| `plugins.marketplace_add` | 2 | `{source, revision?}` → `MarketplaceInfo`. Clones, parses, records; refuses a name already added |
| `plugins.marketplace_list` | 2 | → `MarketplaceInfo[]` (name, source, commit, refreshed time, plugin count, problems) |
| `plugins.marketplace_refresh` | 2 | `{name?}` → per marketplace the commit before and after and plugins added, removed and changed |
| `plugins.marketplace_remove` | 2 | `{name}`; installed plugins stay installed, their updates report the marketplace missing |
| `plugins.browse` | 2 | `{marketplace?, query?}` → `PluginListing[]` (id, metadata, components or `null` for an external source, installed version and `update_available`) |
| `plugins.inspect` | 3 | `{id, revision?}` → `PluginPreview` ([trust prompt](#the-trust-prompt-shows-exactly-what-runs) contents, `sha256`, installed comparison) |
| `plugins.install` | 3 | `{id, sha256, approve, interactive, environment?}` → result with `enabled`, counted `interactive`, `notes` |
| `plugins.uninstall` | 3 | `{name, delete_secrets}` → the trash location and change id |
| `plugins.list` | 3 | `{environment?}` → installed plugins with id, source, commit, `modified`, effective on/off and why, required and missing secrets |
| `plugins.update` | 4 | `{name, sha256, approve, interactive}` → result like install, with `replaced` |
| `plugins.set_enabled` | 4 | `{name, enabled, environment?}`; enabling one with hooks or servers needs a matching approval |
| `plugins.approve` | 4 | `{name, sha256, interactive}`: approve the content that is installed (a `modified` or agent-installed plugin) |
| `plugins.set_secret`, `plugins.delete_secret` | 5 | `{name, variable, value?}`; the value never returns |
| `EnvironmentInfo.plugins` | 4 | the per-environment `enabled` and `disabled` lists, also on `environments.update` |

`mcp.list`'s `PluginInfo` gains optional `id` and `modified` (stage 3) so the Connectors window
keeps working unchanged.

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
  Unknown fields are ignored. Names must pass the existing `check_name` rules.
  (b) `plugins.rs`: read the full `plugin.json` (`version`, `author`, `homepage`, `repository`,
  `license`, `keywords`, and `commands`, `agents`, `hooks`, `mcpServers` as a path, a list of
  paths, or for `hooks` and `mcpServers` an inline object), honour those paths (inside the
  plugin root only) in addition to the default folders, and accept a manifest supplied by a
  `strict: false` marketplace entry.
  (c) `fn runs(&Plugin) -> RunSummary`: each hook (event, matcher, expanded command or URL) and
  each server (command, args, URL, header names, `${VAR}` references), with the
  fetches-code-at-start flag for the launchers listed in the security model.
  (d) Generalise `installed_by_agent` to "downloaded": a source file that names a session **or** a
  marketplace.
- Depends on: none.
- Owned paths: `crates/imperium-extensions/src/marketplace.rs` (new), `plugins.rs`, `lib.rs`
  (module and re-exports only), `crates/imperium-extensions/Cargo.toml` if a dev-dependency is
  needed.
- Acceptance checks: unit tests (in CI, `cargo test -p imperium-extensions`) for: each source
  form; `pluginRoot`; a relative source with `..` refused; an unsupported source listed;
  `plugin.json` custom paths and inline `mcpServers` and `hooks` loaded; a path outside the root
  refused; `strict: false`; `runs` flags `npx -y pkg@latest`; a marketplace-sourced plugin never
  reads the daemon environment in `${VAR}` and its hooks get `may_rewrite = false`; existing
  `plugins.rs` tests unchanged and passing.
- Risks: the public format has more fields than listed; unknown ones must stay ignored, not
  rejected. Component path semantics (does a custom `commands` path replace or add to the default
  folder?) follow Claude Code's documented "adds to"; record the choice in the module doc.
- Status: pending
- Outcome:
- Handoff notes: keep parsing free of I/O beyond reading the given folder, so stage 2 can parse a
  clone and stage 3 a staging copy with the same functions.

## Stage 2: Marketplaces in the daemon

- Objective: add, list, refresh and remove marketplaces, and browse them, through the protocol.
  `source` accepts a git URL, a local folder (absolute, or `~`), or `owner/repo` (exactly two
  name segments and no scheme → GitHub HTTPS). Clone with the existing `extension_install` git
  helpers (factor `git_fetch`, `git`, `copy_tree` and `Staging` into a shared
  `crates/imperium-core/src/fetch.rs` if needed, moving code, not changing it). A local folder is
  read in place, not copied, and re-read on refresh. Write `[plugins.marketplaces.<name>]` with
  comment-keeping edits (follow `crates/imperium-config/src/mcp_servers.rs`), and
  `<data>/marketplaces.json`. `plugins.browse` reads components from the clone for relative
  sources, `null` for external ones. Add `plugins` settings to `imperium-config` and make the
  environment layer refuse `[plugins]`.
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/marketplaces.rs` (new), `crates/imperium-core/src/fetch.rs`
  (new, if factored), `extension_install.rs` (only to use the factored helpers), `rpc.rs`
  (dispatch), `crates/imperium-config/src/settings.rs`, `environment.rs` (refusal),
  `crates/imperium-protocol/src/methods/plugins.rs` (new), `methods/mod.rs`, `lib.rs`
  (version), `snapshots/methods.txt`, `packages/protocol/src/generated/`, docs 02 (version line),
  06 (a new "Plugin marketplaces" section: marketplaces), 10 (`plugins.marketplaces`).
- Acceptance checks: daemon tests with a fixture marketplace in a temporary git repository and a
  folder: add (each source form; `owner/repo` asserted by URL without fetching), list, refresh
  after a new commit reports the changed entry, remove; a marketplace whose name is taken is
  refused; a malformed `marketplace.json` is reported in `problems` without failing the list;
  the marker-file plugin's hook and server have not run after add, refresh and browse (goal 5);
  an environment `.imperium/config.toml` with `[plugins]` is refused (`imperium-config` test);
  the methods snapshot lists the five methods; protocol minor bumped to the next at merge.
- Risks: `git clone` can take minutes; the RPC must not block other requests (run on a task, as
  `mods.inspect` does). GitHub's `owner/repo` heuristic must not swallow a relative folder named
  `a/b`: a folder that exists wins, and the CLI says which it chose.
- Status: pending
- Outcome:
- Handoff notes: if over 800 lines, split protocol and config (2a) from the daemon (2b).

## Stage 3: Inspect, approve, install and uninstall

- Objective: `plugins.inspect`, `plugins.install`, `plugins.uninstall` and `plugins.list`, the
  approval store, and the load-time check. Inspect fetches the plugin (relative source: copy from
  the marketplace clone; external: `extension_install::fetch` with the entry's `ref`/`sha`) into
  a `Staging` folder, parses it with stage 1, and hashes it (reuse `mod_install::tree_sha256`,
  extended with the marketplace entry). Install fetches again, refuses a different hash or a
  taken name, writes the source file, records the approval in `<data>/plugin-approvals.json`
  only when `approve` is set, counts `interactive` with `peer::may_grant`, adds the plugin to
  `extensions.disabled_plugins` when it has hooks or servers and the approval was not counted,
  and journals a `config.changes` entry. Load-time check: in
  `crates/imperium-core/src/state/extensions.rs`, compute a `blocked` list of
  marketplace-installed plugins whose digest differs from their approval and pass it in
  `Sources` so `imperium_extensions::load_trusting` drops them; cache digests by folder
  (file count and newest mtime). `PluginInfo` gains `id` and `modified`. Fix the 06 Hooks line
  about the isolation tier ([assumptions](#assumptions)).
- Depends on: stage 2.
- Owned paths: `crates/imperium-core/src/plugin_install.rs` (new), `mod_install.rs` (only to
  share the hashing helper), `state/extensions.rs`, `connectors.rs` (`PluginInfo` fields),
  `config_changes.rs` (the new change kind), `rpc.rs`, `crates/imperium-extensions/src/lib.rs`
  (`Sources.blocked_plugins`), protocol `methods/plugins.rs`, `methods/connectors.rs`, version,
  snapshot, generated types, docs 02 and 06.
- Acceptance checks: daemon tests: inspect then install a plugin with a hook and `approve`, once
  with the caller check passing and once failing (drive `peer::may_grant_from` with a fake daemon
  pid, as the `mod_install` tests do): passing → installed on with an approval record; failing →
  installed off with a note; a source changed between inspect and install is refused; a taken name is refused; install
  without approval leaves it in `extensions.disabled_plugins`; marker files absent after inspect
  and install; edit a file of an approved plugin → `plugins.list` says `modified` and
  `load_trusting` omits its hooks, servers, skills and commands; `config.undo` of an install
  removes it, and of an uninstall restores it; a skills-only plugin installs on.
- Risks: the largest stage; hashing every plugin at every session start (mitigated by the digest
  cache, measured in the test with 1,000 files as an estimate, not a benchmark). Seam if too large:
  3a inspect and install, 3b load-time check and uninstall.
- Status: pending
- Outcome:
- Handoff notes: the approval store mirrors `mod-approvals.json` (unreadable store → every
  marketplace plugin treated as unapproved and the list says why).

## Stage 4: Enable, disable and update per user and per environment

- Objective: `plugins.set_enabled` (user: edits `extensions.disabled_plugins` through the same
  path `config.set` uses; environment: edits the environment record's `plugins` lists),
  `plugins.update` and `plugins.approve`, and the effective-state order in
  [storage](#storage-layout-and-configuration). Enabling a plugin with hooks or servers without a
  matching approval is refused with "inspect and approve first". Update follows the
  [re-ask rules](#updates-re-ask), keeps the previous folder in `config-trash`, and reports
  "up to date" for an equal hash.
- Depends on: stage 3.
- Owned paths: `crates/imperium-core/src/plugin_install.rs`, `environments.rs` (record field),
  `state/extensions.rs` (environment lists), `rpc.rs`, protocol `methods/plugins.rs`,
  `methods/environments.rs` (or wherever `EnvironmentInfo` lives; find with
  `grep -rn "pub struct EnvironmentInfo" crates/imperium-protocol`), version, snapshot,
  generated types, docs 02, 06, 14 (environment record field).
- Acceptance checks: tests: disable for the user → not loaded in a new session's `Sources`;
  enable only for environment A → loaded in A, not in B; a trusted repository file disabling it
  in A wins; an update that changes only a skill file of a hook plugin installs off without a
  counted approval; an update that changes nothing is "up to date"; an update whose approval
  record names another marketplace is refused; enabling an unapproved hook plugin is refused;
  `plugins.approve` of an agent-installed hook plugin records it and allows enabling.
- Risks: two places now hold on/off (config and the environment record); `plugins.list` must say
  which decided, or users cannot tell why a plugin is off.
- Status: pending
- Outcome:
- Handoff notes: `extensions.disabled_plugins` stays the only user-level switch so the
  Connectors window needs no change.

## Stage 5: Plugin secrets

- Objective: required secrets from stage 1's `runs`, `plugins.set_secret` and
  `plugins.delete_secret`, the resolver change (a downloaded plugin's `${VAR}` reads
  `plugin.<plugin>.<var>` from the secret store, never the environment), missing secrets on
  `plugins.list`, servers with a missing secret not started (with a session notice), and
  `delete_secrets` on uninstall.
- Depends on: stage 3 (stage 4 not required).
- Owned paths: `crates/imperium-extensions/src/plugins.rs` (resolver input),
  `crates/imperium-core/src/plugin_install.rs`, `state/extensions.rs` (pass a secret lookup),
  `rpc.rs`, protocol `methods/plugins.rs`, version, snapshot, generated types, docs 02, 06, 10.
- Acceptance checks: tests with the in-memory secret store: a set secret expands into the
  server's env and nowhere else (assert the config file, plugin folder, `plugin-approvals.json`
  and journal do not contain the value); a missing one leaves the server unstarted with a notice
  and the plugin's skills available; a 65-character secret name is refused; the value is never
  in a protocol result.
- Risks: secret names are lowercase, so `FOO` and `foo` collide; refuse the second with the
  reason.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 6: CLI and the safety floor

- Objective: `imperium plugin marketplace add|list|refresh|remove`, `imperium plugin browse
  [query] [--marketplace m]`, `imperium plugin inspect <name@mp>`, `imperium plugin install
  <name@mp> [--env <env>] [--sha256 h] [--yes]`, `update <name> [--yes]`, `enable|disable <name>
  [--env <env>]`, `approve <name>`, `uninstall <name> [--delete-secrets]`, `list [--env]`,
  `secret set|delete <name> <VAR>`. Install and update print the trust prompt and ask
  `Install <id>? [y/N]`; without a terminal they refuse unless `--yes`, and `--yes` without a
  terminal installs a hook plugin off (follow `crates/imperium-cli/src/mods.rs`). The floor: add
  those subcommands (install, update, enable, approve) and `imperium config set|unset
  extensions.disabled_plugins` to `crates/imperium-policy/src/floor.rs` beside `imperium mods
  install|update`; add `plugin-approvals.json` and `marketplaces.json` to `FLOOR_PATHS`; protect
  `<config>/plugins` and `<data>/marketplaces` with `protect_dir` where the daemon protects
  `<config>/mods` (find with `grep -rn "protect_dir(" crates`).
- Depends on: stages 4 and 5.
- Owned paths: `crates/imperium-cli/src/plugins.rs` (new), `main.rs`, `crates/imperium-cli/snapshots/`,
  `crates/imperium-cli/tests/`, `crates/imperium-policy/src/floor.rs`, `permissions.rs`, the
  daemon line that calls `protect_dir`, docs 05 (floor list) and 06.
- Acceptance checks: CLI help snapshot updated; CLI tests against a test daemon: add the fixture
  marketplace, browse, install with `--yes` and no terminal → installed off with the note;
  `imperium plugin list` shows it; floor tests: `imperium plugin install x@y --yes`,
  `imperium plugin enable x`, `imperium config set extensions.disabled_plugins []` all ask;
  file-tool writes to `plugin-approvals.json` and into `<config>/plugins/x/` ask; reads do not.
- Risks: the floor reads command lines and is easy to sidestep; documented, not a boundary.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 7: Desktop Plugins window: marketplaces and browse

- Objective: a Plugins window built on the Connectors pattern from #223: the shared `Dialog`,
  `Tabs` (*Installed*, *Browse*, *Marketplaces*), search, `EmptyState`, `LoadingState`, `Notice`,
  `.card.interactive` cards and `--row-h` rows. This stage ships *Browse* (cards with name,
  marketplace, description, category, component counts, *Installed* / *Update available* badges,
  and a detail view listing components) and *Marketplaces* (add by URL, folder or `owner/repo`,
  refresh, remove, last refresh and problems). Installing is stage 8, so this stage's cards and
  detail view have no install button. Opened from the title
  bar, the command palette, Settings, and a *Manage plugins* link in Connectors' plugin section.
  **Why a separate window and not a Connectors tab:** Connectors is about MCP servers and
  already has its own *Installed* and *Browse*; a marketplace browser, a marketplace list and a
  trust dialog inside it would give it two meanings of *Browse*. The window reuses Connectors'
  components and structure instead.
- Depends on: stage 2 (stage 3 for installed badges, which show once it lands).
- Owned paths: `apps/desktop/src/plugins/` (new: `PluginsWindow.tsx`, `BrowseView.tsx`,
  `MarketplacesView.tsx`, `common.ts`), the place `ConnectorsWindow` is mounted and opened (find
  with `grep -rn "ConnectorsWindow" apps/desktop/src`), `apps/desktop/src/connectors/InstalledView.tsx`
  (the link only), the command palette entry, `apps/desktop/src/styles/` (only a new
  `plugins.css` if needed, tokens only), `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests with a mocked client for
  browse filtering, an external-source card showing "components after inspection", and add,
  refresh and remove of a marketplace; the visual checklist of the
  [interface polish plan](ui-polish.md) at 1280, 1024 and 420 px, light and dark, by keyboard.
- Risks: the window must stay on the shared components (the polish plan's style guard fails
  otherwise).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 8: Desktop install, trust and update

- Objective: the trust dialog (everything in [the trust prompt](#the-trust-prompt-shows-exactly-what-runs),
  with hooks and servers first, launchers flagged, provenance claims labelled unverified, and the
  hash) for install, update and approve; *Installed* tab rows with on/off for the user, a
  per-environment control, update, approve for `modified` plugins, uninstall (with "also delete
  saved secrets"), and password fields for required secrets.
- Depends on: stages 4, 5 and 7.
- Owned paths: `apps/desktop/src/plugins/` (`InstalledView.tsx`, `TrustDialog.tsx`,
  `SecretsForm.tsx`), `apps/desktop/test/`, doc 07.
- Acceptance checks: `bash scripts/check-web.sh` passes; component tests: the dialog lists every
  hook command and server command from a mocked `PluginPreview`; *Install* sends the shown
  `sha256`; an update with changed hooks shows the diff; a secret field never re-displays a
  saved value; by hand against a dev daemon with the fixture marketplace: install, enable,
  disable for one environment, update, uninstall, undo.
- Risks: the dialog's approval stands at the token-holder level (see [who may approve](#who-may-approve)).
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Stage 9: Agents, docs and close-out

- Objective: `install_extension` with `kind = "plugin"` accepts `source = "name@marketplace"` for
  an added marketplace (installed exactly as other agent installs: off with hooks or servers, no
  approval), and its schema text says so; a final pass over 05, 06, 07 and 10 for the finished
  feature; roadmap item 51 marked implemented; one public Claude Code marketplace the user names
  (for example Anthropic's `anthropics/claude-code` repository, if it still publishes one) added,
  browsed and one plugin inspected and installed, recorded in this plan's outcome.
- Depends on: stages 6 and 8.
- Owned paths: `crates/imperium-core/src/extension_install.rs` (source parsing),
  `crates/imperium-protocol/src/tools.rs`, `crates/imperium-protocol/schemas/tools/install_extension.json`,
  docs 05, 06, 07, 10, 12, and this plan.
- Acceptance checks: daemon test: an agent install of `x@fixture` with a hook lands in
  `extensions.disabled_plugins` and has no approval; `python3 scripts/ci/check-links.py .`
  passes; the manual run above recorded with the marketplace's commit.
- Risks: a real marketplace may use source types this plan leaves out; record them as follow-ups.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Risks across stages

- **The approval is the only control on code that runs unconfined.** A user who approves a
  malicious plugin is compromised, as in Claude Code. The inspection makes that decision
  informed; it does not make it safe. Confinement is a non-goal here.
- **Launchers outside the pin** (`npx …@latest`) make the hash meaningless for that server; the
  prompt flags them but installs them.
- **Desktop approvals stand at the token-holder level** until the open item on stronger caller
  checks is resolved.
- **Format drift.** Claude Code's format changes; unknown fields are ignored and unknown source
  types listed, so drift degrades to "unsupported" rather than failures.
- **Protocol minors.** Four stages bump the minor; concurrent PRs on `experimental` must each take
  the next free minor at merge and rebase the version line in 02 (by merge, not force-push).

## Open questions for the user

The defaults below stand unless you say otherwise; no stage waits on them.

1. **A default marketplace.** Ship Imperium with no marketplace added (default), or with one
   pre-added (for example Anthropic's official Claude Code marketplace)? Pre-adding one means
   Imperium vouches for a source it does not control.
2. **Repository recommendations.** Claude Code lets a repository's settings suggest
   marketplaces and plugins. Should a trusted project's `.imperium/config.toml` be able to
   *suggest* (never install or enable) marketplaces and plugins, shown to you as a prompt? Default:
   not in this plan.
3. **Hook rewrites.** Marketplace plugins' hooks may block but not rewrite tool calls or prompts
   (default, as for agent-installed plugins). Claude Code allows rewrites. Should an approval
   from your own terminal also allow rewrites, shown in the prompt?
