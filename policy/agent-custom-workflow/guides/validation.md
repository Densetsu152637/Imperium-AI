# Validation

This guide owns common validation evidence. Discover applicable methods through the [workflow index](../AGENTS.md#workflows). Load [container usage](container-usage.md#compose-validation-sequence) only for container/Compose work; it owns identity, readiness and the Compose check sequence.

## Shared validation rules

- Wait for the project's documented readiness condition before dependent checks. If healthchecks do not represent required readiness, use the documented application probe or relevant logs.
- Discover required commands and expected outcomes from READMEs/configuration and the assignment brief. Use [coding standards](coding-standards.md#validation-and-completion) to decide code-test coverage.
- Check the actual artifact/revision. Capture command/procedure, exit status, tested revision, and decisive evidence/log locator. Direct work may report concisely; delegated work uses the selected [result schema](../templates/subagent-result.md). Non-code review must identify the artifact checked.
- Distinguish passed, failed, and unrun checks. Inaccessible inputs, unavailable checks, stale evidence, or insufficient coverage are unverified, not passed. Do not weaken a check to obtain success; route scope changes through the manager.
- Validate the integrated result; isolated passing branches cannot establish their combination passes. Rerun affected checks after relevant changes; earlier approval does not approve later revisions.
- Start with the smallest check set covering the changed behavior and required gates; broaden only for failures, new changes, unresolved concerns, or a mandatory check. Once acceptance passes, do not repeat checks for reassurance; never omit required CI/review gates to save tokens.
- Report material limitations and unresolved failures. Functional acceptance needs observed behavior; domain methods and coverage requirements remain in the applicable workflows.
