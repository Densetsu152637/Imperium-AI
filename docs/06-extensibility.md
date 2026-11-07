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

## Hooks

Hook events: `session.start`, `session.end`, `prompt.submit`, `tool.before`, `tool.after`,
`agent.spawn`, `agent.complete`, `model.before`, `model.after`, `compact.before`,
`notification`.

- A hook is a command (any executable receiving JSON on stdin) or an HTTP endpoint.
- `tool.before` and `prompt.submit` hooks can allow, deny, or rewrite; others observe.
- Hooks run with timeouts and inside the session's isolation tier.

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

Contribution points: panels (any pane slot), status-bar items, message renderers (custom
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
