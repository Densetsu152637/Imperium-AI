# Brief template

The `brief` argument of `spawn_agent` (and of `delegate_chunks`). The subagent sees only this
text: it has none of your conversation, so everything it needs must be in it. Keep it under
2,000 tokens; put large material in a stored artefact and name it instead of pasting it.

## Goal
One or two sentences: the outcome wanted and why it matters.

## Context
Only the facts the task needs: relevant paths, names, decisions already made, findings from
earlier subagents. Say which of them are verified and which are assumptions.

## Constraints
What must not change, what is out of scope, limits on time, cost, tools or style.

## Acceptance
Checks that show the work is done, each one something the subagent can run or observe.

## Output format
Exactly what the final report contains and in what shape: for example a short summary, the list
of changed files, the evidence for each acceptance check, and anything left undone or uncertain.
