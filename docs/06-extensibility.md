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
  server's command, arguments, headers, or URL. A plugin's `agents/` folder contributes
  [named agents](#named-agents-implemented). `imperium-plugin.toml` is reported as not yet
  supported.
- **Hooks:** see below. Problems (a skill without a description, an unsupported hook event)
  are shown as session notices.

### Named agents (implemented)

A named agent is a subagent a file defines, in Claude Code's format, so plugins and projects can
ship them: a Markdown file whose YAML front matter says who it is and whose body is its
instructions.

```markdown
---
name: code-reviewer
description: Reviews a diff for bugs and missing tests. Use after a change is written.
tools: Read, Grep           # or a YAML list; left out: the delegator's tools
model: inherit              # or a model id or <endpoint>:<model>; left out: normal routing
---
You review code. Report only problems you can point to ...
```

- **Fields.** `name` (required, kebab-case) and `description` (required: when to use it) must be
  set, and the body must not be empty, or the file is skipped with a notice. `tools` is a comma
  list or a YAML list; `model` is a model id or spec, or `inherit`. Any other key (`color`,
  `skills`, ...) is ignored with one notice per file. The front matter is read as the YAML subset
  these files use (plain, quoted and block scalars, block and inline lists); malformed YAML skips
  the file with a notice.
- **Sources, lowest precedence first, by name:** plugins' `agents/`, the user's
  `<config>/agents/`, the project's `.claude/agents/`, the project's `.imperium/agents/` (a
  later source replaces an earlier agent of the same name; two files of one folder with one name
  keep the first by path). Files are found up to three folders deep, `.md` only, at most 64 KiB.
- **Project files are repository content.** A project's agent files are read only when the
  project is in `mcp.trusted_projects`, the same trust that starts a project's `.mcp.json`
  servers ([10](10-configuration.md)); otherwise one notice per folder says how many files
  were not loaded. A symbolic link as or above a project's agents folder, or as an agent file, is skipped, as is anything but a regular file.
- **Delegating.** The root's prompt lists each agent's name and the first line of its
  description (at most 40 agents, and no body), with how to use them: `spawn_agent` takes
  `agent` (the name). The subagent runs with the file's body after its own base instructions.
  Everything else about `spawn_agent` is unchanged (`access`, `effort`, `knowledge`, tiers and
  tags); `single_call` cannot be combined with `agent`. An unknown name fails the call and lists
  the defined names.
- **`tools` narrows, never widens.** The subagent is offered the tools that are in both the
  file's list and the delegator's own tools at that moment; a name that matches none of them is
  left out with a notice, and a file whose names all miss gets no tools. Claude Code's names map
  to Imperium's (`Bash` is `run_command`, `command_output` and `command_stop`; `Read`, `Write`,
  `Edit` and `MultiEdit`, `LS`, `Glob`, `Grep`, `WebFetch` and `WebSearch` to their equivalents).
  A call to a tool outside the list is refused where calls run, as well as left out of the list.
  Left out, `tools` gives what any subagent of the session has. A resumed named agent
  (`resume_agent`, after a restart too) and a task retried on a stronger route keep the same
  instructions and tool list.
- **A file never grants a permission.** Permission rules (deny over ask over allow), the safety
  floor, plan mode, the agent's permission profile and MCP and plugin allowlists are applied to
  every call of a named agent exactly as to any subagent's: the file's tool list is only a
  further filter in front of them.
- **`model`.** A preference, never an override. `inherit` is the delegating agent's model; a model
  id or spec goes through the same routing as `spawn_agent`'s `model`. The file's choice applies
  only when it satisfies the location the task is confined to (the call's `location`, else a
  user routing rule's); otherwise, and when the model is not available, the task is routed
  normally (tier, tags and rules) and a notice says why. `model` on the call overrides the file;
  with neither, ordinary routing applies.
- **Clients.** `mcp.list` results carry `agents` (name, description, origin, path, tools, model)
  for the user's and enabled plugins' agents (protocol 0.42); a project's are loaded per session, for
  trusted projects only. `imperium agents list` prints them. The desktop shows none yet.

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

Hook events: `session.start`, `session.end`, `prompt.submit`, `turn.start`, `turn.end`,
`tool.before`, `tool.after`, `agent.spawn`, `agent.complete`, `permission.requested`,
`model.before`, `model.after`, `compact.before`, `notification`.

- A hook is a command (any executable receiving JSON on stdin) or an HTTP endpoint.
- `tool.before` and `prompt.submit` hooks can allow, deny, or rewrite
  ([below](#rewriting-hooks-implemented)); others observe.
- Hooks run with timeouts and inside the session's isolation tier.

**Implemented:** hooks for `session.start`, `session.end`, `prompt.submit`, `turn.start`,
`turn.end`, `tool.before`, `tool.after`, `agent.spawn`, `agent.complete`, and
`permission.requested` (payloads [below](#hook-payloads)), from `[[hooks]]` in the user configuration
and from installed plugins (a project's own hooks are not run, since opening a repository
would run its commands). A hook runs through the shell in the project folder with
`IMPERIUM_PROJECT_DIR` (and `CLAUDE_PROJECT_DIR`) set and receives the event as JSON on stdin
(`hook_event`, `tool_name`, `tool_input`, and event fields such as `prompt` or `tool_output`).
`tool.before` and `prompt.submit` hooks block by exiting with code 2 (stderr is the reason) or
printing `{"decision": "block", "reason": "..."}`; a blocked tool call tells the model why,
and a blocked message is reported to the user. Other failures are logged and do not block.
Matchers name tools, `|`-separated, and accept Claude Code's names (`Bash`, `Edit|Write`,
`Read`); Claude Code event names (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`,
`SessionStart`, `SessionEnd`, `SubagentStop`, `Stop`) map to the events above. The events not
listed are later work.

```toml
[[hooks]]
event = "tool.before"
matcher = "run_command"
command = "python D:/hooks/check_command.py"
timeout_s = 10

[[hooks]]                       # an HTTP hook
event = "turn.end"
url = "https://hooks.example.com/imperium"
timeout_s = 5
headers = { Authorization = "Bearer ${secret:hook-token}" }
```

### Hook payloads

Every hook gets a JSON object with `hook_event` (the Imperium name) and the fields below;
tool events also get `tool_name`, and a hook's `matcher` filters tool events and
`permission.requested` by tool.

| Event | Fields |
| --- | --- |
| `session.start` | `session_id`, `project_dir` |
| `session.end` | `session_id`, `reason` (`closed`: every handle was dropped, or `stopped`) |
| `prompt.submit` | `prompt` (can block) |
| `turn.start` | `session_id`, `prompt` (after commands and `prompt.submit` hooks) |
| `turn.end` | `session_id` (after the turn's messages are saved) |
| `tool.before` | `agent_id`, `tool_input` (can block) |
| `tool.after` | `agent_id`, `tool_input`, `ok`, `tool_output` |
| `permission.requested` | `agent_id`, `tool`, `summary` (the request is shown to the user at the same time) |
| `agent.spawn` | the new subagent's task details |
| `agent.complete` | `task_id`, `ok`, `report` |

`session.end`, `turn.start`, `turn.end`, and `permission.requested` only observe, and a block
decision from them is ignored. A hook cannot answer a permission request, so no hook can approve
what a rule, a profile, or the safety floor leaves to the user
([05](05-tools-and-isolation.md#rules)). They also run **detached**: the daemon starts them in
their own task and does not wait, with five seconds for all of an event's hooks (a command's
process is killed, an HTTP request dropped), so a slow hook or endpoint never holds up a turn, an
approval prompt, or a stop. Their delivery is therefore not ordered with the session's other
events, and one can be lost if the daemon exits first. The other events run inline and wait for
their hooks, up to each hook's `timeout_s`.

### HTTP hooks

An HTTP hook has `url` instead of `command`: the payload is POSTed to it as JSON, and a 2xx
answer may carry `{"decision": "block", "reason": "..."}` on the blocking events, exactly as a
command's output does. Anything else (a refusal, a timeout, another status) is logged and does not
block, as for commands. They are bounded and cannot reach beyond the URL the user configured:

- The URL must be `https`, or `http` to an IP-literal loopback address (`127.0.0.0/8`, `::1`) or
  `localhost`, and must not hold credentials. The check runs when the configuration loads and
  again before each request. For plain `http://localhost` the name is resolved first, every
  address must be loopback, and the request connects to exactly those addresses.
- No proxy is used, whatever the environment sets (`HTTP_PROXY` and the like).
- **No redirects** are followed (a 3xx answer is a failure), so a server cannot pass the payload,
  or a header secret, on to another host.
- `timeout_s` bounds the whole exchange, the body included (10 s by default); the answer is read
  up to 64 KiB. Every configured hook, command or HTTP, is clamped to 300 s.
- Header values may use `${secret:name}`, replaced by the keychain secret of that name
  (`imperium secret set name`) when the hook runs; nothing else (not the environment, not a
  project file) is expanded, and a missing secret, or one holding a control character (a line
  break would split the header), fails the hook before anything is sent.
- HTTP hooks come only from the user's configuration; a plugin's `hooks.json` entry of
  `"type": "http"` is reported and skipped, and a repository's hooks are never run.
- The payload holds prompts and tool output, so only configure an endpoint you trust with them.
- **HTTP hooks are sent by the daemon, not by an agent's tools.** They ignore an environment's
  `network = false`, an agent's local-files-only lock, and the isolation tier: those limit what an
  agent's tools reach, and a hook is the user's own configuration. An `https` endpoint can be
  anywhere on the internet, so configuring one sends session data there.

Hooks do not widen anything: an agent's permission profile still decides which plugins' hooks
load, and the safety floor, permission rules and approvals apply before and regardless of any
hook.

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
`examples/mods/tokens-per-dollar/`. Other contribution points and trusted main-context mods are
later work.

### WebAssembly pet behaviours (implemented)

A mod can steer pets with a small WebAssembly module, `ctx.behaviours` (no permission: a module
can touch nothing but its own memory). The module is not compiled until the host has read it and
proved the rules below; it runs in the mod's own sandboxed frame, whose policy allows
`wasm-unsafe-eval` and still no network, so no new capability reaches the network or the app. It does make WebAssembly available to all of the mod's JavaScript, not only to behaviours, and adds the engine as attack surface; the checks below protect the host from runaway *behaviours*, they are not a defence against a malicious mod, which already runs arbitrary JavaScript in its frame (a mod that spins forever can freeze the window on engines that share the page's thread, such as WKWebView and WebKitGTK; that was already true of JavaScript).

**Where it runs, and why.** In the frame (the webview), not the daemon: no WebAssembly runtime is
in the workspace, so the daemon route would add a native dependency (and an engine to keep
patched) for something only the desktop's Nursery uses, while the frame needs only the platform's
engine. The cost is that a browser engine has no fuel metering and cannot interrupt a running
call, so limits are proved up front instead of metered:

- **Bounded steps.** No `loop`, no `call_indirect`, no recursion, no tables and no start function,
  so a tick is straight-line code with forward branches and calls down an acyclic call graph. The
  host counts the worst case exactly (each function's instructions plus its callees', per call
  site) and refuses a module that could run more than 100,000 instructions in a tick. A module
  that wants repetition unrolls it.
- **Deterministic.** Only 32-bit integer arithmetic and memory access: no floating point (NaN
  bits), no 64-bit values, no SIMD, no bulk memory. Its one import is `imperium.rand`, a seeded
  generator (mulberry32); the seed is derived from the pet's key. There is no clock: time is the
  `dt` the host writes into the input each tick.
- **Memory caps.** One memory of at most 2 pages (128 KiB) with a declared maximum, no
  `memory.grow`; at most 64 functions, 64 locals each and 32 globals; at most 4 KiB of module
  (the size Chromium will compile synchronously); at most 32 live behaviours per frame.
- **No ambient access.** No import but `imperium.rand` (anything else is refused before it is
  compiled, and an engine-level link would fail anyway), no network, files, DOM or `postMessage`.
- **Failures stop the module, not the app.** A trap (including an out-of-range memory access or a
  division by zero), three ticks that each took over 50 ms (measured by the host, which cannot interrupt
  one), or an invalid answer stops that behaviour for good, and every later tick returns an error.
  The frame reports it to the app (`behaviours.report`, needs no permission, checked, clipped and
  capped there: ten kept per mod, fifty in all, repeats dropped) and Settings → Mods lists "A behaviour of <mod> was refused/stopped". The
  mod carries on: the Nursery's pet goes back to its built-in roaming.

**ABI** (32-bit little-endian words at the start of the module's exported `memory`, which must be
at least 128 bytes; the host clears words 0 to 31 before every tick, then writes the input):

| Word | Dir | Meaning |
|------|-----|---------|
| 0 | in | ABI version, 1 |
| 1 | in | `dt`: ms since the last tick, 0 to 1000 |
| 2 | in | tick count |
| 3 | in | index of the pet's current animation in `behaviours.animations` |
| 4 | in | flags: 1 the pet is free to roam, 2 it is busy (working, waiting, reacting) |
| 5 | in | ms since the pet last had an event, 0 to 600000 |
| 6, 7 | in | x and y of its feet, px |
| 8 to 11 | in | the room's bounds: minX, maxX, minY, maxY |
| 16 | out | a one-shot to request: 0 none, else a 1-based index in `behaviours.shots` (waving, jumping, celebrating, fidget); out of range stops the module. The pet's arbiter still decides whether it plays |
| 17, 18 | out | velocity x and y in px/s, clamped to ±60 |

The module exports `memory` and `tick()` (no parameters, no result); everything it keeps lives in
its globals and memory. `ctx.behaviours.load(name, bytes, { seed })` returns `{ ok, behaviour }`
or `{ ok: false, error }`; `behaviour.tick(state)` takes `dt`, `anim`, `free`, `busy`, `quiet`,
`x`, `y` and `bounds` and answers `{ ok, shot, vx, vy }`. A behaviour reads what the host gives
it and nothing else, and it can ask only for a velocity and a harmless one-shot, never for an
animation that would report agent status (`failed`, `alert`, `waiting`).

**Approval.** A behaviour ships inside the mod (the bundled Nursery builds its module from source
at start), so the mod's content hash and approved permissions cover it: edit the module and the
mod is `modified`, off and without permissions until approved again ([below](#installing-and-updating-mods-implemented)).
A behaviour in a separate file, or in a pet package (`imperium.behaviour` in `pet.json`), is not
built, because nothing would bind its bytes to an approval yet.

The example is `mods/imperium-nursery/src/behaviour/wander.ts`, assembled with the 100-line
assembler beside it (`asm.ts`, no toolchain needed; any compiler that follows the rules works).
The Nursery's **Roaming** setting chooses it over the built-in roaming.

### Installing and updating mods (implemented)

`imperium mods install <git-url-or-folder> [--path <folder>] [--revision <rev>] [--sha256 <hex>]
[--yes]` and `imperium mods update <id> [--yes]` install a mod into `<config>/mods/<id>` and
update it from the source it was installed from (recorded in `.imperium-source.json`). Nothing in
the mod runs, and an install is a fetch (a shallow `git clone`, or a copy of a folder) like an
agent's `install_extension` ([above](#agents-manage-the-app-config-implemented)); the difference
is the user's approval:

1. The CLI asks the daemon to **inspect** the source (`mods.inspect`): the manifest's id, name,
   version, requested permissions and panels, the source and commit, and a **content hash**
   (SHA-256 over every file's path and bytes, not `.git` or Imperium's own `.imperium-*.json`).
2. It prints them, describing each permission, and asks `Install <name>? [y/N]`; without a
   terminal it refuses unless `--yes` was given. `--sha256` pins the hash a previous run showed.
3. It then calls `mods.install` with the hash and the approved permissions. The daemon fetches
   again and installs only if the content still has that hash and the manifest asks for nothing
   that was not approved, so a branch that moved in between, or a source that changes with the
   prompt, is refused. It never writes into a folder that is not that mod, and its staging
   folder has an unpredictable name made with `create_dir`.
4. **The approval lives in the daemon, not in the mod.** `<data>/mod-approvals.json`, keyed by
   mod id, holds the content hash, version, approved permissions, the approved source, and
   whether the user answered a prompt at a terminal; the mod's folder holds no approval (a
   `.imperium-approval.json` a mod writes is ignored). `mods.list` reports a mod with the
   permissions that are both in its manifest and approved. If its files no longer hash to the
   approved content (it was edited, or its manifest now asks for more) it is marked `modified`,
   has **no** permissions, and is off until the user turns it on; installing it again (after
   uninstalling) approves what is there. If the approvals file exists but cannot be read, every
   installed mod is treated that way (an absent file just means nothing was approved through
   this command). A mod approved with `--yes` and no terminal prompt is installed but **off**
   until the user turns it on in Settings → Mods; so is an update that adds permissions without
   a prompt (only an update that asks for nothing new keeps an earlier prompt's standing), and a
   `modified` mod stays off even if the app had saved it as on. Whether the answer came from a
   prompt (`interactive`) is stated by the CLI, which checks for a terminal; the daemon counts it
   only from a process that [may grant it](#who-may-grant-trust-and-approval-implemented) and
   otherwise installs the mod off and says so (`notes` in the result). The floor also asks about
   the command and about file-tool writes into `mod-approvals.json` and the installed mods
   (`<config>/mods`), but it reads command lines and is not a security boundary
   ([05](05-tools-and-isolation.md#permissions)). A mod without an approval record (copied in
   by hand, or installed by an agent) behaves as before.
5. **Update** reads the source from the approval record, not from the folder's
   `.imperium-source.json` (which a mod could rewrite), and refuses a source, folder or revision
   other than the approved one: to change where a mod comes from, uninstall it and install it
   afresh. It compares what the new version requests with what was approved for the installed
   one and prints the difference (`+` new, `-` no longer requested); when the hash changed it
   also says so, so new code under the same permissions is not silent. It asks again only when
   something was added, and installs nothing wider than what was approved; `--sha256` pins the
   hash to install; the previous folder is kept in `<data>/config-trash/`. A mod whose source
   gives the same hash is reported up to date. A mod without an approval record, or a bundled
   one, is not updated this way.
6. **The safety floor asks about `imperium mods install|update`**: an agent running it in a shell
   (with its own `--yes`) would approve its own code, so the person decides each time
   ([05](05-tools-and-isolation.md#permissions)). Paired phones cannot call `mods.*`.

A mod's permissions only decide which data its frame may ask the desktop app for (above). They
are not agent tool permissions: a mod cannot call tools, widen a permission profile, or lower
the safety floor, and the user's approval at install does not change either. A mod installed this
way is on at once when the user approved it at a terminal prompt, like any installed mod; one
approved with `--yes`, or installed by an agent, stays opt-in in Settings → Mods.

### Rewriting hooks (implemented)

A `tool.before` hook may answer with `{"updated_input": {...}}` (an object: the tool's new
arguments, under the schema's names, as the hook received them in `tool_input`), and a
`prompt.submit` hook with `{"updated_prompt": "..."}`; a command prints it as JSON on stdout
(exit 0), an HTTP hook returns it as its answer. A block always wins and drops every rewrite.
Several hooks rewrite in order, each seeing the last one's result. The rules:

- **The rewritten call is a new call.** It runs again from the start through everything the
  model's own call goes through: an initiative review's tool limits, the agent's permission
  profile and local-files-only lock, the tool's own validation (a rewrite that does not parse
  fails as the model's would), the user's permission rules, the safety floor and its path rules,
  and approval. A pre-approval or "always allow" the original had does not carry over to a call
  the floor covers. So a rewrite can change or narrow a call and never gets one past a check.
- **The same tool.** `updated_input` replaces the arguments only; a hook cannot turn a call into
  another tool's (that would be a new call, and is not offered). A tool that is not a built-in
  or MCP tool (the agent's own delegation and planning tools) runs no `tool.before` hooks.
- **One round.** Hooks run again on the rewritten call but cannot rewrite it a second time (they
  can still block it), so hooks cannot loop.
- **Seen.** The transcript gets a notice with the arguments before and after, the approval and
  the tool's start line say `rewritten by a hook; the model asked with ...`, and the model is
  told its call was rewritten. A prompt rewrite is shown the same way (before and after) and the
  rewritten text is what the model reads.
- **Trust.** Only hooks from the user's own configuration may rewrite. A plugin an agent
  installed may still block, but its rewrites are ignored (and logged); so are rewrites from an
  event that is not `tool.before` or `prompt.submit`, an `updated_input` that is not an object,
  and one over 1 MiB. A repository's hooks never run ([stage 1](#hooks)). A plugin put in the
  config folder by hand (no install record) counts as the user's, as its hooks already run
  commands as the user.
- A hook that fails or times out leaves the call as it was.
- **Where a rewriting plugin may live.** `<config>/plugins` and the user's own
  `extensions.plugin_dirs`. An environment's configuration can add plugin folders too, but a
  plugin found only there may block and never rewrite. A `plugin_dirs` entry of the user's own
  **inside a repository a writer agent can modify gives that agent a rewriting plugin**: put
  those folders outside your projects. The safety floor makes a file tool's write into
  `<config>/plugins`, those folders and `<config>/mods` ask first (the folders are guarded by
  their resolved path as well as the one given, so a link or junction does not escape it);
  commands are read, not path-checked, so this is a guard against mistakes, not a boundary.

### Trusted main-context mods (trust implemented, loader not built)

A mod may ask, in its `mod.json`, to run in the app's own window instead of a sandboxed frame
(`"main_context": true`), for speed or to extend the app itself. That is the one thing a
sandbox exists to stop, so the manifest alone never grants it:

- It is granted by `imperium mods install` or `update`. When the mod asks, the command prints
  the warning below, naming the content hash, and checks that it is running at a terminal and
  that the user **types** `trust <id>`; `--yes`, a missing terminal, or any other answer
  installs the mod in its sandbox. It is separate from the permission prompt and no flag
  answers it.
- **What enforces that.** The terminal check and the typed answer are made by the CLI, which
  tells the daemon (`mods.install` with `trusted`). The daemon does not take its word: it grants
  trust only to a caller [it can tell is the user's own process](#who-may-grant-trust-and-approval-implemented).
- The confirmation is bound to the content hash and is kept in the daemon's approvals
  ([above](#installing-and-updating-mods-implemented)), not the mod's folder. A new version
  (`mods update`) asks again and is sandboxed unless the user types it again; a modified mod
  loses it (it is `modified`, with no permissions, and off); a mod without an approval record
  (copied in by hand, or installed by an agent's `install_extension`) is not trusted. An
  installed mod must sit in the folder named for its manifest id (`mods/<id>`, where
  `imperium mods install` puts it); another folder's mod is not listed, with a problem saying why
  and to rename the folder to the id, so it cannot ride on what an id earned, and the daemon
  still reads `mods/<id>` once for the hash and for the source it lists (a listing that is not
  that folder's is `modified`). **Migration:** a mod you copied in by hand under another folder
  name stops being listed; rename its folder to its `id` (mods installed by `imperium mods
  install` or an agent's `install_extension` already are).
  `mods.list` reports `main_context` (what the manifest asks) and `trusted`
  (what the user confirmed for these files, checked intact). A mod whose update newly asks for
  `main_context` is installed sandboxed: trusting it takes a fresh install.
- It is revocable: turning the mod off in Settings → Mods stops it, and installing it afresh
  without confirming returns it to the sandbox. The app's gate is `mayRunInMainContext`
  (`apps/desktop/src/settings/modTrust.ts`): asked, trusted, intact, and on.
- The warning says, in plain words: it runs inside the app's own window, where it can read and
  change everything the app can, including the daemon connection and its token, and act as
  the user; trust it only after reading its code; the confirmation is for exactly this content;
  a new version asks again.
- **Not built, and why:** the desktop loader that actually runs a trusted mod in the app's own
  window. The app's content security policy is `default-src 'self'` with no `script-src`, so a
  main-window script must come from the app's own origin. Loading a mod's module needs either a
  `script-src` that names the `imperium-mod:` protocol (which would let every mod's page run
  scripts in the main window unless that handler, in `src-tauri`, refuses all but a trusted mod
  whose files still hash to the trusted content), or a per-mod nonce or hash. Either is a change
  to the one policy that keeps untrusted code out of the main window, and whether it holds
  (the protocol handler's checks, the frame's own policy, `frame-src`) can only be seen in a
  running app, which this work could not launch. It stays the next step: build the serving
  check first, then the policy scoped to it. Until then every mod, trusted or not, stays in its
  sandboxed frame, and `trusted` has no effect beyond being recorded and shown. The daemon-side
  check that only the user can grant it, which had to come first, is below.

### Who may grant trust and approval (implemented)

`trusted` and `interactive` on `mods.install` are claims that a person answered at a terminal.
The daemon cannot see the terminal, so it does not rely on the claim: it checks **who is
asking**, using what the operating system says about the caller.

- **Chosen: the caller's process, not a daemon-owned prompt.** A prompt the daemon puts to the
  user would be answered over the same protocol (or shown on a screen) that anything holding
  the daemon's token can use, so it would only move the question to who may answer. What the
  daemon can observe about a caller is its process.
- On the local socket, the daemon learns the caller's process id from the operating system
  (`SO_PEERCRED` on Unix; `GetNamedPipeClientProcessId` on Windows). A grant is counted only if
  that process (1) is neither the daemon nor a **descendant** of it (the parent chain is
  walked), and (2) does not carry the `IMPERIUM_SPAWNED_BY_DAEMON` mark in its environment,
  which the daemon sets on every shell it starts for a tool call, a background command or a
  hook, in the OS sandbox and the container runner too, and which a child keeps when it detaches
  from its parent (an orphan has no daemon in its chain but keeps its environment). The mark is
  checked on the peer and on every ancestor walked.
- **It fails closed.** The peer must still be in the process table, not a zombie or dead, and
  must have started no later than the moment its connection was accepted (so a reused process
  id is not mistaken for it); its environment must be readable and non-empty. An elevated
  terminal's environment is not readable by the daemon, so a grant from it is refused: run the
  command from a terminal of your own privilege. A missing parent ends the walk (on Windows a
  parent that has exited is no longer linked, so a chain can end early; the mark in the peer's
  own environment covers that). Any `sshd` among the peer and its ancestors is refused too.
- A connection over loopback TCP has no process to ask about and never makes a grant (the CLI
  tries the local socket first). A paired phone and a mod frame cannot call `mods.*` at all.
- **When the check fails:** `trusted` is an error (`trust was not granted`, nothing installed);
  `interactive` is dropped, the mod installs off, and the result's `notes` say so. The result
  carries `interactive` and `trusted` as counted, and the CLI reports those, not what it sent.
- **What it is not.** An agent's shell running the CLI does not give a grant; an agent that
  detaches a process and clears its environment can. This is not a boundary against one that
  is trying to evade it: it talks to the daemon some other way, or acts through the user's
  desktop session. SSH is refused when the CLI runs in an SSH session (`sshd` is among its
  ancestors, which covers a plain `ssh localhost <command>`) and from a socket SSH forwards to
  the daemon (the peer is then `sshd` itself); a grant needs the machine's own terminal. That
  check is easy to sidestep: `ssh localhost tmux new -d ...` (or `mosh`, or anything that
  leaves a process that no longer descends from `sshd`) starts a process with no `sshd`
  ancestor, and it is caught only if its environment still carries the mark. The check does not
  depend on the agent being unable to read the token. Isolated tool tiers, the safety floor and the per-agent permission profiles remain the
  controls for the rest.
- **Required before the main-window loader ships.** The loader must not be built on this check
  alone. It needs a stronger one first: either confinement the daemon can verify, such as a
  Windows job object with breakaway disallowed (and `IsProcessInJob` on the caller) or, on
  Linux, `PR_SET_CHILD_SUBREAPER` so detached descendants stay in the tree, or a confirmation
  that does not pass through the daemon's protocol at all: a **native, non-webview OS dialog
  shown by the desktop app**. The native dialog answers the objection to a daemon-owned prompt
  above, that anything holding the token could answer over the same protocol: it is a window of
  the desktop app's own process, so answering it takes input into that window, not a call to the
  daemon, and the app can show the content hash it is confirming. It is much harder to answer
  than a protocol prompt, not impossible: same-user input automation (`SendInput`, UI
  Automation, `xdotool`) can click a dialog on the user's desktop, unless the dialog runs on a
  secure desktop (Windows' UAC-style desktop, which other processes cannot reach) or checks that
  the input is real (for example, that it is not injected). The loader's precondition is one of
  those, not the dialog alone.

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
requested permissions. For UI mods the approval, the hash check and the update diff are built
([above](#installing-and-updating-mods-implemented)); the plugin manifest's permissions are not.
