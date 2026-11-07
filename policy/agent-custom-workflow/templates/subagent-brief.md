# Subagent assignment brief

Assignment fields under the [coordination contract](../guides/subagent-coordination.md#assignment-and-result-contracts). Combine fields in prose; omit inapplicable fields, not required constraints. The recipient need not read the template.

## Compact schema

```text
Task ID; tag line `Role: <leaf|manager>; Profile: <name>` (mandatory for every child: exact lowercase file stem of a [catalogue profile](../profiles/README.md), e.g. `implementer`; `manager` for managers; if none fits exactly, use the closest, e.g. `explorer` for read-only discovery, `implementer` for changes, never omit it or invent a name); immediate manager and acceptance owner:
Deliverable; scope/non-goals; checks/expected outcomes (assigned subset of root's boolean goals, each with its verifying command/check); stopping milestone:
Absolute workspace; file context (paths, symbols/ranges/sections, revisions, why each matters, or stated gaps); owned writes/resources (or read-only):
Model/effort; preset and effective ceiling; user restrictions:
Instruction manifest (guides): exact owner paths/sections, absolute or relative to a stated absolute workflow-repository root:
Return format/location; actionable limits/checkpoint, if beyond the milestone:
```

Managers fill the file-context and guide fields per [context handoff](../workflows/AGENTS.manager.md#context-handoff). When applicable, add plan stage excerpt, plan document path/revision, decisions/assumptions/dependencies; the stream's [worktree record](../guides/git-worktrees.md#worktree-record) entry (absolute path, branch, base commit, creator/cleanup owner, owned files/resources, dependencies/checks, initial state); iteration unit/allowance, retry override, configured limits versus monitored allowances, and stop instructions. Use the coordination retry default otherwise. Keep manager-only controls out of the brief; child managers receive what they need to direct descendants.

For a manager, include the [manager assignment requirements](../workflows/manager-delegation.md#manager-or-leaf-selection): delegated authority, likely child deliverables, coordination responsibility and allocated limits/ownership. Do not include leaf-only authority.

## Expanded schema

For assignments requiring the expanded contract, also load [additional fields](subagent-brief-expanded.md).
