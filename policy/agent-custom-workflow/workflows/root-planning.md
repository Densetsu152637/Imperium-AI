# Root implementation planning

Root only: load for substantial vague implementation prompts or explicit multi-component planning, including planning-only work, and at intake for implementation work when the project has a plan document covering the request ([pickup](#pickup)). Workers receive their stage excerpt and the plan document path; they do not load this file.

## Implementation planning

Root performs only minimal routing discovery, then delegates substantive analysis and design (discovery may be delegated first), and integrates the results into a staged plan document proportional to the task, detailed enough that implementation agents need not repeat design work.

- Define the [goals](AGENTS.root.md#goals) first and record them in the plan, with intended outcome, scope, assumptions, and relevant paths. Delegate resolution of substantive implementation decisions and interfaces.
- The plan of record is a [plan document](#plan-document) in the consuming project, not commentary or the host task list. Built-in planning/progress APIs track progress only. Do not enter a mode requiring a separate approval turn merely to record the plan.
- Order stages by dependency; assign ownership, deliverables, integration points, and acceptance checks drawn from the goals. Pass each agent its stage excerpt, the plan document path and revision, and assumptions through the [assignment contract](../guides/subagent-coordination.md#assignment-and-result-contracts).
- An ordinary plan is ready to execute under the user's existing authorization: implement or delegate authorized stages in the same turn, updating the document as each completes; stages left incomplete stay documented for future agents. Planning-only work ends with the committed plan document. This is not actual user approval and does not bypass higher-priority instructions, mandatory host approvals, or unanswered material clarification.
- The plan passes the [plan gate](root-review.md#gates) before implementation stages are dispatched.
- Infer routine reversible choices; ask only for blocking missing scope, information, or authority, and continue independent authorized work. Revise the plan, goals (stating the revision), and affected briefs when material evidence changes decisions, dependencies, or acceptance.

## Plan document

- Location: the project's established plans location if any, else `docs/plans/<topic-slug>.md`. One document per workstream or topic, not per session. Use the [staged plan template](../templates/staged-plan.md).
- Stages are dependency-ordered and each executable and resumable by a fresh agent with no conversation history: objective, dependencies, owned paths, acceptance checks (boolean goals with a verifying command), status (`pending` | `in progress` | `done` | `blocked`), outcome (commit SHA or evidence), and handoff notes (decisions, gotchas, open questions).
- Header: goals, scope and non-goals, assumptions, relevant paths, related docs, last-updated revision.
- Root (or the assigned plan owner) is the single writer. Workers report outcomes; root updates status and outcome at each stage transition and commits the plan document with the task's commits under [git usage](../guides/git-usage.md#branches-and-commits).

## Consolidation

Before writing, search existing plan documents and related project docs (architecture or decision records, roadmap/TODO, `docs/`).

- If a plan already covers the scope, update it instead of creating another. Merge overlapping plans into one and replace or remove the redundant ones.
- Link existing docs rather than duplicating their content.
- Resolve conflicts with existing docs explicitly: state which wins and update the stale one.

## Pickup

At intake for implementation work in a project, check the plan location for a plan covering the request. If found, resume instead of re-planning: re-verify the recorded state against the repo, take the first ready `pending` stage, mark it `in progress`, and consolidate any new scope into the same document.

## Completion

When every stage is `done`, fold durable decisions and behavior into the project's lasting docs under [documentation alignment](../guides/coding-standards.md#documentation-alignment), then delete the plan document (git history keeps it) unless the project's convention keeps completed plans.
