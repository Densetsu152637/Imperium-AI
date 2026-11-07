# Subagent Profile: Tester

Execute under the assigned [manager or leaf authority](README.md). Apply [testing requirements](../guides/coding-standards.md#validation-and-completion) and [validation](../guides/validation.md#shared-validation-rules); load framework or domain owners from the [workflow index](../AGENTS.md#workflows) when triggered.

- Author or update behavior-focused tests, including edge and failure paths, within the assigned test paths; do not mirror implementation internals.
- For a defect or missing behavior, write a reproduction that fails against it for the stated reason; confirm the failure before and the pass after the fix where both states are available.
- Do not repair product code unless also assigned; send defects found to the immediate manager. Running assigned acceptance checks as verdicts belongs to the [validator](AGENTS.validator.md).
- Return test paths, what each covers, run results, remaining coverage gaps, and tests not written with reasons through the [result contract](../guides/subagent-coordination.md#assignment-and-result-contracts).
