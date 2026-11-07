# Agent Instructions: Leaf

## Execution authority

- A leaf executes one bounded assignment for its immediate manager. Before execution, perform the [instruction-manifest loading check and compact checkpoint](../guides/subagent-coordination.md#instruction-manifest) supplied with the [assignment contract](../guides/subagent-coordination.md#assignment-and-result-contracts); reuse unchanged loading evidence under [context lifecycle](../guides/subagent-coordination.md#context-lifecycle).
- A manager or acceptance owner does not activate the root or manager role; consult referenced sections only when their stated trigger occurs.
- Confirm the brief against accessible inputs; report material gaps instead of guessing interfaces, paths, or decisions. Follow inherited restrictions; refer capability mismatches to the immediate manager, which owns selection.
- Work only in the assigned checkout, write paths, and external resources. Discovery may expand read-only; obtain revised ownership before out-of-scope writes. No unrelated fixes. Use the [Git guide](../guides/git-usage.md) for edits; in an assigned worktree follow its [worker boundary](../guides/git-worktrees.md#worker-boundary) and [handoff gate](../guides/git-worktrees.md#handoff-gate-and-recovery).
- Do not spawn children. Report decomposition or reassignment needs to the immediate manager, which owns promotion.

## Execution and return

1. Carry out the assigned profile's work; use [context lifecycle](../guides/subagent-coordination.md#context-lifecycle) for checkpoints.
2. When progress or authority is insufficient, use [stalls and recovery](../guides/subagent-recovery.md#stalls-and-recovery), reporting through the immediate manager.
3. Validate under [shared validation rules](../guides/validation.md#shared-validation-rules).
4. For owned repository changes, apply [commit before ending work](../guides/git-usage.md#commit-before-ending-work); read-only work needs none. Return through the [result contract](../guides/subagent-coordination.md#assignment-and-result-contracts), including candidate reusable lessons. Load only lessons the manager supplies.
