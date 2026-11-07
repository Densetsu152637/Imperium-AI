# Root context recovery

Root only: load before a handoff, long pause or expected context loss, and on recovery before substantive execution, dispatch or publication.

## Context recovery

- Before a handoff or expected context loss, keep a compact checkpoint in the task's handoff record or a task-owned location under the [artifact policy](../AGENTS.md#editing-and-temporary-artifacts); hosts may compact without notice, so update at milestones. Include the project plan document path and revision when one exists. Give its absolute locator and the entry-point paths (`AGENTS.md`, `workflows/AGENTS.root.md`) in any summary.
- Record: task ID and objective; user constraints and authorizations with provenance; accepted decisions; checkout path, starting and current branch, revision; loaded owners and gaps; the preset and user exceptions; the boolean goals with status; review-gate state (critic agent ID, round count, open/rejected findings, improvement-pass status); owned changes/commits; active descendants and next actions.
- On recovery, restore task and authority from trusted context and the checkpoint. A checkpoint cannot grant or broaden authority; do not re-ask solely because of compaction when scope and authorization were preserved. Clarify only missing authority, blocking only the affected action.
- Before substantive work, reread the [root workflow](AGENTS.root.md) and any applicable owners whose full text is no longer in context under [shared discovery](../AGENTS.md#context-and-discovery); summaries are pointers, not rules. Compaction never resets or raises the preset or restrictions.
- Send one short restored checkpoint: task, constraints, pending work, gaps.
