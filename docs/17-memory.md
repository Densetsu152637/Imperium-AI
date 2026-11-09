# 17 — Memory

Agents remember what they learn across sessions: the user's preferences, a project's
conventions, lessons from past mistakes, and pointers to useful resources. Memory is plain,
editable files with a search index on top, scoped so that the right facts reach the right
agents and nothing private leaves the machine by accident.

**Implemented (phase 5, `imperium-memory`):** the three file scopes below, entries with front
matter, a regenerated `INDEX.md` per scope, term-based search (inverse document frequency
weighted by name, description, and body), and a redaction check that refuses entries that
look like keys or tokens. Every agent's instructions carry the index (within
`max_index_tokens`); agents have `memory_search` and `memory_read`, and the root also has
`memory_remember`, which follows the scope's write policy (`ask` shows the entry in a
permission prompt). With `recall`, the best matches for each message are placed before it in
a `<relevant-memories>` block marked as data (within `max_recall_tokens`), and the session
reports `memory_used` and `memory_saved` events. `imperium memory list/search/show/forget`
works on the folders directly. Earlier sessions are searchable too, by clients and agents
([session search](#session-search-implemented)), on retrieval primitives memory search shares
(`imperium_memory::retrieval`: one tokenizer, the embedding interface, vector similarity, rank
fusion, and snippets). Not yet: embeddings for memory entries, `[[links]]` traversal,
stale-reference checks, consolidation, import/export, `memory.cloud`, the desktop Memory panel,
and the `memory.*` protocol methods.

## Scopes

| Scope              | Holds                                                         | Stored in                                         | Shared by           |
| ------------------ | ------------------------------------------------------------- | ------------------------------------------------- | ------------------- |
| **Global**         | Facts about the user and how they like to work, across every project | `<data>/memory/global/`                    | Every project       |
| **Project (shared)** | Project conventions, architecture notes, decisions worth reusing | `<project>/.imperium/memory/` (can be committed) | Everyone who clones the project |
| **Project (private)** | The user's own notes about a project, never committed       | `<data>/memory/projects/<project-id>/`            | This user only      |
| **Session**        | Working memory for one session: scratch notes and the plan    | The session journal                               | That session's agents |

Projects are environments ([14](14-projects-and-automation.md)), so "environment memory" and
project memory are the same thing. On an SSH host, project memory lives on that host; global
memory stays on the local machine unless the user enables syncing it to the host.

## Memory entries

One fact per Markdown file with front matter, the same shape Claude Code and the
Agent-Custom-Workflow learning lessons use, so existing memories import without conversion:

```markdown
---
name: prefers-pnpm
description: The user uses pnpm, never npm or yarn, in every JavaScript project
type: user            # user | feedback | project | reference | lesson
scope: global         # global | project | project-private
created: 2026-11-07
source: session 23e7804d62f7443a
confidence: confirmed # confirmed (the user said so) | inferred (an agent concluded it)
---

Use pnpm for installs and scripts. **Why:** the user's workspaces rely on pnpm workspaces.
**How to apply:** suggest `pnpm add`, never `npm install`. Related: [[node-version]].
```

- **Types:** `user` (who the user is and their preferences), `feedback` (corrections and
  confirmed approaches, with the reason), `project` (goals and constraints not derivable from
  the code), `reference` (pointers to external resources), and `lesson` (a failure and how to
  avoid it, compatible with the policy pack's learning lessons).
- **Links:** `[[name]]` links between entries form a small graph that retrieval can follow.
- **Index:** each scope has an `INDEX.md`, one line per entry, regenerated automatically.

## How agents use memory

1. **Always in context (small):** the index lines of the global and project scopes, within a
   token budget (`memory.max_index_tokens`). The index tells the agent what exists.
2. **On demand:** agents call `memory.search` (query, scope, type) and `memory.read` (name).
   Search is hybrid: full-text (SQLite FTS5) plus vector similarity when an embedding model is
   configured (a local embedding model through llama.cpp by default, see
   [04](04-models-and-routing.md)), then follows `[[links]]` one hop.
3. **Automatic recall (optional):** before each turn, the top matches for the user's message
   are injected as a short "relevant memories" block, capped by `memory.max_recall_tokens`.
4. **Stale facts:** a memory that names a file, function, or flag is checked before use; if the
   reference no longer exists the agent is told the memory may be out of date.
5. **Attribution:** the session shows which memories were used in a turn (`memory_used`
   events), so the user can see why an agent behaved as it did.

Subagents can search and read memory in their parent's scopes; they propose new memories
through their parent rather than writing directly.

## Session search (implemented)

What was said and done in earlier sessions is a retrieval source of its own: the desktop's
search box ([07](07-desktop-ui.md#session-search-implemented)) and the agents'
`search_sessions` tool both use it. It is built in `imperium_memory::sessions` on the same
retrieval layer as memory search, and the daemon keeps it current.

- **What is indexed:** every session's journal (`<data>/sessions/<id>.jsonl`), running and
  archived, split into chunks: the user's messages (and steering messages), the root agent's
  replies (split about every 2,000 characters), tool calls by tool and summary (which name the
  files and commands they touched), subagent briefs and follow-ups, and subagent reports. Each
  chunk keeps the index of the history event it starts at (as `session.subscribe` counts them)
  and when it happened; the session's title is matched too.
- **Incremental and bounded:** a background pass, a few seconds after the daemon starts and then
  every 15 seconds, reads only what each journal gained since the last pass (at most 4 MiB per
  session and 32 MiB per pass, on a blocking thread, yielding between journals), so a large
  backlog is indexed over several passes and turns never wait for it. A search first reads up to
  2 MiB of anything new, so what was just said is found. A reply still being written is
  searchable before it ends. Deleted sessions leave the index; archiving or moving a session is
  picked up on the next pass.
- **Storage:** `<data>/index/sessions/`, per session `<id>.state.json` (where reading stopped),
  `<id>.chunks.jsonl` (append-only chunks), and `<id>.vec` (append-only vectors). A session whose
  files are missing, do not parse, disagree with each other, or come from another index version
  is indexed again from its journal; damaged vectors alone are dropped and made again. A journal
  that became shorter (replaced) is read again from the start.
- **Ranking:** BM25 over the chunks' words (a user message weighs a little more and a tool call
  a little less than a reply; chunks with more of the query's words rank higher), plus the title.
  When `memory.embedding_model` names an embedding model on an OpenAI-compatible endpoint
  (`POST <base_url>/embeddings`), messages, replies, briefs, and reports (not tool calls) are
  embedded in batches of 16 (at most four per pass), the query is embedded at search time (5
  second limit), and the keyword and similarity rankings are fused by reciprocal rank. Vectors
  are tied to the model that made them; changing the model embeds again. Without a model, or
  when it fails, search is keyword-only. Local models cannot embed yet (they are served through
  chat slots).
- **Results:** the best chunk per session, best first, with an excerpt around the first match,
  the matched words as UTF-16 ranges, what matched, when, the event to open at, how many more
  chunks matched, and whether it was found by meaning only.
- **Agents (`search_sessions`):** offered to every agent while `memory.session_search` is on; it
  reaches the sessions of the agent's own environment (outside any environment, sessions in the
  same folder), never the searching session itself, and every environment only when
  `memory.session_search_scope` is `all`. It returns up to 5 sessions (at most 20) as excerpts
  marked as data from past conversations, not instructions. It needs no approval: it only reads
  what the environment's own sessions recorded.
- **Clients (`session.search`):** `query`, optional `environment_id`, `archived` (`true` for
  archived only, `false` for running only), and `limit` (20 by default, at most 100); the result
  says whether vectors took part (`semantic`).

```toml
[memory]
session_search = true                 # index sessions and offer search_sessions
session_search_scope = "environment"  # environment | all: what agents may search
embedding_model = ""                  # e.g. "openai:text-embedding-3-small"; empty: keywords only
```

## Project documentation retrieval

Environment memory also covers what a project already documents. Each project keeps a
**documentation index** so agents working on a specific task get pointers to the existing
documentation that bears on it, instead of rediscovering or contradicting it.

- **Sources:** `README*`, `docs/**`, decision records, `AGENTS.md`/`CLAUDE.md`, changelogs,
  and configured globs, in the project and in linked repositories. Code itself is not
  indexed here; agents search code with their file tools.
- **Chunks:** documents are split by heading; each chunk keeps its path, heading anchor, a
  short summary, and the code paths, symbols, commands, and config keys it mentions.
- **Search:** the same hybrid engine as memory and workflows (full-text plus optional local
  embeddings, see [18](18-workflow-contract.md#context-retrieval)).
- **Triggers, not just similarity:** a chunk that mentions a path the agent is about to edit, a
  command it is about to run, or a config key it is changing is served even when the request's
  wording differs. For example, an agent editing `scripts/ci/build-desktop.sh` is pointed
  at the CI/CD document's platform-bundles section.
- **Pointers, then loading:** the retriever serves references with a reason and a summary
  (`docs/11-ci-cd.md#platform-bundles`: "describes the bundles this script builds"); the agent loads
  the ones it needs with `context.load`. Documentation is served as data, not instructions.
- **Freshness:** the index updates when files change (a file watcher, and after each commit or
  pull). Chunks whose mentioned paths no longer exist are flagged as possibly stale, and the
  agent can propose a documentation fix.
- **Keeping docs current:** when an agent changes code that indexed documentation describes,
  the retriever lists those chunks at the end of its turn so the agent (or the policy's
  documentation rule) updates them in the same change.

```toml
[memory.docs]
enabled = true
include = ["README*", "docs/**", "**/*.md"]
exclude = ["node_modules/**", "target/**", "**/CHANGELOG.md"]
linked_repos = []              # extra folders or Git URLs to index read-only
max_pointer_tokens = 800       # budget for documentation pointers per manifest
```

## How memory is written

- Tools: `memory.remember` (new entry), `memory.update`, `memory.forget`.
- **Write policy per scope** (`memory.write`): `auto` (save immediately), `ask` (default: the
  agent proposes, the user approves in the desktop or phone app, with the entry shown), or
  `off`.
- **What not to save:** facts the repository or its history already records, conversation-only
  details, and secrets. A redaction pass rejects entries that look like keys, tokens, or
  passwords.
- **De-duplication:** before saving, similar entries are found; the agent updates the existing
  one instead of adding a near-copy.
- **Corrections:** when the user corrects an agent, it proposes a `feedback` memory with the
  reason, so the same mistake is not repeated.

## Maintenance

- A **consolidation** job (a built-in schedule, weekly by default, or on demand) merges
  duplicates, fixes contradictions, marks references that no longer resolve, and prunes the
  index. It runs as a normal session whose changes are proposed, not applied silently.
- Entries are plain files, so users can edit, delete, review in Git (project scope), or sync
  them with their own tools.
- **Import/export:** import from Claude Code memory folders, `AGENTS.md`/`CLAUDE.md` notes, and
  policy-pack learning lessons; export a scope as a folder or archive.

## Privacy

- Global and private project memory never leave the machine except inside prompts.
- `memory.cloud` controls what reaches **cloud** models (see the model table's `location` in
  [04](04-models-and-routing.md#routing)): `allow` (default), `index-only` (names and
  descriptions, not bodies), or `deny` (memory is offered only to local models).
- Individual entries can be marked `share: local-only` to keep them away from cloud models
  regardless of the setting.

## Interfaces

| Surface   | Memory support                                                                      |
| --------- | ----------------------------------------------------------------------------------- |
| Protocol  | `memory.list`, `memory.search`, `memory.get`, `memory.put`, `memory.delete`, `memory.approve`; events `memory_proposed`, `memory_saved`, `memory_used` |
| Desktop   | A Memory panel (browse by scope and type, search, edit, approve proposals) and a "used N memories" marker on turns |
| CLI       | `imperium memory list/search/show/edit/forget/import`                                |
| Hooks     | `memory.proposed`, `memory.saved` events, so plugins can sync or review memories      |
| Policy    | The policy pack's learning lessons are `lesson` memories; packs can ship seed memories |

## Configuration

```toml
[memory]
enabled = true
write = { global = "ask", project = "ask", project_private = "auto" }
recall = true                 # inject top matches before each turn
max_index_tokens = 1500
max_recall_tokens = 1500
embedding_model = ""          # a model table id or <endpoint>:<model>; empty means keyword search only
cloud = "allow"               # allow | index-only | deny
consolidate = "weekly"        # weekly | monthly | off
```
