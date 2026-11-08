# Plan template

The shape of the plan you keep with `update_plan`: a goal, then stages as steps. Revise it when
evidence contradicts an assumption, and say what changed.

## Goals
Measurable outcomes, in the user's terms. State how you will know each one is met.

## Stages
Dependency-ordered. For each stage:
- **Title**: what it does, in a few words.
- **Depends on**: the stages that must finish first. Stages with no unmet dependency run in
  parallel as separate subagent tasks.
- **Owner**: yourself, or the subagent (and tier) that does it.
- **Acceptance**: the check that shows the stage is done.

## Assumptions
Each gap you filled yourself, by inference, a default or research, with the value you chose.

## Open questions
Only decisions that are the user's to make or cannot be researched. Ask them together in one
`ask_user` batch (see the question format) and keep working on stages that do not depend on them.
