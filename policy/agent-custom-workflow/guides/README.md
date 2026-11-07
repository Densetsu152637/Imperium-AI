# Shared guidance

- [Root implementation planning](../workflows/root-planning.md#implementation-planning): staged plan documents in the project for substantial initially vague prompts, consolidated with existing docs, resumed by later agents, and executed in the same turn.
- [Root review](../workflows/root-review.md#gates): independent critic gates on plans and implementations, triage and revision loop, a post-convergence improvement pass, and feeding recurring findings into the failure catalog.
- [Host adapters](host-adapters.md): Claude Code and Codex tool mappings for dispatch, context, effort, worktrees, and celebration.
- [Subagent coordination](subagent-coordination.md): compact/expanded contracts, manifests and retained context; [recovery](subagent-recovery.md) for stalls, limits and ownership transfer.
- [Coding standards](coding-standards.md): core implementation, documentation alignment, and completion requirements.
- [Coding conventions](coding-conventions.md): conditional language/design/async defaults under the coding standards loading rule.
- [Pattern guides](patterns/README.md): function reuse and function/class structure, plus object-oriented, data-oriented and functional patterns, each linking its relevant languages and external references.
- [Usage accounting](usage-accounting.md): requested/substantial-task metrics, aggregation, and numeric budgets.
- [Git usage](git-usage.md): local preflight, branches and commits; [worktrees](git-worktrees.md): isolation, integration and cleanup; [publication](git-publication.md): starting-main authority, root pushes, PRs and merges.
- [GitHub Actions](github-actions.md): workflow authoring, actionlint/pre-commit and hosted controls; [act rehearsal](github-actions-local.md) when supported local execution applies.
- [Container usage](container-usage.md): Compose-first discovery and service lifecycle.
- [Environment configuration](environment-configuration.md): environment files, variables, and secrets.
- [Validation](validation.md): shared check evidence; Compose readiness and sequence are in [container usage](container-usage.md).
- [Subagent profiles](../profiles/README.md): portable generic and domain (researcher, debugger, analyst, architect, auditor, navigator, inspector) assignments.
- [Failure catalog](../learning/README.md): evidence-backed reusable corrections and their applicability.

Load applicable sections under [shared discovery](../AGENTS.md#context-and-discovery). Resolve placeholders and commands against the consuming project's files.
