# 06 — Extensibility

Imperium has three extension surfaces, all installable from Git repositories, local folders,
or registries, and all declared through manifests with explicit permissions.

| Surface        | Runs in              | Written in                     | Examples                                   |
| -------------- | -------------------- | ------------------------------ | ------------------------------------------ |
| Agent plugins  | Daemon               | Markdown, JSON, any executable | Skills, hooks, slash commands, agent roles, MCP servers |
| UI mods        | Desktop webview      | TypeScript / React             | Panels, status bars, themes, telemetry views |
| Runtime adapters | Daemon (subprocess) | Any language                  | New inference backends, search providers   |

## Compatibility with existing formats

| Format                         | Support                                                                 |
| ------------------------------ | ----------------------------------------------------------------------- |
| MCP servers                    | Native                                                                  |
| Agent Skills (`SKILL.md` + resources) | Native; skills are listed by description and loaded on demand    |
| Claude Code plugins (`.claude-plugin/plugin.json`, `commands/`, `agents/`, `skills/`, `hooks/hooks.json`, `.mcp.json`) | Loaded through a compatibility layer; vendor-specific tool names are mapped where an equivalent exists and reported otherwise |
| `AGENTS.md` / `CLAUDE.md` instruction files | Read as project instructions                             |

Plugins written natively for Imperium use `imperium-plugin.toml`, a superset that adds
permissions, UI mod entry points, and router hints.

**Implemented (phase 5, `imperium-extensions`):**

- **Skills:** folders with a `SKILL.md` (front matter `name` and `description`) in
  `<config>/skills/`, the project's `.imperium/skills/` and `.claude/skills/`, and plugins'
  `skills/`; the first of a name wins. Every agent's instructions index each skill's name and
  description (and front matter `when_to_use`, when present), and agents call `load_skill`
  (`name`, optional `file` for a resource inside the skill folder) to load one; see
  [lazy skills and plugins](#lazy-skills-and-plugins-implemented).
- **Slash commands:** Markdown templates in `<config>/commands/`, the project's
  `.imperium/commands/` and `.claude/commands/`, and plugins' `commands/` (also reachable as
  `/<plugin>:<name>`; subfolders give `/<folder>:<name>`). A message starting with
  `/<name> args` is replaced by the template with `$ARGUMENTS` and `$1`, `$2`, ... filled in.
- **Plugins:** folders with `.claude-plugin/plugin.json` in `<config>/plugins/` or
  `extensions.plugin_dirs` contribute skills, commands, `hooks/hooks.json`, and `.mcp.json`
  servers (named `<plugin>_<server>`), with `${CLAUDE_PLUGIN_ROOT}` expanded. A plugin's
  servers start when an agent activates it (below). Other `${VAR}` and `${VAR:-default}` in
  `.mcp.json` read the daemon's environment, except in a plugin an agent installed (its folder
  has `.imperium-source.json`, [below](#agents-manage-the-app-config-implemented)): there only
  `${CLAUDE_PLUGIN_ROOT}` expands, so downloaded content cannot put the daemon's secrets into a
  server's command, arguments, headers, or URL. `agents/` and `imperium-plugin.toml` are
  reported as not yet supported.
- **Hooks:** see below. Problems (a skill without a description, an unsupported hook event)
  are shown as session notices.

### Lazy skills and plugins (implemented)

Only each skill's and plugin's name and short description are loaded up front; the rest loads
when an agent first uses it, keeping context and start-up cost low.

- **Index.** The system prompt holds one line per skill (`- name: description (use when:
  hint)`) and one per plugin with MCP servers (`- name: description (servers: ...)`), with how
  to load them. No skill body is read and no plugin server starts when a session opens.
- **Skills.** `load_skill` returns a skill's instructions, once per agent conversation: a
  second call says the skill is already loaded. Each first load is journalled as
  `extension_loaded` (`kind: skill`). A compaction that summarises the conversation forgets
  the agent's loaded skills, so they can be loaded again.
- **Plugins.** `activate_plugin` (offered while some plugin is inactive) starts the plugin's
  MCP servers for the session; their tools are offered from the agent's next model call, as
  `mcp__<plugin>_<server>__<tool>`, to every agent of the session. Activation is journalled
  as `extension_loaded` (`kind: plugin`, with the tools it added). A plugin whose servers all
  fail is not activated and can be tried again.
- **Resume.** A resumed session replays its journal: plugins it had activated start again
  before the first turn, and skills an agent had loaded (since its last compaction) are not
  loaded twice.
- **Hooks are eager.** Plugin and user hooks are active from the session's start, whatever
  `extensions.lazy` says: a `tool.before` or `prompt.submit` hook can be a security check that
  must see every call, and hooks cost no context and run nothing until their event.
  Slash commands are expanded by the daemon, never shown to the model, so they need no index.
- **Eager loading.** `extensions.eager` names skills and plugins to load up front (an eager
  skill's instructions are in the system prompt; an eager plugin's servers start with the
  session); `extensions.lazy = false` makes everything eager ([10](10-configuration.md)).
- **Cost.** Context breakdowns count the index as `system_skills` ("Skill and plugin index"),
  loaded skill instructions (eager ones and `load_skill` results) as `skills_loaded`, and
  activated plugins' tool definitions as `tools_plugins`
  ([03](03-agent-engine.md#context-management)).

```toml
[extensions]
lazy = true                 # the default
eager = ["house-style"]     # always loaded
```

## Agents manage the app config (implemented)

Agents add to the user's own Imperium config through three tools the daemon runs, so they work
in every session, whether or not it is in an environment's folders: they never write through the
session's file tools.

| Tool | Does | Without a rule |
| --- | --- | --- |
| `set_model_card(model, card)` | Writes a model's card (a model table id or a local model id; `null` clears it) to the app settings file, as the model dialog does, and applies it at once ([10](10-configuration.md#model-cards)) | Runs |
| `install_extension(kind, source, revision?, path?)` | Installs a `skill`, `plugin`, or `mod` from a git URL (`https://`, `ssh://`, `git@host:path`, `file://`) at a branch, tag, or commit, or from a folder on this machine (relative to the session's folder); `path` picks a folder inside the source | Runs; the tool notice (and an approval, when a rule asks) names the source |
| `uninstall_extension(kind, name)` | Moves an installed skill, plugin, or mod out of the config folder | Runs for something an agent installed; asks otherwise |

Each tool is its own permission key, so `[permissions]` rules (global or per environment) allow,
ask about, or deny it ([05](05-tools-and-isolation.md#rules)):

```toml
[permissions]
rules = [
  { tool = "install_extension", action = "ask" },
  { tool = "uninstall_extension", action = "deny" },
]
```

**Installing runs nothing.** A git source is cloned with `git clone` (shallow, no submodules, no
Git LFS downloads, no prompts, the `ext::` transport refused, five-minute limit); a folder is
copied. Either way `.git` and symbolic links are left out, the copy is limited to 256 MiB, and the
result must be the kind asked for (`SKILL.md` with a description, `.claude-plugin/plugin.json`
that loads, or a valid `mod.json`). It lands in `<config>/skills/<name>`, `<config>/plugins/<name>`
or `<config>/mods/<id>`, never over an existing one, and no install script, build, or hook runs.
After that, the usual rules apply: skills are listed in the index of new sessions and load when
an agent first uses them ([lazy loading](#lazy-skills-and-plugins-implemented)); a mod an agent
installed is opt-in in Settings → Mods, like the bundled ones. Imperium has no extension
registry yet, so a bare name is refused.

**A plugin with hooks or MCP servers installs turned off.** Its hooks are active from a session's
start, and an agent's `activate_plugin` would start its servers: downloaded code running as the
user with no approval, which is the same as the install running it. So the install adds the
plugin to `extensions.disabled_plugins` and tells the agent so. A disabled plugin is not offered
to sessions, `activate_plugin` cannot start it, and nothing in it runs until the user turns it on
in Connectors (a setting the daemon reads at start, so it applies after a restart), where its
source (URL, commit, and the installing session) is shown. A plugin
with neither (skills and commands only) is listed at once. Agent-installed plugins also never
read the daemon's environment through `${VAR}` in `.mcp.json` (see [Compatibility](#compatibility-with-existing-formats)),
even after the user turns them on. Plugins agents installed before this change and that are
already enabled stay enabled; only their `${VAR}` expansion tightens.

**Provenance.** Each install writes `.imperium-source.json` into its folder: `kind` (`git` or
`path`), `location` (the URL or folder), `revision` (the commit installed) and `requested` (the
branch or tag asked for), `subdir`, `installed_at_unix_ms`, and the `session_id` of the agent.
`mods.list` and `mcp.list` report it as `installed_from` on each mod, plugin, and skill, and the
desktop shows it in Settings → Mods and the Connectors window.

**Change journal and undo.** Every change is appended to `<data>/config-changes.json` (the
newest 1,000 are kept) with an id, the time, the session, a summary, and what undo needs: a
card's `before` and `after` (what the app settings file held), or an extension's folder and
source (and for a removal, where its folder was kept, `<data>/config-trash/`). `config.changes`
lists them newest first and `config.undo { id }` reverses one: a card goes back to what it was
(refused if it was edited since), an install is removed (only if its folder still records the
same source; a plugin is also taken off `extensions.disabled_plugins`), and a removed extension
is put back (refused if something of that name was installed since). Settings → *Changes by
agents* and `imperium config changes` / `imperium config undo <id>` show and undo them.

## Hooks

Hook events: `session.start`, `session.end`, `prompt.submit`, `tool.before`, `tool.after`,
`agent.spawn`, `agent.complete`, `model.before`, `model.after`, `compact.before`,
`notification`.

- A hook is a command (any executable receiving JSON on stdin) or an HTTP endpoint.
- `tool.before` and `prompt.submit` hooks can allow, deny, or rewrite; others observe.
- Hooks run with timeouts and inside the session's isolation tier.

**Implemented (phase 5):** command hooks for `session.start`, `prompt.submit`, `tool.before`,
`tool.after`, `agent.spawn`, and `agent.complete`, from `[[hooks]]` in the user configuration
and from installed plugins (a project's own hooks are not run, since opening a repository
would run its commands). A hook runs through the shell in the project folder with
`IMPERIUM_PROJECT_DIR` (and `CLAUDE_PROJECT_DIR`) set and receives the event as JSON on stdin
(`hook_event`, `tool_name`, `tool_input`, and event fields such as `prompt` or `tool_output`).
`tool.before` and `prompt.submit` hooks block by exiting with code 2 (stderr is the reason) or
printing `{"decision": "block", "reason": "..."}`; a blocked tool call tells the model why,
and a blocked message is reported to the user. Other failures are logged and do not block.
Matchers name tools, `|`-separated, and accept Claude Code's names (`Bash`, `Edit|Write`,
`Read`); Claude Code event names (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`,
`SessionStart`, `SubagentStop`, `Stop`) map to the events above. Rewriting, HTTP hooks, and
the other events are later work.

```toml
[[hooks]]
event = "tool.before"
matcher = "run_command"
command = "python D:/hooks/check_command.py"
timeout_s = 10
```

## UI mods

UI mods are ES modules loaded into the desktop app, each in its own **sandboxed iframe** with
a message-passing bridge — a mod cannot reach the daemon token, the filesystem, or other mods
directly.

```ts
import { defineMod } from "@imperium/mod-sdk";

export default defineMod({
  id: "tokens-per-dollar",
  permissions: ["telemetry:read"],
  contributes: { panels: [{ id: "tpd", title: "Tokens / $", slot: "sidebar.right" }] },
  activate(ctx) {
    ctx.telemetry.subscribe("call.completed", (e) => ctx.panel("tpd").render(/* ... */));
  },
});
```

**Implemented (phase 5):** mods installed in `<config>/mods/<id>/` with a `mod.json`
(`id`, `name`, `version`, `entry` (default `index.js`), `permissions`, and
`contributes.panels` with `id` and `title`) are listed by the daemon's `mods.list` and shown as
panels in the right-hand column. Each panel runs in its own frame with `sandbox="allow-scripts"`
and no same-origin access, so the mod cannot reach the app window, the daemon token, or other
mods. In the desktop app the frame loads a page from the app's `imperium-mod` protocol, which
carries a content security policy of its own (inline scripts and `data:` modules, nothing from
the network), and receives its document from the app; the app's own policy allows no inline
scripts and would block a `srcdoc` frame. The installers build bundled mods first and ship only
their manifest, entry and notices. `@imperium/mod-sdk` (`packages/mod-sdk`) provides `defineMod` and the context types;
inside the frame the bare import resolves through an import map, so both bundled and unbundled
mods work. The context offers `telemetry.subscribe("call.completed", ...)` (permission
`telemetry:read`), `sessions.subscribe(...)` (raw session events, `sessions:read`), and
`panel(id).render(html | node)`; the app forwards only the events a mod's permissions allow.
Theme tokens are copied into the frame.

Beyond pushed events, a frame asks the app for data with request/response messages (`request`
`{id, method, params}` answered by `response` `{id, result}` or `{id, error}`); the app answers
only the window of that frame, and only for methods the mod's permissions cover.
`sessions:read` adds `sessions.list()` (snapshots of every session and its agents, with each
agent's status), `sessions.selected()`, and `sessions.onChange(...)`, which the app pushes at most
every 250 ms. `pets:read` adds `pets.list()` (installed Pixel Pets packages, whether or not the
daemon's own pet is on) and `pets.strip(petId, animation)` (a `data:image/png` URL, served by the
daemon from the pet's own `strips/` folder). `storage` adds `storage.get(key)` and
`storage.set(key, value)`: JSON values kept per mod in the app's `mods` document, with keys up to
128 characters and at most 256 KiB per mod (setting `undefined` removes a key).

Mods shipped with Imperium live in its `mods/` folder (bundled into the app, or the source tree
when developing) and are listed after the installed ones; a mod installed in `<config>/mods`
with the same id wins. `mods.list` marks them `bundled`. Bundled mods are opt-in (their panels
show only after the user turns them on in Settings → Mods), except Imperium-Nursery, which is on
until turned off, as installed mods are. That section also lists each mod's version,
permissions, panels, and load problems. A panel's `slot` picks the column it docks in:
`sidebar.left` or `sidebar.right` (the default).
An example lives in
`examples/mods/tokens-per-dollar/`. Other contribution points, install and update commands,
permission prompts at install, and trusted main-context mods are later work.

Contribution points: new panels, modules placeable inside any panel
([layout](07-desktop-ui.md#modules-inside-panels)), extensions to any panel's slots, panel wrappers and
replacements ([panels](07-desktop-ui.md#panels)), status-bar items, message renderers (custom
rendering per content type), command-palette commands, settings pages, themes (CSS tokens),
and layout presets. Trusted mods may opt into running in the main webview context for
performance, with a prominent warning at install time.

## Telemetry API

Available to mods (`telemetry:read`), the CLI (`imperium telemetry`), and external tools
(OpenTelemetry/OTLP export and a Prometheus endpoint, both off by default).

| Stream / metric            | Fields                                                                 |
| -------------------------- | ---------------------------------------------------------------------- |
| `call.started/completed`   | agent, model, source, prompt/cached/completion tokens, TTFT, tokens/sec, cost |
| `agent.*`, `task.*`        | lifecycle, depth, parent, budget used/remaining, status                |
| `runtime.*`                | load/unload, memory reserved vs measured, slot usage, health           |
| `hardware.sample`          | per-GPU VRAM/utilisation/temperature, RAM, CPU (configurable interval) |
| `tool.*`                   | tool, duration, outcome, permission decision                           |
| Ledger queries             | cost and tokens grouped by session, agent, model, endpoint, day        |

## Permissions model for extensions

Manifests declare permissions (`fs:read:<glob>`, `exec`, `net:<domain>`, `telemetry:read`,
`session:write`, `secrets:<name>`, ...). Users approve them at install and on change; the
daemon enforces them. Extensions are pinned by version and content hash; updates show a diff of
requested permissions.
