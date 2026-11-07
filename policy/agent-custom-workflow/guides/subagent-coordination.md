# Subagent coordination

Owns handoffs, instruction loading and retained context. Load [stalls and recovery](subagent-recovery.md) only for its triggers. [Manager policy](../workflows/AGENTS.manager.md) owns dispatch; [leaf policy](../workflows/AGENTS.leaf.md) owns execution authority.

## Assignment and result contracts

- After discovery, supply an explicit [brief](../templates/subagent-brief.md): compact for bounded work with stable inputs, expanded for multiphase work, replacement, cross-stream integration or consequential uncertainty. Include the relevant stage excerpt and path of the [plan document](../workflows/root-planning.md#plan-document), decisions and dependencies; on updates send changes only.
- Every handoff uses the [result schema](../templates/subagent-result.md), inline for compact work and in an assigned record for expanded work. Workers report `ready for review`; only the immediate manager records `accepted`.
- Before return or transfer, [commit owned repository changes](git-usage.md#commit-before-ending-work) and report the SHA (a scoped patch if committing is blocked; non-Git results use a hash/artifact version). Stop writes for review; resume only under the manager. Follow [validation](validation.md#shared-validation-rules).

## Instruction manifest

- Give exact owner paths relative to an explicit absolute root, with role/domain labels and section names, per the [context handoff](../workflows/AGENTS.manager.md#context-handoff). Never include [personality](../personalities/README.md) files.
- Every child receives [shared policy](../AGENTS.md), this guide's contracts/manifest/context sections, [validation rules](validation.md#shared-validation-rules), and owners triggered by its actions. Add [leaf policy](../workflows/AGENTS.leaf.md) and the profile for a leaf; [manager policy](../workflows/AGENTS.manager.md) and [delegation](../workflows/manager-delegation.md) for a manager; [recovery](subagent-recovery.md) when triggered. Name needed sections, not whole files; read-only workers need no Git mutation rules.
- Complete unchanged authoritative text in the recipient's context counts as loaded under [shared loading rules](../AGENTS.md#context-and-discovery); paths and the parent's claim do not.
- Each child sends one pre-execution checkpoint referencing the manifest and loading basis, listing only exceptions, extra reads and gaps; no acknowledgement unless blocked. If required instructions are missing, stop the affected work and report through the manager; load newly applicable owners before the affected action.

## Context lifecycle

- Reuse a worker for related follow-ups with compact deltas, reusing unchanged loading evidence and reading only new or changed sections; use fresh context for unrelated work.
- When context or failed approaches impede work, replace or reset the worker through supported controls, preserving the result and artifacts.
- Before compaction, handoff or a long pause, keep resumable state in the record or a compact checkpoint, separate from [durable lessons](../learning/README.md). Root recovery: [root context recovery](../workflows/root-recovery.md#context-recovery).
- Keep decisive excerpts and artifact paths, not full logs; integrate results by changed decisions, not echoed reports.
