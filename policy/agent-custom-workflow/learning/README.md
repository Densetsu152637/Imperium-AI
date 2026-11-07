# Failure catalog maintenance

This guide owns selection and maintenance of reusable failure lessons. Task-local state remains in [coordination records](../guides/subagent-coordination.md#context-lifecycle). Entry fields are defined only in the [entry template](../templates/failure-entry.md).

## Using lessons

- At dispatch, scan the [catalog index](failure-catalog.md#index) for matching task/environment symptoms, load only relevant entries, verify applicability, and pass IDs/paths in the assignment brief.
- Lessons are fallible evidence subject to [shared instruction priority](../AGENTS.md#scope-and-policy-ownership). Recheck matching conditions and invalidation criteria against current sources before applying a correction.

## Maintaining lessons

- A leaf returns candidate lessons through its result record. The manager triages them and assigns a single catalog writer when reusable learning is in scope.
- Root review-gate findings are also a lesson source ([root review](../workflows/root-review.md#learning)). Prefer recurring patterns or costly avoidable failures; consolidate duplicates by cause and applicability. Do not promote transient errors or unsupported opinion.
- Complete the linked entry template. Distinguish observed facts, hypothesized causes, and proposed corrections. Mark a correction verified only after the recorded procedure passes against an identified revision/environment under [validation rules](../guides/validation.md#shared-validation-rules).
- Keep a small reproducible verification procedure. Promote a lesson into policy only when evidence supports broad applicability and policy editing is authorized.
- Recheck entries when assumptions change. Mark stale corrections candidate or superseded and link replacements; disclose inaccessible evidence.
- Use evidence locators, not raw logs. Sanitize under [secret handling](../guides/environment-configuration.md#secret-handling) and omit unrelated personal data.
- Use stable IDs such as `FAIL-001` and keep index links current. Split large catalogs into topical files without changing stable IDs.
