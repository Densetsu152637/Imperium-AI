# 08 — Agent policy

## Default policy pack

**Status:** packs load from folders listed in `workflow.packs` through the
[workflow contract](18-workflow-contract.md). With no packs listed, the bundled snapshot of
Agent-Custom-Workflow in `policy/agent-custom-workflow` (pinned commit in its `PINNED` file,
MIT, shipped beside the daemon) is used; `workflow.default_pack = false` turns that off, and
`workflow.enabled = false` turns off packs and project instruction files entirely. Root
personality selection is implemented ([Personalities](#personalities)), with Marcus Aurelius as
the default. Fetching updates and the diff-and-accept flow are later work.

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
4. Project instructions (`AGENTS.md`, `CLAUDE.md`, `.imperium/policy/`). The project folder's
   files are served by the workflow manifest. The `AGENTS.md` and `CLAUDE.md` in any other
   directory an agent opens, and in its parents up to the allowed folder's root, are loaded
   automatically the first time the agent's file tools touch that directory (nearest last, so
   it wins), and `@path` imports inside them and inside a pack's entry files are followed. See
   [instruction files met while exploring](18-workflow-contract.md#instruction-files-met-while-exploring)
   and [imports](18-workflow-contract.md#imports). They are workflow context: compaction keeps
   them verbatim, and `workflow.enabled = false` turns them off.
5. Session overrides.

Users can **disable** the default pack entirely, replace it with another pack (any Git
repository following the pack layout), or disable individual workflows. The settings UI shows
the effective policy and which layer each rule came from.

## Personalities

**Status:** implemented: selection, the default, local overrides, the Settings select, and
`personalities.list`. The editor for local personalities (creating, duplicating, previewing)
is later work; until then, put Markdown files in `<config>/personalities/` by hand.

- **Default:** Marcus Aurelius (`policy.personality = "marcus_aurelius"`, written in the
  built-in defaults). The pack's own rule ("there is no default personality"; a `Personality:`
  line in the user's instruction file) does not apply under Imperium: the setting is the
  selection. `none` (or an empty value) turns personalities off; an id that is not found
  falls back to the default (not to none), with a notice in the session and a warning in the
  daemon log.
- **Lookup**, first match wins: `<config>/personalities/<id>.md`, then each folder in
  `workflow.packs` (its `personalities/<id>.md`), then the bundled snapshot, then a copy of the
  default compiled into the daemon, so the default works offline and with no pack. Personalities
  load even when `workflow.default_pack = false` or `workflow.enabled = false`: those switches
  govern packs and project instruction files, not the personality lookup. A local file that
  exists but is unreadable or not UTF-8 is skipped with a warning (not silently): a notice in
  the session that selected it and a line in the daemon log; the Settings list shows the
  personality under its id and has no field for the warning.
  `marcus_aurelius.md` in the bundled snapshot comes from a newer upstream commit than the rest
  (recorded in its `PINNED` file).
- **Where it goes:** only the root agent's system prompt, after the reply rules
  ([response style](03-agent-engine.md#response-style)), with a short frame saying it sets tone only: the reply
  rules still hold, deliverables (code, commits, pull requests, documentation, tool inputs)
  stay plain, and warnings come first. Subagents, chunk agents, titles, and
  compaction summaries never get it.
- **When a change applies:** the daemon applies `policy.personality` at once (no restart), and
  each session reads it when it starts, so new sessions use the new choice. A running session
  keeps the personality it started with until it is resumed after a daemon restart, when it
  takes the current one; the conversation so far is kept, so the voice may shift mid-thread.
- **Picking one:** Settings → Personality lists every personality the daemon finds
  (`personalities.list`: id, name from the file's heading, source `local`, `pack`, or
  `builtin`, and whether a local one overrides), marks the default, and offers None. From the
  command line: `imperium config set policy.personality <id>`.
- Personalities come from two places: those shipped in the active pack (`personalities/`) and
  **local personalities** the user creates, stored in `<config>/personalities/<id>.md`
  (outside any repository, never uploaded).
- *Settings → Personalities* lists both, previews them, and has an editor for creating,
  duplicating (for example, start from a pack personality and adjust it), renaming, and
  deleting local personalities. Pack personalities are read-only; editing one saves a local
  copy.
- A local personality with the same id as a pack one takes precedence, and the list marks it as
  overriding.
- Selection is a setting (`policy.personality`), global (a per-project override is later work),
  and applies only to the root agent, as the pack's personality rules require.

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

The public, versioned contract for packs (manifest, front matter, triggers) and the context
retrieval that serves pack files to agents are defined in
[18 — Workflow contract](18-workflow-contract.md); the summary below predates it.

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
