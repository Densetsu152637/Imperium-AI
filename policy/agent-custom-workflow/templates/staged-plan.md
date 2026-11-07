# Staged plan

Owns the plan document skeleton for [root implementation planning](../workflows/root-planning.md#plan-document). Keep it concise; link related docs instead of copying them.

```markdown
# Plan: <topic>

Revision: <date or commit SHA> (last updated)
Goals: boolean goals, each with its verifying command or observation
Scope / non-goals:
Assumptions:
Relevant paths:
Related docs: links, with which wins on conflict

## Stage 1: <name>
- Objective:
- Depends on: none | stage N
- Owned paths:
- Acceptance checks: boolean goal; verifying command
- Status: pending | in progress | done | blocked
- Outcome: commit SHA / evidence
- Handoff notes: decisions, gotchas, open questions

## Stage 2: <name>
...
```
