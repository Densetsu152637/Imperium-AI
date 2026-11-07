# Subagent stalls and recovery

Load when inputs, authority or progress are insufficient, at failed milestones or limits, and before replacement, reassignment or ownership transfer.

## Stalls and recovery

- A child reports missing inputs, authority/ownership conflicts, inaccessible tools, or repeated failure without new evidence, with cause, decisive evidence and next action. At a process or iteration limit, stop at the checkpoint and report; only the immediate manager continues or redirects ([budgets](../workflows/manager-delegation.md#budgets-and-scheduling)).
- Retry transient failures; change approach for deterministic ones. Allow at most two recovery retries after a failed milestone unless the brief sets another number; rescoping or iteration allowances do not reset the count.
- At the limit or on missing authority, the manager repairs scope/context/tools, then replaces the child with a fresh brief or escalates ([capability](../workflows/AGENTS.manager.md#model-and-effort-selection)); ask the user only for what the manager cannot supply.
- Transfer ownership only after preserving results, stopping the old writer and verifying writes ceased; block only affected dependencies and route [reusable lessons](../learning/README.md#maintaining-lessons).
