# Git-worktrees and integration

Load for parallel write streams, worktree creation/use, integration, or task-created worktree reconciliation and cleanup. Apply [local Git preflight](git-usage.md#scope-and-preflight) before edits or Git mutations.

## Parallel work and worktrees

- Give each concurrent write stream its own branch and worktree; reuse a suitable task-owned worktree when safe. Read-only reviewers and planners may share a stable checkout; planners can return inline drafts to one integration writer. A single-writer restriction serializes writes, not read-only planning. If worktrees are unavailable or prohibited, serialize writers rather than switching branches in a shared directory.
- Admission, before dispatching parallel writers: the manager compares each stream's files, APIs/contracts, shared schemas, lockfiles, migrations, build outputs, generated artifacts, temporary paths, ports, databases, and external services. On overlap, serialize in dependency order or reassign to one stream before work starts. A worktree isolates files and indexes, not external services; serialize tests that mutate the same resource.
- Admit a stream only once its [record](#worktree-record) entry exists. Name its branch with the task ID (for example `feat/wt-1-component`, under the [branch convention](git-usage.md#branches-and-commits)). Place the worktree at a task-ID-specific path outside every other checkout's working directory, or for host-managed worktrees under a host path that checkout ignores. When creating a worktree, use the location in [`worktrees.md`](../local-paths/worktrees.md) if it exists, substituting `<repository-name>` (the repository top-level directory's basename) and `<task-id>`; the placement rules still apply. A manual `git worktree add` bypasses host worktree hooks and settings, so read that file before choosing its path; never place a worktree in the system temporary directory (for example `%TEMP%` or `/tmp`), and do not inherit a path from a checkpoint or another session without checking it against that file. Host-assigned branch/path replace the naming/path rule: record the entry as `planned` with "host-assigned" and complete it on return. Start from an agreed committed base; uncommitted changes do not carry over, so transfer them as reviewed commits or a scoped patch, preserving ownership and user changes.
- Build output: keep build artifacts inside the checkout or worktree being built, in a directory the repository ignores (for example Cargo's default `target/`); if the repository does not ignore it yet, add the rule to its `.gitignore` in the task's change. Do not redirect build output outside the repository (no `--target-dir` or `CARGO_TARGET_DIR` pointing elsewhere, never the system temporary directory). Each worktree has its own build directory, so concurrent builds never share one, and retiring the worktree removes it.
- Prefer the host's managed-worktree tools and follow their lifecycle rules; otherwise use Git-worktrees. Never check out one branch in several worktrees with force options. One owner creates, deletes, and renames refs, repoints any ref other than a stream's own branch, and handles repository-wide configuration or maintenance; avoid global Git configuration changes.

CLI example, after resolving the actual paths and base commit (replace all placeholders; do not execute them literally):

```text
git -C "<repository>" worktree add -b feat/<task-id>-component "<new-worktree-path>" "<base-commit>"
git -C "<new-worktree-path>" status --short --branch
```

### Worker boundary

- A worker runs commands with an explicit working directory or `git -C` in its assigned worktree and writes only there, to its owned files. It does not create worktrees or branches (except host-provisioned ones), switch branches, rebase/reset/delete other branches or refs, run `git worktree prune`, or touch another stream's worktree. On needing an out-of-scope write or discovering overlap, it stops that action, preserves state, and reports to its manager.
- Base drift: the manager alone decides whether a stream updates from a moved base (for example an upstream contract change); the stream's writer then merges or rebases only in its own worktree, rebasing only commits not yet handed off or pushed, and reruns affected checks. Otherwise defer reconciliation to [integration](#integration).

## Worktree record

One record per task-created worktree, kept by the creating manager (the creating agent for direct work; a one-line entry suffices for a single worktree); a child manager's entries roll into its parent's. Fields: task ID, absolute path, branch, base commit, creator, cleanup owner, owned files/resources, dependencies and required checks, state, and (when applicable) recovery ref or retention reason/condition. Briefs carry the stream's entry by these field names. A worktree in `git worktree list --porcelain` traceable to a recorded dispatch matches its entry rather than being unowned; reconcile an isolated agent cancelled or crashed before return by matching against that dispatch.

States: `planned` (manager, before creation) → `active` (manager, once the porcelain list shows it) → `handed off` (worker reported `ready for review` and the gate below passed; the manager records acceptance per [coordination](subagent-coordination.md#assignment-and-result-contracts)) → `integrated` (integration owner supplies proof) or `preserved` (recovery commit/patch recorded) → `retired` (cleanup owner supplies removal evidence). A `preserved` worktree may then be retired, else `retained`. `retained` (reason, condition, owner) may replace any state after `active` and returns to the flow when its condition is met. The manager records each transition from the named actor's evidence. Transferring accountability requires explicit acceptance.

### Handoff gate and recovery

- A stream is handed off only when `git -C "<path>" status --porcelain --untracked-files=all` shows nothing except declared artifacts, the head SHA and branch are reported, its processes, ports, containers, and services are stopped or named to an accepting owner, generated artifacts are listed, and ignored outputs needed downstream are declared. The worker reports this through the [result schema](../templates/subagent-result.md) and stops writing.
- For a failed, cancelled, crashed, or dirty-orphaned stream, stop writers and preserve before any retirement: a checkpoint commit on a recovery branch or a scoped patch with path/hash, recorded in the entry. The manager or cleanup owner makes the commit in the preserved worktree via `git -C`. Never delete or clean to make the problem disappear. A worktree absent from the record is not task-owned; report it.

## Integration

1. Worker handoffs use the [coordination result contract](subagent-coordination.md#assignment-and-result-contracts) and [result schema](../templates/subagent-result.md), accepted only after the handoff gate, and include the assigned checkout/branch and commit SHA under [commit before ending work](git-usage.md#commit-before-ending-work). A scoped-patch handoff under a disclosed blocker is accepted through the recovery path (`preserved`), not the clean-status gate.
2. One integration owner applies results in dependency order in the assigned integration checkout, using the repository's merge/cherry-pick convention; do not apply both a branch merge and its commits.
3. Resolve conflicts by understanding both changes and their contracts; coordinate unclear intent with the responsible manager. Do not accept all of one side or discard unrelated edits.
4. Inspect the combined diff and apply [shared validation rules](validation.md#shared-validation-rules) to the integrated revision.
5. When blocked, pause only the dependent integration step; continue independent authorized work. Report unresolved conflicts and failing checks.

## Safe cleanup

- Creating a worktree includes responsibility for its retirement; cleanup of task-created worktrees is part of the task's default scope. The coordinating manager owns reconciliation of all worktrees created for its workstream, including descendants'; the root verifies it across the task before closing. For direct work, the creating agent owns cleanup. Delegating removal does not transfer accountability unless the receiving owner explicitly accepts it.
- An inactive worktree is not evidence of ownership or safe removal; do not remove pre-existing, shared, primary, pinned, or user-retained worktrees without ownership and host authorization. Task completion alone does not justify leaving an unused worktree; pending review, required artifacts, active consumers, or a host restriction may justify retention. On cancellation or handoff, retire unneeded worktrees or explicitly transfer outstanding cleanup.
- The cleanup owner runs, in order, before declaring cleanup done:
  1. Compare `git worktree list --porcelain` with the record, normalizing absolute paths (porcelain uses forward slashes): each entry is listed (or verified gone), and unrecorded worktrees stay untouched.
  2. Per worktree, confirm no process or shell cwd is inside it; inspect `git -C "<path>" status --porcelain --untracked-files=all --ignored`; preserve required outputs.
  3. Prove integration: `git merge-base --is-ancestor <branch> <integration-branch>`, or for squash, rebase, or cherry-pick integration (with or without a PR) `git diff <integration-branch> <branch> -- <owned paths>` is empty or explained; then record the branch head SHA in the entry. Otherwise record a preserved ref or patch; retire the worktree once preserved, else mark it `retained`.
  4. Retire: a managed worktree through its host tool; otherwise `git worktree remove "<verified-path>"` with no force and no recursive delete. Tools that discard unrecoverable work (uncommitted changes, kept dirty worktrees, session deletion) need the user's explicit request. Report a locked worktree as retained; unlock only a lock this task set whose holder has stopped. On a dirty refusal, delete only enumerated, verified-unneeded task-generated files and retry; otherwise (including submodules) mark it `retained`.
  5. Delete only task-owned branches not checked out elsewhere or needed, after the worktree is retired, with `git branch -d`. When `-d` refuses only because integration was by squash, rebase, or cherry-pick, use `-D` without asking once step 3 proved integration by content and recorded the head SHA; otherwise keep the branch and note it. Remote branch deletion follows [publication](git-publication.md#pull-requests-and-merge-rules).
  6. Run `git worktree prune` only after a task-owned directory was removed out-of-band and `--dry-run` lists exclusively task-owned entries.
  7. Re-run the list and branch inventories against the record.
- Reconciliation report, one line per record entry: task ID, path, branch, `retired` (removal and integration/preserved-ref evidence) or `retained` (reason, cleanup condition, cleanup owner), plus validation results and unrecorded observations. Root's close requires every entry terminal (`retired` or `retained`), with no active writers or processes in any `retained` worktree.
