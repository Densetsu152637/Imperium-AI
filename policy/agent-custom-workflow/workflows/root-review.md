# Root review

Root only: load before closing non-trivial implementation or refactor work and after writing or revising a plan for it ([AGENTS.root.md](AGENTS.root.md#root-responsibilities) triggers). Workers do not load this file; critics get the [critic profile](../profiles/AGENTS.critic.md) plus their brief.

## Gates

- Plan gate: after root writes or materially revises a plan (a [plan document](root-planning.md#plan-document), or the plan and goals for any non-trivial implementation) and before dispatching implementation stages, dispatch an independent critic.
- Implementation gate: before acceptance of non-trivial implementation or refactor work, after integration and validation evidence exist and before commit/push close-out, dispatch a critic on the final diff against the goals and validation evidence.
- Non-trivial means the task requires [goals](AGENTS.root.md#goals); scale the critic's scope to the artifact.
- Gates complement, not replace, [validation](../guides/validation.md#shared-validation-rules) and domain-mandated critique. Skip them for trivial direct answers and work with no plan or implementation artifact.

## Critic brief

- A leaf with Profile: critic, fresh context (never a fork or inherited history), and never the agent that authored the artifact.
- The brief carries only the artifact (plan document path and revision, or diff range/commit), the boolean [goals](AGENTS.root.md#goals), scope and non-goals, relevant paths, and validation evidence locators. Omit root's rationale and preferred conclusions to avoid anchoring.
- Ask for findings that could change acceptance, each with location, impact, confidence, and smallest fix or check, per the critic profile.
- Model: Specialist by default; Frontier only for consequential or hard critique per [model and effort selection](AGENTS.manager.md#model-and-effort-selection).

## Agent pool

The gate critic does not count toward the session subagent cap ([agent pool](manager-delegation.md#agent-pool)), including its reuse rounds and any fresh-context replacement. Only root dispatches gate critics; the exclusion covers no other critic, and critics dispatched by managers count against their share. At most one gate critic runs at a time: a replacement starts only after the prior critic has finished or been stopped. The usage gate, user no-subagent instructions and host limits still apply; if the usage gate blocks the spawn, use the [self-check](#self-check) and say so.

## Triage

- Root records each finding as accepted (fix it or assign the fix) or rejected with an evidence-backed reason; preferences may be declined without evidence. Implementation-gate fixes and adopted improvements go to the same implementer by reuse (it counts toward the agent pool); plan-gate revisions stay with the plan's [single writer](root-planning.md#plan-document). Use a fresh implementer only if the original is unavailable or the work needs a different profile.
- After revisions, resubmit the revised artifact to the same critic by reuse, for a delta review. Repeat while rounds keep producing accepted acceptance-changing findings.
- Stop when: (a) a round returns no open finding that could change acceptance (converged); (b) a round yields no newly accepted finding, or findings repeat or oscillate (no progress); or (c) a budget or allowance, the usage gate or a user limit is reached.
- There is no fixed round limit; preferences never extend the triage loop and go to the [improvement pass](#improvement-pass) instead. Every third round (triage and improvement rounds counted together), tell the user the rounds so far, findings and improvements accepted, and what remains open.
- If the reused critic appears anchored on its earlier findings, a new artifact revision may earn one fresh-context critic.
- On (b) or (c) with open acceptance-changing findings, or a rejected acceptance-changing finding the critic maintains after root's rebuttal, report them to the user as blockers or open risks with both positions; never drop them silently or vote them away.
- The final report lists gate outcomes briefly: rounds, accepted/rejected counts, open items.

## Improvement pass

- Starts only when a gate's triage stops on (a) convergence, never on no-progress or limits. Reuse the same critic.
- The critic returns at most 3 ranked in-scope improvements to the already-sound artifact (e.g. robustness, failure handling, simplicity, testability, risky assumptions, maintainability), each with expected benefit, cost, added risk, and smallest change and check. It lists out-of-scope suggestions separately, judged against the brief's scope and non-goals; they do not count toward the 3.
- Root adopts in-scope suggestions whose benefit outweighs cost and declines the rest with a reason. Out-of-scope suggestions are never adopted; collect them for the user.
- Apply adopted improvements: plan gate, into the plan document; implementation gate, implemented (by assignment where delegation applies) and re-validated. Then resubmit. Each round the critic first checks the revision for acceptance-changing defects (any found return to [Triage](#triage), and the pass resumes once triage converges again), then proposes the next improvements.
- No round limit. Stop when: (i) no more improvement: the critic returns no in-scope suggestion, none is worth adopting, or suggestions repeat declined ones or reverse adopted ones; (ii) scope convergence: only out-of-scope suggestions remain; or (iii) a budget or allowance, the usage gate or a user limit is reached. On (iii), validate or revert any applied but unvalidated improvement before close-out, and report it.
- Improvements never block acceptance. The final report lists adopted improvements, declined ones with reasons, and out-of-scope ones deferred to the user.

## Self-check

When a gate applies but no critic can be dispatched ([delegation exceptions](root-delegation.md#delegation-default), usage gate, host limits) or a dispatched critic fails to return, root runs a short self-check and discloses that the review was not independent:

- each goal met, with evidence
- consequential assumptions
- missing failure paths
- interface or compatibility breaks
- docs aligned ([documentation alignment](../guides/coding-standards.md#documentation-alignment))
- validation actually supports the claims

## Learning

At close, compare accepted findings with the [failure catalog index](../learning/failure-catalog.md#index). A finding matching an existing entry adds evidence to it; a recurring or costly avoidable pattern becomes a candidate lesson under [maintaining lessons](../learning/README.md#maintaining-lessons). One-off or preference findings are not recorded.
