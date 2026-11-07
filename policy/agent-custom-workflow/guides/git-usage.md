# Git usage

## Scope and preflight

Load this core before local edits or Git mutations. Read-only work needs none of it. Load [worktrees and integration](git-worktrees.md) for parallel write streams, integration or worktree cleanup; load [publication](git-publication.md) for pushing, PR/merge operations and root session-end publication. Publication beyond the [root session-end push](git-publication.md#root-session-end-push), merging, and contacting reviewers need separate task authorization.

Before edits or Git mutations:

1. Confirm the repository and checkout with `git rev-parse --show-toplevel`, `git status --short --branch`, and `git worktree list --porcelain`.
2. Identify the intended base, current branch, active worktrees, and existing staged, unstaged, and untracked changes; do not assume a clean checkout or that the current branch is the requested base.
3. Preserve changes owned by the user or other agents: do not reset, clean, stash, overwrite, switch their branch, or rewrite their commits.
4. If the folder is not a repository, report that and do the file work; do not initialize a repository unless requested.

## Branches and commits

| Change         | Branch prefix | Commit type               |
| -------------- | ------------- | ------------------------- |
| Feature        | `feat/`       | `feat`                    |
| Fix            | `fix/`        | `fix`                     |
| Temporary work | `temp/`       | `temp` (local convention) |
| Maintenance    | `chore/`      | `chore`                   |
| Documentation  | `docs/`       | `docs`                    |
| Formatting     | `style/`      | `style`                   |
| Refactor       | `refactor/`   | `refactor`                |
| Tests          | `test/`       | `test`                    |
| CI             | `ci/`         | `ci`                      |

- Branch suffix is short and descriptive, for example `feat/login-page`. Follow a required host prefix; do not rename an existing user branch to fit this convention.
- Commit format: `<type>(<optional-scope>): <imperative summary>`. Mark breaking changes with `!` and describe the impact in the body.
- Stage explicit owned paths or reviewed hunks, never blanket-add in a dirty shared checkout; run `git diff --cached --check` before committing. Never include someone else's staged changes.
- Commit only on the assigned authorized branch under [commit before ending work](#commit-before-ending-work). Correct unpublished task-owned commits when needed; do not rewrite shared history or force-push without explicit authorization (if authorized, use a lease and verify the expected remote state).

## Commit before ending work

Every agent, including root, managers, and leaves, must commit its intended task-owned repository changes (including docs and tests) before normal completion, termination, handoff, replacement, or cancellation when it controls that transition. Read-only work needs no commit; no empty commits. Explicit user no-commit instructions and higher-priority restrictions take precedence. Non-Git work follows [scope and preflight](#scope-and-preflight).

Authorization to work in an assigned checkout includes scoped local commits on its assigned branch and root's [session-end push](git-publication.md#root-session-end-push). Non-root agents need explicit push authorization or the [starting-on-main authorization](git-publication.md#starting-on-main-authorization); merging and committing on unrelated branches remain separately authorized.

1. Review the owned changes and run checks under [validation](validation.md#shared-validation-rules). Stage only owned paths or hunks. If existing staging cannot be safely separated, stop the commit and report the ownership blocker.
2. Inspect the staged diff, run `git diff --cached --check`, and make a focused commit under [branches and commits](#branches-and-commits). Before a controlled interruption of incomplete work, make a recoverable checkpoint commit where possible, labelled incomplete with validation limitations; it does not establish acceptance.
3. Verify the commit SHA and remaining status. Commit intended residual changes; report the SHA, validation evidence, and residuals with ownership and reason through the [result contract](subagent-coordination.md#assignment-and-result-contracts).
4. If identity, hooks, permissions, conflicting ownership, or another blocker prevents committing, preserve the work and report the blocker and recovery owner. Do not bypass hooks or permissions, change Git configuration to force a commit, or claim finalization succeeded.
