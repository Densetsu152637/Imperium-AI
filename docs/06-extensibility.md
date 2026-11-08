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
  `skills/`; the first of a name wins. Every agent's instructions list each skill's
  description, and agents call `load_skill` (`name`, optional `file` for a resource inside the
  skill folder) to load one.
- **Slash commands:** Markdown templates in `<config>/commands/`, the project's
  `.imperium/commands/` and `.claude/commands/`, and plugins' `commands/` (also reachable as
  `/<plugin>:<name>`; subfolders give `/<folder>:<name>`). A message starting with
  `/<name> args` is replaced by the template with `$ARGUMENTS` and `$1`, `$2`, ... filled in.
- **Plugins:** folders with `.claude-plugin/plugin.json` in `<config>/plugins/` or
  `extensions.plugin_dirs` contribute skills, commands, `hooks/hooks.json`, and `.mcp.json`
  servers (named `<plugin>_<server>`), with `${CLAUDE_PLUGIN_ROOT}` expanded. `agents/` and
  `imperium-plugin.toml` are reported as not yet supported.
- **Hooks:** see below. Problems (a skill without a description, an unsupported hook event)
  are shown as session notices.

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
mods. `@imperium/mod-sdk` (`packages/mod-sdk`) provides `defineMod` and the context types;
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
with the same id wins. `mods.list` marks them `bundled`. **Bundled mods are opt-in:** their
panels show only after the user turns them on in Settings → Mods (installed mods are on until
turned off). That section also lists each mod's version, permissions, panels, and load problems.
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
