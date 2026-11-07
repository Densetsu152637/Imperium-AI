# Agent Instructions: Architecture and Design Review Workflow

## Scope and inputs

Use this workflow for substantial technical design, interface boundaries, migrations, or review of consequential design choices, including comparison and migration plans before implementation. It owns design reasoning and decision records; [coding standards](../guides/coding-standards.md) own implementation conventions.

Assigned designers use the [architect profile](../profiles/AGENTS.architect.md).

- Establish the problem, requirements, non-goals, compatibility obligations, operational constraints, and measurable acceptance criteria. Identify accepted decisions and the owner of unresolved choices.
- Map the current architecture from actual sources (contracts, dependencies, data ownership, deployment boundaries); distinguish implemented behavior from aspirational documentation.
- Load [research](AGENTS.research.md), [data analysis](AGENTS.data-analysis.md), or [security review](AGENTS.security.md) when the decision needs their methods; each scoped exception retains its own eligibility.

## Alternatives and decision

1. Compare a small set of viable alternatives, including retaining or minimally extending the current design, against the same requirements.
2. Describe contracts, invariants, dependency direction, ownership, failure handling, and concurrency boundaries. Specify versioning, rollout, migration, and rollback where they affect the design; inspect callers and consumers before changing a shared contract.
3. Resolve consequential uncertainty with the smallest useful prototype, measurement, contract check, or focused critique. Mark projected performance and cost as estimates until measured; do not build multiple full implementations merely to compare ideas.
4. Select the simplest viable design that meets the criteria. Record reasons, rejected alternatives, tradeoffs, unresolved risks, and revisit conditions. Escalate requirement changes through the manager rather than silently broadening scope.

## Review and handoff

- The manager adds an independent [critic](../profiles/AGENTS.critic.md) leaf for consequential assumptions, hidden coupling, failure paths, migration feasibility, and operational burden; the authoring leaf only reports where critique is needed. Preserve unsettled disagreements.
- Produce a concise decision record plus affected interfaces, compatibility/migration plan, dependency-ordered implementation slices, ownership, and validation criteria. Use diagrams only when they clarify.
- Validate design claims and prototype evidence under [shared validation](../guides/validation.md#shared-validation-rules). Design acceptance does not establish that the implementation works; hand implementation to the assigned editor and verify the integrated result separately.
- Set a bounded checkpoint under [shared execution defaults](../AGENTS.md#execution-defaults), or [manager scheduling](manager-delegation.md#budgets-and-scheduling) for delegated work. Stop when decision-relevant uncertainty is resolved or cannot be reduced within scope; name the exact remaining decision/input.
