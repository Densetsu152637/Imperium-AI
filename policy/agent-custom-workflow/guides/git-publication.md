# Git publication

Load before publication, PR creation/update/merge, or root session-end publication. Establish starting-branch evidence during local preflight; switching branches later grants no starting-main authority.

## Starting-on-main authorization

An agent whose assigned checkout is already on `main` at the start of its work (root or subagent, including when `main` is the default branch) may push validated, committed work to `origin/main` without further permission. A subagent pushes under this authority only after its immediate manager records the work `accepted` and root's [review gates](../workflows/root-review.md#gates) have passed or do not apply, or when explicitly assigned the push. Normal fast-forward pushes only: explicit no-push instructions, required checks, branch protections, and higher-priority or host restrictions still apply.

## Root session-end push

When finishing a session with repository work, root must push all commits reachable from the branch being worked on to the same branch on `origin`, including commits that predate the current task. This is the default push authorization; explicit no-push instructions and higher-priority or host restrictions take precedence. Non-root agents return committed results for root to review and integrate unless explicitly assigned a push or covered by the [starting-on-main authorization](#starting-on-main-authorization).

1. Complete required integration, validation, reviews, task-owned commits, and worktree reconciliation. Confirm the checkout, branch, commit SHA, and `origin` URL under [scope and preflight](git-usage.md#scope-and-preflight); if the destination is unclear, do not choose another branch or remote.
2. Push only that branch with an explicit destination, for example `git push origin "HEAD:refs/heads/<branch>"` (replace `<branch>`). If upstream tracking is missing, add `--set-upstream` to this same push. Never use `--all`, `--mirror`, tag pushes, or force options for this default.
3. Verify the matching `origin` branch contains the final local commit; report branch, SHA, and outcome. A rejected or unverified push is not completion.
4. If a missing remote, detached HEAD, authentication, permissions, non-fast-forward rejection, protection, no-push instruction, or another blocker prevents the push, preserve local commits and report the blocker, unpushed SHA(s), and recovery owner. Do not bypass restrictions, rewrite shared history, or substitute another destination.

## Pull requests and merge rules

- When creating or changing workflow files, load [GitHub Actions guidance](github-actions.md).
- Use the repository's PR template when present; otherwise describe the problem, resulting behavior, context, validation, and remaining limitations.
- Required CI/CD checks, including configured GitHub Actions, must pass for the revision being merged. Local checks do not replace required CI; unavailable checks are unverified, not passed.
- Local act fallback: only when hosted GitHub Actions checks did not run or failed for a recorded cause outside the revision (runner outage, billing, quota), a merge or push may proceed if every required job passes in a [local act rehearsal](github-actions-local.md) of that exact revision. Record the revision, each job's local result and the hosted failure cause, and report that hosted CI did not pass. It never applies when the failure reproduces in act or when branch protection requires the hosted check. Branch protections, required reviews and explicit user instructions still apply.
- Require at least one human (user) approval and any other repository-mandated reviews. The author must not approve or merge their own PR. A subagent review is evidence, not repository approval.
- Merge only within the user's authorization and repository protections. Keep PR title and description aligned with the final diff. Do not message reviewers unless authorized.
- After confirmed merge, retire the task-owned worktree under [safe cleanup](git-worktrees.md#safe-cleanup), then delete its source branch when no longer needed under the cleanup order. Preserve branches still used by another stream; record anything deferred as `retained`. Worktree retirement also applies to preserved results that need no PR merge.
