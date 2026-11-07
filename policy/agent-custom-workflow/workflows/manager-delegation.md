# Manager delegation

Managers: load at intake and before dispatch, redirection or acceptance. Ceilings and model/effort selection: [manager policy](AGENTS.manager.md). Leaves do not load this.

## Recursive delegation

- Managers own their workstream's child assignments, dependencies and integration; root duties are in [AGENTS.root.md](AGENTS.root.md#root-responsibilities). Every manager applies root's [delegation default](root-delegation.md#delegation-default) split to its own work: assign independently ownable deliverables separately and start ready children within the [agent pool](#agent-pool). User no-subagent instructions and host limits take precedence; disclose affected work.
- With no useful independent split, a non-root manager may execute cohesive work itself or delegate for specialist execution or review. Duplicate work only for a deliberate comparison.
- Keep the hierarchy shallow: respect depth/host limits; no cycles or managers for trivial work. Each agent has one immediate manager, which alone assigns or redirects it; route cross-workstream requests through managers. A leaf that finds a useful split reports it; its manager splits, reassigns, or promotes it with an updated brief (promotion lifts only the leaf role restriction, not inherited scope or user restrictions).
- Before splitting, fix acceptance criteria, dependencies and shared contracts; stop at cohesive, testable assignments.

## Manager or leaf selection

Root and every manager apply this before dispatch; difficulty alone does not select the role:

| Work assigned to the child                                                                               | Role                                                     |
| -------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| One cohesive deliverable with bounded acceptance                                                         | Leaf with the applicable profile                         |
| Independent deliverables the current manager can coordinate                                              | Separate leaves, started in parallel when ready          |
| A workstream with useful child deliverables and recurring dependency, interface or integration decisions | Manager owning that workstream and its integrated result |

- Assign the manager role automatically for the third case, naming its likely child deliverables. Its brief grants delegation within inherited restrictions and supplies the manager manifest, concurrency share, depth limit, interfaces, acceptance criteria and integration ownership. If the host role lacks delegation tools, flatten or queue.
- Do not fill the allowance with managers waiting on descendants.

## Agent pool

Plan-based subagent cap, staging and usage gates. Telemetry and control names: [host adapters](../guides/host-adapters.md).

- Session cap: root sets one cap at intake, re-checks it on root recovery, and states it in the routing checkpoint. Source order: the user's explicit cap in chat (it wins, even above the plan cap; state it); else the plan from host telemetry. Fill gaps from [`local-paths/plan.md`](../local-paths/plan.md) when telemetry is unavailable, not applicable (API key, gateway) or names a tier without its multiplier. If file and telemetry conflict, use the smaller cap and report the mismatch once.

| Plan                                    | Cap                                                       |
| --------------------------------------- | --------------------------------------------------------- |
| Free                                    | 1                                                         |
| Pro                                     | 2                                                         |
| Max Nx                                  | 4N/5, rounded down                                        |
| Max, multiplier unknown                 | 4                                                         |
| Team, Enterprise, API or not applicable | Per `plan.md` (a tier above or `Subagent cap: N`), else 2 |
| No plan known                           | 2                                                         |
| Usage Credits                           | 2                                                         |

- The cap counts every running non-manager subagent in the session tree at any depth. Root, every manager at any depth and root's [review-gate critic](root-review.md#agent-pool) are excluded. A manager never takes a slot itself, and its share covers its descendants only. Root allocates slot shares to child managers, and a share is never exceeded. A manager returns unused slots by finishing.
- Staging: managers queue ready assignments and spawn while a slot is free. When an agent finishes, is stopped or is exhausted, its slot frees and its immediate manager starts the next queued assignment. At the cap, wait for a completion notification; do not poll.
- Prefer reusing a finished worker for related follow-ups over a cold start. An idle reusable worker holds no slot; reuse takes one while it runs.
- Usage gate: immediately before each spawn, read the plan usage windows where telemetry exists. Do not spawn when the 5-hour window is at least 85% used or any applicable weekly window (all models, or the dispatched model's own) is at least 95% used. Running agents finish their current assignment; queued work stays queued; nothing is cancelled for the gate. Root tells the user once which window tripped and when it resets, and resumes staging after the reset or below threshold.
- A manager without telemetry asks its manager; root relays a halt to child managers through the reuse/message control. Without any telemetry, say so once and proceed under the cap alone.
- Reuse is not a spawn but still consumes usage: near the gate, finish rather than start new work.
- Usage Credits (signal: the user's statement, or usage information a plugin or mod puts in context, for example the Hit The Ground Running usage notes described in [host adapters](../guides/host-adapters.md#claude-code)): the usage gate is off and the cap is 2. Running agents finish their current assignment; queued work starts under the new cap.

## Budgets and scheduling

- Before dispatch set any applicable concurrency limit, effort/time allowance, stop condition, retry allowance and checkpoint. Honor user and host limits; a preset adds no token/cost caps. Configure documented host controls (concurrency, depth, turn/tool limits, timeouts, cancellation); do not invent APIs. Without them, allowances are monitored checkpoints; tell each child its limits and checkpoint.
- Concurrency is bounded by the [agent pool](#agent-pool): start ready independent children up to the free slots, within any user or host limit. Serialize only exclusive resources (a shared service stack, ports, databases, timing or performance measurements, single-use evaluation sets) and coordinate them through the root.
- Child allowances come from the parent's remaining budget, reserving review time. [Accounting](../guides/usage-accounting.md) is needed only for numeric budgets or required metrics.
- Start unblocking and ready independent work before waiting; serialize real dependencies, using [Git isolation](../guides/git-worktrees.md#parallel-work-and-worktrees) for concurrent writes.
- At the checkpoint or on material change, stop dispatching at the allowance and stop/redirect running work through available controls (if cancellation is unavailable, request a stop and verify writes ceased before transfer); narrow, queue or escalate when acceptance is unreachable. A stopped or exhausted run is incomplete, not done. User budgets cannot be expanded without authorization.

## Dispatch and acceptance

- New workers get fresh context plus a brief, selected explicitly (a brief alone does not disable default history inheritance); inherit history only when essential and say why. Pass only the [context handoff](AGENTS.manager.md#context-handoff) content, not transcripts.

1. Discover context ([shared discovery](../AGENTS.md#context-and-discovery)); for substantial vague implementation, apply the [implementation plan](root-planning.md#implementation-planning) first (or resume an existing [plan document](root-planning.md#pickup)) and carry its stages into assignments.
2. Select [manager or leaf](#manager-or-leaf-selection) and the [profile](../profiles/README.md) (mandatory for every child, closest fit if none is exact; a brief with no profile tag or a tag outside the catalogue is a defect); state the role explicitly; supply the [assignment contract](../guides/subagent-coordination.md#assignment-and-result-contracts) and [instruction manifest](../guides/subagent-coordination.md#instruction-manifest).
3. Apply [failure lessons](../learning/README.md#using-lessons), [model selection](AGENTS.manager.md#model-and-effort-selection) and the budgets above.
4. At acceptance, check the work against the rules and criteria; a read alone does not prove compliance. No audit agent or acknowledgement gate.
5. Coordinate path ownership before editing active child work; use [Git integration](../guides/git-worktrees.md#integration), [documentation alignment](../guides/coding-standards.md#documentation-alignment) and [validation](../guides/validation.md#shared-validation-rules).
6. [Commit before ending work](../guides/git-usage.md#commit-before-ending-work) for manager-owned changes and verify child commit evidence before acceptance. Add critique for material risk. Record acceptance in the [result contract](../guides/subagent-coordination.md#assignment-and-result-contracts); external approvals follow [Git publication](../guides/git-publication.md#pull-requests-and-merge-rules).
7. Route failure candidates to [catalog maintenance](../learning/README.md#maintaining-lessons); clean up under [Git cleanup](../guides/git-worktrees.md#safe-cleanup).
