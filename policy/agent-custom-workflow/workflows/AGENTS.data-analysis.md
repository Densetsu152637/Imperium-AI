# Agent Instructions: Data Analysis Workflow

## Scope and inputs

Use this workflow for quantitative analysis, dataset transformations, statistical interpretation, or numerical conclusions, including plans to assess a statistical claim when its dataset is missing. It owns analytical methods; use relevant spreadsheet, database, plotting, or other artifact skills for implementation and presentation.

Load [research](AGENTS.research.md) when external source assessment or qualifying research interpretation is also required; its exception applies only to assignments meeting its eligibility. Assigned analysts use the [analyst profile](../profiles/AGENTS.analyst.md).

- Define the question, intended decision, population, observation unit, metrics, and denominators before selecting methods; distinguish exploratory analysis from a prespecified test.
- Preserve source data; write transformations and outputs only to assigned paths/resources. Handle sensitive data under [environment configuration](../guides/environment-configuration.md#secret-handling).
- Record material data-quality issues (missingness, duplicates, coverage, selection bias, inconsistent units) before drawing conclusions.

## Analysis

1. Build a reproducible transformation path from inputs to results. Record filters, exclusions, imputations, join cardinality, aggregation level, and manual changes. Never silently treat missing values as zero or discard inconvenient observations.
2. Report effect sizes, denominators, units, and uncertainty. Label assumptions needed for causal interpretation.
3. For predictive work, choose evaluation splits appropriate to time, entities, and deployment; keep preprocessing and tuning from leaking evaluation information; compare against a meaningful baseline.
4. Check sensitivity to consequential choices and account for multiple comparisons. Label post-hoc findings exploratory; do not select only favorable analyses.

## Deliver and validate

- Check decisive computations independently where practical (join/count invariants, reconciliation to source totals, small known cases or an alternative calculation).
- Validate the delivered tables, formulas, charts, and conclusions under [shared validation](../guides/validation.md#shared-validation-rules). Load [vision](AGENTS.vision.md) for rendered visual acceptance; appearance alone cannot establish numerical correctness.
- Deliver the answer, reproducible calculation paths, dataset revisions, consequential transformations, uncertainty, and material quality/access limitations. Charts must expose labels, scales, and units; conclusions must trace to their calculations.
- Distinguish confirmed results from assumptions and analyses left unrun. Set a bounded checkpoint under [shared execution defaults](../AGENTS.md#execution-defaults), or [manager scheduling](manager-delegation.md#budgets-and-scheduling) for delegated work; stop when more analysis cannot change the decision.
