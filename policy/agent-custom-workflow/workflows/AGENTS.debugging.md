# Agent Instructions: Debugging and Incident Investigation Workflow

## Scope and inputs

Use this workflow for incorrect behavior, regressions, exceptions, intermittent failures, or incidents, including diagnosis plans for application, test, build, and container failures. It owns causal investigation; tool-specific guides supply command and environment procedures. [Coding standards](../guides/coding-standards.md) own implementation and the [validation guide](../guides/validation.md) owns shared evidence requirements.

Assigned investigators use the [debugger profile](../profiles/AGENTS.debugger.md).

- Establish expected and observed behavior, impact, affected versions/environments, reproduction inputs, and the first known failing and last known working states. Separate supplied reports from reproduced observations.
- Record the runtime/configuration and source revision; handle sensitive evidence under [environment configuration](../guides/environment-configuration.md#configuration-output).
- For an active incident, distinguish immediate containment from root-cause repair; record temporary mitigation, its limits, and the follow-up owner. Scope live-system actions through the manager; investigation does not grant production mutation authority.

## Investigation

1. Reproduce with the smallest representative case. If reproduction is unavailable, preserve evidence and label conclusions provisional.
2. Maintain a compact hypothesis ledger: candidate cause, supporting/contradicting evidence, discriminating check, and result.
3. Change one relevant factor at a time where practical. Preserve the failing evidence before changes and isolate mutable test resources under [Git usage](../guides/git-worktrees.md#parallel-work-and-worktrees).
4. For timing or concurrency failures, measure frequencies and timing; a single successful rerun does not establish an intermittent fix.
5. Use a bounded checkpoint under [shared execution defaults](../AGENTS.md#execution-defaults), or [manager scheduling](manager-delegation.md#budgets-and-scheduling) when directing descendants. Stop when the causal chain is supported, remaining hypotheses cannot change the repair, or access/budget prevents further discrimination. Use [recovery](../guides/subagent-recovery.md#stalls-and-recovery) for failed milestones; ordinary hypothesis experiments are not recovery retries.

## Repair and validation

- Return the causal explanation, evidence locators, rejected alternatives, and the smallest repair to the assigned implementer through the manager. Distinguish a workaround from a root-cause fix.
- Where feasible, demonstrate the failure on the old revision and success on the repaired revision with the same regression check, and validate adjacent failure paths under [shared validation](../guides/validation.md#shared-validation-rules).
- For incidents, verify recovery against the affected service behavior over the relevant observation window, including any residual mitigation. Report symptom resolution separately from certainty about the cause.
- Deliver reproduction steps, tested revisions/environment, experiment outcomes, root-cause confidence, repair/mitigation, and remaining uncertainty. Submit genuinely reusable lessons through [catalog maintenance](../learning/README.md#maintaining-lessons).
