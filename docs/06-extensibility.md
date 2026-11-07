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
