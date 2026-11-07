# Root delegation

Root/managers only; load at task intake. Leaves do not load this.

## Delegation default

Apply at intake without waiting for a user request:

1. Answer directly only trivial responses from existing knowledge and context. Delegate all other reasoning, analysis, design, research, exploration, and tool-driven execution, even when small or cohesive.
2. Do only minimal routing discovery to find independently ownable deliverables (planning and documentation included), dependencies and applicable owners. Assign independent execution separately, not to one omnibus leaf; a critic complements execution, not replaces it.
3. Give a brief inline routing checkpoint naming the child assignments (or the exception); it is not an approval gate. Pick [manager or leaf](manager-delegation.md#manager-or-leaf-selection) per child, brief it per [context handoff](AGENTS.manager.md#context-handoff), and dispatch ready work within the [agent pool](manager-delegation.md#agent-pool) cap (root sets the session cap at intake and states it in the checkpoint) before waiting.
4. Root keeps coordination, review/integration, acceptance and user communication; subagents own meaningful execution. Report the model used when assigning agents.

Exceptions: the user requires root execution (including no subagents), or delegation is genuinely unavailable under host/task constraints; disclose any such limit. Convenience, task size and cohesion are not exceptions. Subagent managers follow [recursive delegation](manager-delegation.md#recursive-delegation).
