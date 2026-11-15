# 08 — Agent policy

## Default policy pack

**Status:** packs load from folders listed in `workflow.packs` through the
[workflow contract](18-workflow-contract.md). With no packs listed, the bundled snapshot of
Agent-Custom-Workflow in `policy/agent-custom-workflow` (pinned commit in its `PINNED` file,
MIT, shipped beside the daemon) is used; `workflow.default_pack = false` turns that off, and
`workflow.enabled = false` turns off packs and project instruction files entirely. Root
personality selection is implemented ([Personalities](#personalities)), with Marcus Aurelius as
the default. Fetching updates and the diff-and-accept flow are implemented ([Pack updates](#pack-updates));
the policy crate that layers packs into the prompt is later work.

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

## Pack updates

**Status:** implemented (roadmap 31, stage 3a), as the `imperium pack` command. It needs no
daemon and no protocol change: sources are read from the user's config folder, pins are files in
the data folder, and a session reads the pins when it starts.

- **Sources are the user's own.** A source is a name and a Git URL (`https://`, `ssh://`,
  `git@host:path` or `file://`; never `http://`, never credentials in the address, never an
  `ext::` helper, never a host that starts with `-`), optionally a branch or tag. A source name
  may not be `project` or the name of a pack listed in `workflow.packs` (references are
  `<name>:<path>`). They live only in `<config>/pack-sources.toml`,
  added with `imperium pack add-source <name> <url> [--revision <ref>]`. A project's files
  (`.imperium/`), environment variables, a pack's own manifest and an agent's tool calls
  cannot add or change one: the command reads that one file and nothing else. The pack store
  (`<data>/packs/`) is on the safety floor's protected list, so an agent's write there is
  always asked about, and so is an agent running `imperium pack add-source`, `accept` or
  `remove-source` (a person decides those, as for `imperium mods install`); `check`, `diff`,
  `list` and `reject` change nothing in force and are not asked about.
- **Fetching never applies anything.** `imperium pack check [name]` shallow-clones the source
  without a checkout and without blobs over 2 MiB, with git hooks off, no prompts (ssh runs in
  batch mode through `GIT_SSH_COMMAND`, which takes the place of a `core.sshCommand` of yours), no LFS downloads, no submodules, the `ext::` transport refused and the caller's
  `GIT_*` environment removed (except `GIT_SSL_CAINFO` and `GIT_SSL_CAPATH`). Before any file is written it refuses a source with more than
  2000 files or any file over 2 MiB; then it checks out, reads the files (leaving out `.git`
  and anything that is not a regular file; at most 32 MiB in all) and stages them as a
  **pending update** next to the pinned version. Sessions keep using the pinned version. The
  clone itself is bounded by a five-minute timeout, not by size: a server that ignores the
  filter can still send a large history before the limits are checked.
- **The diff is in plain words.** Files are grouped as *rules and workflows* (the entry index,
  workflows, guides), *prompts and personalities*, *lessons*, *pack settings* (the manifest:
  which files each role must load, which of your settings a pack file is filled from) and
  other files, each marked added, removed or changed with its line counts.
  `imperium pack diff <name> --full` lists every changed line. A first version has nothing to
  compare with, so it is shown as a full listing to read before accepting. When no
  comparison was possible (nothing pinned yet, or the pinned files failed their check),
  `check`, `diff` and `accept` say so in capitals, say that nothing can be flagged as
  loosening, and, for the first pack pinned, say that the bundled snapshot stops loading.
- **An update cannot loosen quietly.** Changes that may reduce what agents are required or
  allowed to do are listed first, marked `!`, and accepting an update that has any needs
  `--allow-loosening` (or the longer phrase typed at a terminal) on top of the usual
  confirmation. They are: a role's instruction file removed; a file dropped from a role's
  required list; the entry index changed; a pack file newly filled from one of your settings;
  an unreadable manifest; wording added that reduces asking or checking ("without asking",
  "bypass", "force push" and similar); and a line removed that made agents ask or hold back
  ("ask before", "explicit permission", "must not" and similar). The wording checks run on
  every changed file except the manifest (which is read structurally): rules, prompts, lessons,
  local-paths files and any other kind, whether or not the manifest or the index references
  them. They are a floor for review, not a parser of meaning: they can miss a rewrite and they
  flag harmless edits, which is why the full diff is always available and why they never
  decide for the user. Tightening (a newly required file) is shown as a note. The screen shows
  the first 50 loosening items and 20 notes; the accept gate reads every finding, loosening
  ones first, however many there are.
- **Accepting is explicit and bound to what was read.** At a terminal, `imperium pack accept
  <name>` shows the diff and asks for `accept <hash12>` (`accept <hash12> loosening` when
  loosening is present). Without a terminal it needs `--hash <at least 12 digits>`, and the hash
  must match the staged version, so an update that changed since `check` is not accepted by
  mistake. `imperium pack reject <name>` discards it. A pack that does not load as a pack (no
  entry index, an unreadable manifest) is refused.
- **Pinned by content hash.** The pin records the source, the commit and the SHA-256 of the
  pack: every file path with the SHA-256 of its content, in path order. Starting a session
  checks the pinned files against that hash (the result is reused for two seconds, so one
  start hashes once) and skips the pack, with a notice, if they differ (a pack edited on disk
  is never loaded under an old pin). The check is at session start: a file changed on disk
  while a session is running is read as it then is, which the safety floor's protection of
  `<data>/packs` is there to ask about. A link or other non-regular file, or a `.git` entry,
  in a pinned or staged folder fails the check outright, since the hash would not cover what
  it points to; personality files of a pinned pack must also resolve inside the pack. Staged
  files are checked the same way before accepting. The previous pinned version's files are removed on accept; the commit
  is in Git.
- **How a pinned pack is used.** Like a folder in `workflow.packs`, except its `@` imports stay
  inside the pack (it is not trusted to reach other files) and its references are
  `<source name>:<path>`. With a pinned pack present the bundled snapshot is not also loaded.
  Pinned packs also supply personalities. Running sessions are unaffected until they restart.
- **Removing a source** (`imperium pack remove-source`) also removes its pin and files.

**Signatures: not supported, and why.** The pack format has no signature field, and verifying
one needs a signing scheme and key distribution that Imperium does not have (and a new
dependency). What protects an update is the transport (`https://` or `ssh://`, so a fetch is
authenticated to the host), Git's own object hashes for the commit, the user's review of the
diff, the explicit hash-bound accept, and the SHA-256 pin re-checked at every load. A pack
author who wants detached signatures can add them later (Ed25519 over the content hash, public
keys in the source entry); until then trust in a source is the user's choice and the pin makes
any later change visible.

**What a pack can and cannot do.** A pack is instructions: it ranks below the host's and the
user's instructions and cannot widen the permission rules, the safety floor or the sandbox,
which Imperium enforces whatever an agent is told. "Loosening" above therefore means loosening
what the workflow asks of agents (asking, confirming, loading rules), which is what a changed
pack can do; it is highlighted because a weaker workflow is still a weaker guard.

**Not built:** scheduled or automatic checks (a check runs only when asked); a desktop screen
for the diff (the CLI is the review surface for now); rolling back to a previous pin (re-fetch
the old commit by pointing a new source at it); several sources layered by priority beyond
list order; detached signatures; pinning a commit instead of a branch.

**The policy crate.** `imperium-policy` ("pack layering and prompt assembly", planned in
[02](02-architecture.md)) is deferred to the final cleanup: layering lives in `imperium-core`'s
session setup and `imperium-workflow`, and moving it needs a stable interface (pack list in,
instruction blocks out) that is easiest to cut once the remaining roadmap items stop touching
those call sites. The [cleanup plan](plans/cleanup.md#decision-the-policy-crate) sets out its
scope and interface.

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
