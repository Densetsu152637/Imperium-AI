# Agent Instructions: Browser and Interactive Verification Workflow

## Scope and inputs

Use this workflow to verify websites or interactive applications through actual user journeys. It owns behavioral inspection; [vision](AGENTS.vision.md) owns visual inspection and [audio](AGENTS.audio.md) owns listening checks. Load relevant browser/computer-use skills and use supported host controls.

Assigned testers use the [navigator profile](../profiles/AGENTS.navigator.md).

- Identify the target URL/application, build/revision, environment, user role, starting state, device/viewport, required journeys, and expected outcomes.
- Establish service readiness under [shared validation](../guides/validation.md#shared-validation-rules). Use assigned accounts, fixtures, and isolated sessions where available; define ownership and cleanup for state created by checks under [Git resource isolation](../guides/git-worktrees.md#parallel-work-and-worktrees).
- Identify steps with external effects (sending, purchasing, publishing, changing shared records). Execute them only within existing authorization and assigned resources; use a sandbox where available and report untested live behavior.

## Journey verification

1. Start from a known state, observe the interface before choosing controls, and record actions and decisive state transitions so the journey is reproducible.
2. Check the result at the level the criterion requires: rendered state, navigation, state after reload/reopening, and server/API effects when accessible. A click, successful request, or screenshot alone does not establish the outcome.
3. Cover relevant empty, loading, error, retry, cancellation, invalid-input, and permission states, plus stale responses and repeated actions for asynchronous behavior.
4. When accessibility is in scope, exercise keyboard operation, focus movement, and labels. Report actual device/browser/viewport coverage rather than generalizing from one.
5. Use network, console, accessibility-tree, and application evidence to resolve ambiguity when supported. Redact sensitive data under [environment configuration](../guides/environment-configuration.md#configuration-output); inaccessible internals remain unverified.

## Findings and recheck

- Return a verdict per journey/criterion with target revision, starting state, steps, expected/actual outcome, evidence locators, and browser/device details. Separate functional defects from visual findings and environment/tool failures.
- Route repairs through the manager to the assigned editor. Recheck the stable repaired revision and dependent journeys under [shared validation](../guides/validation.md#shared-validation-rules); use [coordination recovery](../guides/subagent-recovery.md#stalls-and-recovery) for retries.
- If only static output or screenshots are accessible, complete those checks with the appropriate domain workflow and leave interactive behavior unverified. Clean up authorized task-created state using the applicable resource owner.
