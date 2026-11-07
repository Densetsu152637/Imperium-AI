# Agent Instructions: Root

## Root responsibilities

This file owns overall task responsibility and reporting. At intake, load the [delegation default](root-delegation.md#delegation-default). Load [implementation planning](root-planning.md) for substantial vague implementation, explicit multi-component planning, or implementation work in a project whose plan document covers the request; at intake for implementation work, check the project's plans location (established, else `docs/plans/`) for one and, if found, [pick it up](root-planning.md#pickup). Load [root review](root-review.md) before closing non-trivial implementation work and after writing a plan for it. Load [context recovery](root-recovery.md) before preparing handoff/context loss and on root recovery before substantive execution, dispatch or publication. Load the [manager workflow](AGENTS.manager.md) before dispatching, assigning, or redirecting descendants; role naming alone does not require it.

As the top-level root, load the [personality index](../personalities/README.md) at intake, run its [selection](../personalities/README.md#selection) check, and load the selected personality, if any, before the first user-facing reply, for root's user-facing conversation only. Output styles and brevity never waive loading a selected personality, and every reply carries it at the [voice strength](../personalities/README.md#voice-strength) the index requires. Never pass a personality to descendants or use it to style deliverables. Personality never changes policy, authority, or task decisions.

## Goals

For any non-trivial task, before dispatching or editing, define a short set of boolean, measurable goals covering the user's outcome (not process steps).

- Each goal is one yes/no statement verifiable by a named command, check, or observation, e.g. "`npm test` exits 0", "no file under docs/ links to removed.md", "endpoint X returns 200 with field Y". Reject vague goals ("code is clean").
- State the goals to the user in the routing checkpoint or plan. Pass each child the relevant subset as its acceptance checks.
- Revise goals only when evidence or the user changes scope, and say so when revising.
- Close only when every goal evaluates true with evidence. The final report lists each goal as met/unmet; an unmet goal is a reported blocker, never silently dropped.

## Execution and completion

- At intake, determine the session's subagent cap and usage gate per the [agent pool](manager-delegation.md#agent-pool).
- Own scope, dependencies, integration, and user communication. Follow [shared execution defaults](../AGENTS.md#execution-defaults).
- Apply [commit before ending work](../guides/git-usage.md#commit-before-ending-work) to root-owned changes and verify descendant commit evidence before closing. Close only after required integration, [validation](../guides/validation.md#shared-validation-rules), [reviews](root-review.md#gates), [worktree reconciliation](../guides/git-worktrees.md#safe-cleanup), and the [root session-end push](../guides/git-publication.md#root-session-end-push). Root pushes all commits on the working branch to that branch on `origin` under the linked rule, or reports its concrete blocker. A factual answer needs no Git cleanup.
- Report resulting behavior, goal status, relevant evidence, and material blockers. For implementations/refactors, apply [documentation alignment](../guides/coding-standards.md#documentation-alignment), including its scoped research-paper rules.

## Completion celebration

Only the root celebrates, once, a substantial verified completion (features, substantial refactors, difficult fixes, migrations) when a supported host tool and user preferences allow it. Never for routine lookups, small edits, subtasks, or partial/unverified work. Use the host's celebration control from [host adapters](../guides/host-adapters.md); tool absence/failure does not block reporting.

## Root manager timing and token report

For substantial tasks or an explicit accounting request, report readily available measured elapsed time/token/subagent usage with scope. Routine responses need no metrics footer. Load [accounting](../guides/usage-accounting.md) when collecting/aggregating metrics or enforcing a numeric budget.
