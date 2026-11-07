# 08 — Agent policy

## Default policy pack

Imperium ships with [Agent-Custom-Workflow](https://github.com/Densetsu152637/Agent-Custom-Workflow)
as its default **policy pack**: the root/manager/leaf roles, delegation defaults, validation,
git, research, debugging, and other workflows, plus personalities.

- A snapshot is bundled in `policy/` at a pinned commit so the app works offline.
- Updates are fetched from the upstream repository on request (or on a schedule), shown as a
  diff, and applied only after the user accepts. The pinned commit is recorded per session.
- The pack is MIT-licensed upstream.
- Upstream adds an **Imperium host adapter** section alongside its Claude Code and Codex
  adapters, mapping its concepts to Imperium tools (`spawn_agent`, `await_tasks`, routing
  hints, telemetry).

## How a pack maps onto the engine

| Pack concept                    | Imperium mechanism                                              |
| ------------------------------- | --------------------------------------------------------------- |
| `AGENTS.md` index and companions | Loaded on demand like skills: the index is always in context, companions load when their trigger applies |
| Root / manager / leaf roles     | Agent roles; each spawned agent receives its role's instructions |
| Profiles                        | Task templates with default requirements and budgets            |
| Personalities                   | Root-only persona, selectable in settings                       |
| Host adapter                    | Tool-name and capability mapping table                          |
| Learning lessons                | Optional memory entries injected for matching tasks             |

## Layering and overrides

Effective policy is assembled from layers, later layers overriding earlier ones:

1. Built-in safety floor (cannot be disabled: permission prompts for destructive actions,
   secret handling).
2. Default pack (bundled or updated).
3. User pack overrides (`<config>/policy/`), including a full fork via a Git URL.
4. Project instructions (`AGENTS.md`, `CLAUDE.md`, `.imperium/policy/`).
5. Session overrides.

Users can **disable** the default pack entirely, replace it with another pack (any Git
repository following the pack layout), or disable individual workflows. The settings UI shows
the effective policy and which layer each rule came from.

## Pack format

A pack is a directory or Git repository with an `imperium-pack.toml`:

```toml
name = "agent-custom-workflow"
version = "git:<commit>"
entry = "AGENTS.md"                 # always-loaded index
roles = { root = "workflows/AGENTS.root.md", manager = "workflows/AGENTS.manager.md",
          leaf = "workflows/AGENTS.leaf.md" }
personalities = "personalities/"
adapter = "guides/host-adapters.md#imperium"
```

Packs without a manifest are loaded by convention from `AGENTS.md`.
