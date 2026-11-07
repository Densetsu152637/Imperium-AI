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

## Personalities

- Personalities come from two places: those shipped in the active pack (`personalities/`) and
  **local personalities** the user creates, stored in `<config>/personalities/<id>.md`
  (outside any repository, never uploaded).
- *Settings → Personalities* lists both, previews them, and has an editor for creating,
  duplicating (for example, start from a pack personality and adjust it), renaming, and
  deleting local personalities. Pack personalities are read-only; editing one saves a local
  copy.
- A local personality with the same id as a pack one takes precedence, and the list marks it as
  overriding.
- Selection is a setting (`policy.personality`), global with an optional project override, and
  applies only to the root agent, as the pack's personality rules require.

## Local paths come from app settings

Agent-Custom-Workflow reads per-user machine values from its gitignored `local-paths/` files.
Under Imperium those values come from **Imperium's configuration instead**, so there is one
place to set them:

| `local-paths` file  | Imperium setting                                                 |
| ------------------- | ---------------------------------------------------------------- |
| `worktrees.md`      | `worktrees.location` / `worktrees.custom_path` ([14](14-projects-and-automation.md#worktrees)) |
| `plan.md`           | `agents.max_concurrent` (the subagent cap)                       |
| `pool-managers.md`  | `agents.manager_pool` (how managers count toward the agent pool) |

- The pack manifest declares this mapping (`local_paths` below), and the Imperium host adapter
  tells agents to resolve these values through the app rather than reading files.
- The daemon injects the resolved values into the policy context; `local-paths/` files in the
  pack directory are ignored, and the policy view says so if any exist.
- A value missing from config falls back to the pack's default rule, as a missing file does
  today.
- Placeholders map across: `<repository-name>` → `{repo}`, `<task-id>` → `{task}`.

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

[local_paths]                       # pack files whose values come from Imperium settings
"worktrees.md" = "worktrees.custom_path"
"plan.md" = "agents.max_concurrent"
"pool-managers.md" = "agents.manager_pool"
```

Packs without a manifest are loaded by convention from `AGENTS.md`.
