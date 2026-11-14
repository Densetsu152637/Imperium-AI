# Ambiguity and reference patterns

Messages often point at something the model cannot see: "that file", "the issue from earlier",
"the second option", "deploy it". A small model that guesses acts on the wrong thing; one that
asks about everything is useless. These are the patterns, each with what to do in order.

The same table is in `imperium_protocol::patterns` (the engine puts the matching guidance at
the top of the turn); a test keeps this file and that table in step. The ids below are the
headings.

General order for every pattern:

1. Look at what is already known: the `<session-state>` block (entities, goals, constraints),
   the conversation, the latest tool results. A reference that has exactly one candidate there
   is resolved: say which one you took and go on.
2. If that does not settle it, look it up with a tool (`glob`, `grep`, `search_sessions`,
   `memory_search`) before asking.
3. If it is still open, call `request_clarification(missing_parameter, options)` with the
   likely answers, best first, and do whatever does not depend on the answer meanwhile. Never
   ask for something one of the steps above would have found.
4. When the action cannot be taken back (deleting, sending, deploying, publishing), a vague
   scope or a missing destination is always a reason to ask, even at high confidence.

## earlier-issue

"Fix the issue from earlier." "It is still failing." "Try again."

Inspect the session state's errors and tasks and the latest failed tool results first. One
failure: fix that one. Several: ask with them as options. None: ask what failed and where, with
no invented options.

## that-file

"Open that file." "Change the config." "Run that test." "This one."

Resolve against the state's files, then the most recently read or written files. One candidate:
use it and name it. Several: ask with the paths as options. None: search the project first.

## previous-step

"Do that again." "Undo the last step." "Revert that."

Take it from the plan and the latest tool calls of this conversation. Undo only what was
changed here; confirm first when it touched more than a few files.

## same-as-before

"Same as last time." "Like we did before." "As usual."

Search earlier sessions and saved memories. One clear match: repeat it and say which. Nothing
or several: ask what to repeat; never invent a past.

## numbered-reference

"The second option." "Option B." "The other one."

Map the reference onto the latest list in the previous reply and say which item it was. No
list, or two items fit: ask with the items as options.

## destructive-scope

"Delete the old logs." "Clean up." "Remove everything." "Force push."

List the concrete set first. A small, clearly covered set: say what goes and proceed. Anything
vague or large: ask with concrete groups as options, the smallest first.

## missing-destination

"Send it to the team." "Deploy it." "Publish this."

The recipient or environment is missing and the action reaches outside the project. Build the
options from the state and the project configuration (staging, production, known recipients),
ask, and prepare whatever does not depend on the answer.

## vague-improvement

"Make it faster." "Clean it up." "Improve it."

Take the target from the goals and constraints in the state or from the slow command or failing
test just shown. None: ask what to improve with two or three concrete options from the code in
play.

## correction

"Actually, not that one." "I meant the other file." "Scratch that."

The latest message wins. Drop the superseded goal, constraint or entity with `update_state`,
then redo the work on the corrected target. Ask only when it is unclear which earlier choice is
replaced.

## How training should reward it

`schemas/eval/clarification-cases.json` holds messages whose context either settles the
reference (`ambiguous: false`) or does not (`ambiguous: true`, with the missing parameter and
the expected options). Run each through a rollout and read what the model did from the turn's
events (`imperium_protocol::clarification::Observed::from_events`), then score it:

| Case | Behaviour | Reward |
| --- | --- | --- |
| ambiguous | calls `request_clarification` | 0.6, plus 0.2 when `missing_parameter` names the missing thing, plus 0.2 when its `options` hold an expected answer |
| ambiguous | acts without asking | -1.0 |
| resolvable | acts | 1.0 |
| resolvable | asks anyway | -0.5 |

`imperium_protocol::clarification::report` also gives the ask recall (ambiguous cases it asked
on) and ask precision (its asks that were on ambiguous cases); a model that asks about
everything has full recall and poor precision, and one that never asks the reverse. The reward
makes both lose, so only asking exactly when the context does not settle it scores well.
