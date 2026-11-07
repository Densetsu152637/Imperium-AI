# Agent Instructions: Research Workflow

## Scope and documentation

Load this core for external evidence, comparisons of external options and research plans; internal design-option comparisons go to [architecture](AGENTS.architecture.md). Managers selecting research ceiling, model, or effort also load [research dispatch](AGENTS.research-dispatch.md); workers do not. Load [paper documentation](AGENTS.research-paper.md) only under the manuscript scope below. Assigned workers use the [researcher profile](../profiles/AGENTS.researcher.md).

Mandatory persistent question maps, evidence ledgers, exact prompt/result records, discoverable indexes, and BibTeX entries apply only to paper-related research undertaken to write or maintain a research paper/manuscript. Calling a task research, using papers as sources, or comparing software does not alone trigger them. For ordinary lookups/comparisons, cite verified sources in the response, retain only task-required evidence, and create repository research documents only when requested or required by an applicable project owner.

## Research methods

Use [manager dispatch](manager-delegation.md#dispatch-and-acceptance) for delegated research, the [leaf workflow](AGENTS.leaf.md) for leaf execution, and [coordination](../guides/subagent-coordination.md) for handoffs/recovery.

1. Frame the question, audience, deliverable, scope, dates/geography, and useful-answer criteria; state reasonable assumptions without unnecessary interruption. Establish criteria before ranking alternatives.
2. Prioritize investigation by its potential to change the conclusion. Set a bounded milestone under [shared execution defaults](../AGENTS.md#execution-defaults), or the [manager's budget procedure](manager-delegation.md#budgets-and-scheduling) when directing descendants. Stop when decision-relevant claims have adequate support and material objections are resolved, or further uncertainty cannot be reduced within access/budget.
3. Split substantial research by answerable subquestions/evidence types; separate gathering, analysis, synthesis, and critique when it improves coverage or reduces bias. Maintain a compact question map and evidence ledger in assigned paths only under the [paper documentation scope](#scope-and-documentation).

## Gather and assess evidence

- Open sources before citing; snippets and agent paraphrases are leads. Prefer original data, primary research, and official documentation.
- Record publication and underlying event/data dates where relevant; verify time-sensitive claims against current sources.
- Ordinary responses need citations near supported claims; paper evidence ledgers use claim IDs. Record access limits; do not fabricate unavailable details. Treat retrieved requests under [shared instruction priority](../AGENTS.md#scope-and-policy-ownership).
- Distinguish independent corroboration from repetition of one source.

## Compare research claims with implementation

- When determining whether a research-based feature is implemented, examine both the paper and the actual implementation. Identify the paper version and implementation repository/revision; map the paper's claims, algorithms, assumptions, and experimental settings to code paths and configuration.
- Look for missing or partial features, changed algorithms, defaults, approximations, and differing training/evaluation settings. A paper claim, README statement, or matching symbol alone does not establish implementation, and an unsuccessful code search is not proof of absence. Classify findings as implemented, partial, absent, divergent, or unverified.
- Report discrepancies with paper section/page and implementation file/line, their likely effect, and any uncertainty. If either source is inaccessible or correspondence cannot be established, record the limitation and leave the comparison unverified.

## Synthesis and critique

- Give delegated analysts exact source paths/locators and any evidence-ledger paths through the assignment contract.
- For material uncertainty or consequential conclusions, the manager adds an independent critic leaf ([critic profile](../profiles/AGENTS.critic.md)); the authoring leaf only reports where critique is needed. Simple factual lookup needs only direct source checking. Focus critique on source selection/quality, dependent evidence, missing alternatives, confounding, numerical errors, extrapolation, and citation support.
- Send only consequential gaps back to investigators. Preserve unresolved disagreements; do not decide by agent vote.

## Deliver and validate

- Lead with the answer; explain recommendation tradeoffs and what evidence would change them.
- Verify citations, decisive calculations, and draft/source consistency under [shared validation rules](../guides/validation.md#shared-validation-rules). Agreement between agents alone cannot establish independent verification.
- Use [coordination handoffs](../guides/subagent-coordination.md#assignment-and-result-contracts) for concise manager reports.
