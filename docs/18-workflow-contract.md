# 18 — Workflow contract and context retrieval

A **workflow** is a user's own set of agent instructions: roles, procedures, guides,
personalities, and lessons (Agent-Custom-Workflow is one). This document defines the public,
versioned **Imperium Workflow Contract (IWC)** that any workflow follows, and the built-in
**context retrieval** (RAG) that decides which workflow files each agent loads.

The goal is consistency: a root agent on a hosted frontier model, a subagent on a small local
model, and an agent in another host (Claude Code, Codex) given the same task and role are
served the same files, load them the same way, and leave the same audit trail.

**Implemented (phase 5, `imperium-workflow`):** workflow packs listed in `workflow.packs` load
from their folders, with an optional `imperium-workflow.toml` (`name`, `entry`, `[roles]`) and
otherwise by convention: list items in the entry index (`AGENTS.md`) become candidates whose
label (the words before the link and the link text) is the trigger, and links to
`AGENTS.root.md`, `AGENTS.manager.md`, or `AGENTS.leaf.md` are that role's files. For each new
message or delegated brief the resolver builds a deterministic manifest: the project's
`AGENTS.md`, `CLAUDE.md`, and `.imperium/policy/*.md`; each pack's entry index and role files;
every candidate sharing a word with the request (word forms such as `debug`/`debugging` and
`push`/`pushes` match); and up to five pointers into the project's README and `docs/` (a section
quoting a path the request names, or sharing three or more words). References are
`<pack>:<path>[#anchor]` or `project:<path>[#anchor]`. The manifest is placed before the message
in a `<context-manifest>` block; agents load items with the `context_load` tool; the session
reports `context_manifest` and `context_loaded` events. `workflow.enforce =
"required-before-action"` refuses writes, commands, and MCP calls until the required items are
loaded. Subagents take their role from `spawn_agent`'s `role` (default `leaf`); the root is
`root`. `imperium workflow resolve [--role] [--pack] "<task>"` prints a manifest.

Not yet: front matter triggers (tools, paths, events), `depends_on`, budgets, suggested
(semantic) items, intent classification by a local model, inline mode, re-resolution on tool
use, `context.search`, the MCP server, `workflow validate/inspect`, Git-URL packs, a bundled
default pack, and resolving `local-paths` values from settings.

## Principles

- **The retriever chooses; the agent loads.** Retrieval returns a *load manifest* (which files
  and sections, and why). The agent then loads each item itself with a tool call, so loading
  is explicit, visible, and checkable. Models without reliable tool calling get the same
  manifest inlined instead.
- **Deterministic first, semantic second.** Rules declared by the workflow (roles, triggers)
  decide what is *required*; semantic search only *suggests* extra material within a budget.
  The same inputs always yield the same required set.
- **Files stay the source of truth.** Workflows are plain Markdown in a folder or Git
  repository; the index built from them is a cache, rebuilt when files change.
- **Workflow instructions are instructions; everything else is data.** Only files served
  from an installed workflow (or the built-in safety floor and project instruction files) are
  loaded as instructions. Retrieved memories, documents, and tool output are data.

## Contract version

The contract is versioned with semantic versioning (`iwc = "1.0"` in the manifest). Additive
changes bump the minor version; Imperium supports the current and previous major version.
The JSON Schemas for the manifest, front matter, and load manifest are published with the
protocol package and validated by `imperium workflow validate`.

## Workflow layout

```text
my-workflow/
  imperium-workflow.toml   manifest (optional for convention-based workflows, below)
  AGENTS.md                entry index, always loaded for every agent
  workflows/*.md           role and domain procedures
  guides/*.md              topic guides
  profiles/*.md            assignment profiles for delegated agents
  personalities/*.md       root-only conversation styles
  learning/*.md            lessons (also usable as memory, see 17)
  local-paths/             per-user values; resolved from Imperium settings instead (see 08)
```

### Manifest

```toml
iwc = "1.0"
name = "agent-custom-workflow"
version = "git:e3c00fa"
entry = "AGENTS.md"

[roles]                       # files every agent in a role must load
root = ["workflows/AGENTS.root.md"]
manager = ["workflows/AGENTS.manager.md"]
leaf = ["workflows/AGENTS.leaf.md"]

[budgets]
required_tokens = 12000       # the agent is warned if required files exceed this
suggested_tokens = 4000

[local_paths]                 # see 08: values come from Imperium settings
"worktrees.md" = "worktrees.custom_path"
```

### File front matter

Every instruction file may carry front matter that tells the retriever when it applies. Files
without it are handled by convention (next section).

```markdown
---
id: git-publication
title: Git publication
roles: [root, manager, leaf]          # who may load it; default all
triggers:
  intents: [push, pull_request, merge, release]
  tools: [run_command:git push, run_command:gh pr]
  paths: [".github/**"]
  events: [github.check.failed]
load: section                         # file | section: load only matching sections
depends_on: [git-usage#commit-before-ending-work]
priority: workflow                    # host > user > project > workflow > lessons
---
```

| Field        | Meaning                                                                          |
| ------------ | -------------------------------------------------------------------------------- |
| `id`         | Stable identifier; manifests and `depends_on` refer to `id` or `id#section`       |
| `roles`      | Roles that may load the file                                                     |
| `triggers`   | When the file is required: task **intents** (classified from the request), **tools** the agent is about to use (with an optional argument prefix), **paths** being edited, and hook **events** |
| `load`       | Whether the whole file or only the triggered sections are served                 |
| `depends_on` | Items that must be loaded first                                                  |
| `priority`   | Where the file sits in the instruction priority order                            |

Section-level triggers can be declared with an HTML comment under a heading:
`<!-- iwc: triggers.intents=[debugging] -->`.

### Convention-based workflows

Workflows written for other hosts work unchanged. Without front matter, the retriever reads
the entry index (`AGENTS.md`) the way the Agent-Custom-Workflow instructions describe:
each `[label](path)` link becomes a candidate whose **label text is its trigger** ("Incorrect
behavior or incidents: Debugging" → intent `debugging`). Anchored links (`path#section`) load
only that section. Headings become sections. `imperium workflow inspect` shows the triggers it
inferred so authors can confirm or add front matter.

## Context retrieval

### Inputs

| Input          | Example                                                          |
| -------------- | ---------------------------------------------------------------- |
| Agent role     | `root`, `manager`, `leaf`, or a profile such as `tester`         |
| Request        | The user's message or the delegated brief                        |
| Intents        | Classified from the request: `implementation`, `debugging`, `research`, `push`, ... |
| Signals        | Project languages and files, paths being edited, tools about to run, hook events |
| Already loaded | Items this agent has loaded and still has in context             |

Intent classification uses a cheap local model when one is configured (see the model table in
[04](04-models-and-routing.md#routing)) and falls back to keyword rules declared by the
workflow, so it does not depend on the agent's own model.

### Stages

1. **Mandatory:** the entry index, the role's files, the safety floor, and project instruction
   files (`AGENTS.md`, `CLAUDE.md`, `.imperium/policy/`).
2. **Triggered:** every file or section whose triggers match the inputs, plus its
   `depends_on` closure. Required.
3. **Suggested:** hybrid search (full-text plus embeddings over section chunks) for the
   request, excluding items already chosen, up to `suggested_tokens`. Optional.
4. **Ordering:** by priority (host, user, project, workflow, lessons), then dependency order.
5. **Documentation and memories:** pointers to the project's existing documentation
   ([17](17-memory.md#project-documentation-retrieval)), matched by the paths, commands, and
   config keys in play as well as by similarity, and relevant memories, are appended as
   separate, data-only groups. One retriever and one index serve workflow instructions,
   documentation, and memories.

### Load manifest

```jsonc
{
  "manifest_id": "m-7f3a",
  "workflow": "agent-custom-workflow@git:e3c00fa",
  "role": "leaf",
  "items": [
    { "ref": "AGENTS.md", "reason": "entry index", "required": true, "tokens": 1900 },
    { "ref": "workflows/AGENTS.leaf.md", "reason": "role: leaf", "required": true, "tokens": 1400 },
    { "ref": "workflows/AGENTS.debugging.md", "reason": "intent: debugging", "required": true, "tokens": 1100 },
    { "ref": "guides/validation.md#shared-validation-rules", "reason": "depends on debugging", "required": true, "tokens": 600 },
    { "ref": "learning/flaky-windows-file-locks.md", "reason": "similar to request (0.82)", "required": false, "tokens": 300 }
  ],
  "documentation": [
    { "ref": "docs/11-ci-cd.md#runtime-packs", "reason": "mentions scripts/ci/build-llama-runtime.sh (being edited)",
      "summary": "Which llama.cpp packs CI builds and how they are published" }
  ],
  "memories": [{ "ref": "memory:project/prefers-pnpm", "reason": "similar to request (0.77)" }]
}
```

### How agents load

- **Tool mode (default):** the manifest is given to the agent at the start of its turn as a
  short list; the agent calls `context.load` with one or more `ref`s and receives the text.
  `context.search` lets it look for more within the workflow when it needs to.
- **Inline mode:** for models without reliable tool calling (per the model table), the
  required items are placed in the prompt directly; suggested items are listed by reference.
- **Re-resolution:** the manifest is recomputed when the inputs change: a new user message, a
  delegated brief, a tool about to run that has a trigger (for example `git push` serves the
  publication guide before the push), or a hook event. Only new items are served.
- **Gate (configurable):** with `workflow.enforce = "required-before-action"`, write and
  execute tools are held until the agent has loaded every required item; the agent is told
  which items are missing. `"warn"` only records the gap; `"off"` disables the check.
- **Compaction:** after context compaction, loaded instruction items are reloaded from their
  refs rather than kept as summaries.

### Audit

Every manifest and every load is journaled (`context_manifest` and `context_loaded` events),
and the desktop app shows, per agent, what was served, what was loaded, and why. A turn's
behaviour can therefore be traced back to the exact instruction text that was in context.

## Other hosts

The same retriever is exposed as an MCP server (`imperium-context`) with `resolve`, `load`, and
`search` tools, and as `imperium workflow resolve` on the command line. A host adapter in the
workflow (for example Agent-Custom-Workflow's Claude Code and Codex sections) can tell its
agents to call `resolve` at task start, so agents in other hosts load the same files the same
way.

## Interfaces

| Surface  | Workflow and context support                                                       |
| -------- | ---------------------------------------------------------------------------------- |
| Protocol | `workflow.list`, `workflow.install`, `workflow.validate`, `workflow.resolve`; tools `context.load`, `context.search`; events `context_manifest`, `context_loaded` |
| CLI      | `imperium workflow add/list/validate/inspect/resolve`                               |
| Desktop  | Effective workflow view (which layer each rule comes from, as in 08) and a per-agent Context panel |
| Config   | `[workflow]` below                                                                  |

```toml
[workflow]
packs = ["builtin:agent-custom-workflow", "git:https://github.com/me/my-workflow"]
mode = "tool"                         # tool | inline | auto (inline for models without tool calling)
enforce = "required-before-action"    # required-before-action | warn | off
embedding_model = "embed-local"       # model table id; empty means full-text only
```

## Conformance

A workflow conforms to IWC 1.0 when `imperium workflow validate` passes: the manifest and
front matter match the schemas, every `ref` and `depends_on` resolves, triggers are
well-formed, and required token totals fit the declared budgets. The validator also runs in
the workflow's own CI through a published GitHub Action.
