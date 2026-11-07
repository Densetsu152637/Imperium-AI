# Subagent result record

Owns result fields under the [coordination contract](../guides/subagent-coordination.md#assignment-and-result-contracts).

Lifecycle values: `in progress | ready for review | accepted | blocked | cancelled`

## Compact schema

```text
Task ID; status; result/artifact paths and stable revision:
Acceptance verdicts (each assigned goal/check true or false) with check/procedure, exit status and decisive evidence:
Loading checkpoint reference; later reads/gaps and material limitations:
Required next action/owner; writes stopped, outstanding processes/cleanup, if applicable:
```

State changes and the tested revision. For an owned worktree, include the [handoff gate](../guides/git-worktrees.md#handoff-gate-and-recovery) evidence and, if retained, its path, cleanup owner and condition. Keep handoff metadata short (about 200 words) without truncating acceptance coverage; link artifacts rather than copying, and return the deliverable once.

## Expanded schema

For expanded handoffs, also load [additional fields](subagent-result-expanded.md).
