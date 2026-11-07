# Agent Custom Workflow

A host-neutral workflow for coding agents. It defines how a root agent plans and delegates, how managers direct descendants, and how leaves execute bounded work, using assignment profiles, domain workflows (research, debugging, security, browser verification and others), shared guides, and selectable root personalities. [`AGENTS.md`](AGENTS.md) is the entry point: it owns shared priority, discovery rules, and the index of everything else. Host-specific tool mappings live in [`guides/host-adapters.md`](guides/host-adapters.md).

## Setup

Point your host at this repository from a personal, user-level instruction file so the workflow applies in every project. Use absolute paths. Keep the personal file minimal and leave policy in this repository.

In the examples below, `<path-to-repo>` is wherever you cloned this repository (for example `D:/Git Repositories/Agent-Custom-Workflow`).

### Claude Code (CLI and desktop app)

Add an eager import of this repository's [`CLAUDE.md`](CLAUDE.md) to your personal `~/.claude/CLAUDE.md` (Windows: `C:\Users\<you>\.claude\CLAUDE.md`), using the absolute path with spaces escaped:

```markdown
@D:/Git\ Repositories/Agent-Custom-Workflow/CLAUDE.md

Personality: luxion.md
```

The `Personality:` line is optional; see [Choosing a personality](#choosing-a-personality). The repository `CLAUDE.md` imports `AGENTS.md` and points to the Claude Code host adapter. Imports are eager; companion links inside `AGENTS.md` are not, and are resolved relative to the file that contains them. Details, including how subagents inherit the instructions, are in the [Claude Code adapter](guides/host-adapters.md#claude-code).

### Link-reference alternative

If your personal instruction file or host does not support `@` imports, reference the entry point with a link instead:

```markdown
# Custom workflow

Read [AGENT-INSTRUCTIONS](D:/Git Repositories/Agent-Custom-Workflow/AGENTS.md) and assume its configuration.

Resolve companion links relative to the instruction file that contains them.

Personality: luxion.md
```

The `Personality:` line is optional here too. Links are followed lazily rather than imported, so the agent opens `AGENTS.md` and then each companion only when needed. The "Resolve companion links" line matters: without it the agent may resolve the relative links in `AGENTS.md` against your personal file's directory and fail to find them.

### Codex

Codex has no `@` import, so use the link-reference form in your personal `~/.codex/AGENTS.md` (Windows: `C:\Users\<you>\.codex\AGENTS.md`), pointing at this repository's [`AGENTS.md`](AGENTS.md) by absolute path:

```markdown
# Custom workflow

Read [AGENT-INSTRUCTIONS](D:/Git Repositories/Agent-Custom-Workflow/AGENTS.md) and assume its configuration.

Resolve companion links relative to the instruction file that contains them.

Personality: luxion.md
```

The `Personality:` line is optional; see [Choosing a personality](#choosing-a-personality). Configure subagents under `[agents]` in `~/.codex/config.toml`; briefs and manifests carry the role and profile, and subagents never load a personality. See the [Codex adapter](guides/host-adapters.md#codex).

### Local paths

Machine-specific paths, such as where worktrees go, live in the gitignored [`local-paths/`](local-paths/README.md) folder. Setup and supported files are in its README.

## Choosing a personality

The top-level root loads at most one conversational personality at intake. To select one, add an optional line to the same personal instruction file, naming a file from the [personality catalogue](personalities/README.md):

```markdown
Personality: luxion.md
```

With no valid selection (line missing or naming an unknown file) root uses no personality and a neutral tone, and tells the user in its first reply how to select one; the rules are in [personality selection](personalities/README.md#selection). A selection made in chat overrides the file for that session. Personalities affect only the root's conversational tone, never deliverables, and subagents never load them.

## Repository layout

- [`workflows/`](workflows): role workflows (root, manager, leaf) and domain workflows such as research, debugging, security, architecture, and browser verification.
- [`profiles/`](profiles/README.md): assignment profiles (explorer, implementer, tester, documenter, and others) that describe a leaf's execution focus.
- [`guides/`](guides/README.md): shared guidance for coordination, validation, coding standards, Git and worktrees, publication, environment configuration, and host adapters.
- [`personalities/`](personalities/README.md): selectable root conversation personalities and the selection rules.
- [`local-paths/`](local-paths/README.md): gitignored per-user machine paths the workflow resolves, such as the worktree location or plan tier.
- [`templates/`](templates): subagent brief and result templates, plus the failure-entry and staged-plan templates.
- [`learning/`](learning/README.md): maintenance rules and the catalogue of reusable failure lessons.

## Licence

[MIT](LICENSE)
