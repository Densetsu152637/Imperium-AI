# Agent Instructions: Security Review Workflow

## Specialized capability ceiling

Managers only; workers skip.

Generic [ceiling](AGENTS.manager.md#capability-ceilings) and [selection](AGENTS.manager.md#model-and-effort-selection) rules apply except: bounded difficult threat modeling or adversarial analysis of consequential vulnerabilities (authorization bypass, privilege escalation, interacting trust-boundary failures) may use up to Specialist under `light` and Frontier under `balanced`/`heavy`. Routine checklist review, scanner triage, dependency inventory, managers, and code repair keep the general ceiling, so split mixed assignments.

## Scope and threat model

Assigned reviewers use the [auditor profile](../profiles/AGENTS.auditor.md).

- Verify access to the relevant code and configuration. Establish the review objective, authorized targets/environment, revision, and expected controls. A focused review is not a whole-system assurance claim.
- Identify assets, attacker capabilities, entry points, trust boundaries, and sensitive data flows from actual sources. Load [architecture](AGENTS.architecture.md) when design reasoning is also needed and [research](AGENTS.research.md) for external advisory/source verification.
- Prefer source review and controlled local tests with synthetic data. Active probing must stay within authorized targets, methods, and resources; a security assignment does not grant access or mutation permissions. Handle credentials and sensitive output under [environment configuration](../guides/environment-configuration.md#secret-handling).

## Review and substantiation

1. Trace untrusted inputs and authority decisions through the system: authentication, server-side authorization, tenant/object boundaries, validation/encoding, secret exposure, and resource limits as applicable.
2. Examine bypass and failure paths (alternate endpoints, state transitions, replay/races, default/error behavior). Distinguish an absent visible control from a verified exploitable path.
3. Treat scanner findings as leads: verify affected versions, configuration, reachability, prerequisites, and mitigations; record false positives. Do not claim exploitability from a package name or severity label alone.
4. Substantiate each finding with source/behavior locators, attacker prerequisites, attack path, impact, severity rationale, confidence, and the smallest repair and verification. Label unconfirmed candidates; avoid reproducing secrets or unnecessary exploit data.

## Repair and acceptance

- Route remediation through the manager to the assigned implementer; keep review separate from implementation ownership. Prefer fixes at the trust boundary and inspect dependent paths that could retain the bypass.
- Verify, under [shared validation](../guides/validation.md#shared-validation-rules), the failure on the vulnerable revision where feasible, rejection on the repaired revision, and continued success for legitimate authorized behavior.
- Deliver the threat model, tested revision/surfaces, findings, coverage, residual risk, and checks left unrun. A no-findings verdict applies only to the inspected scope; it does not establish the system is secure.
