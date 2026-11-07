# Agent Instructions: Vision Workflow

## Specialized capability ceiling

Managers only; workers skip.

Generic [ceiling](AGENTS.manager.md#capability-ceilings) and [selection](AGENTS.manager.md#model-and-effort-selection) rules apply except: assignments inspecting actual visual inputs may use up to Specialist under `light` and Frontier under `balanced`/`heavy`, with verified vision capability. Text-only summaries are not image QA; text-only and non-inspection work keep the general ceiling, so split mixed assignments.

## Visual inputs

Assigned inspectors use the [inspector profile](../profiles/AGENTS.inspector.md).

- Supply current outputs and comparisons as accessible image paths/references, plus intended style, viewing scale, acceptance criteria, and specific questions.
- For PDFs, slides, or UI, inspect actual rendered pages/screens. Do not judge unseen regions or inaccessible/insufficient-resolution inputs.
- Split by image batch, page, screen, or evaluation dimension, in batches small enough to detect required detail.

## Inspection and repair loop

1. Inspect pixels for the relevant defects. For interactive UI, also perform functional checks; screenshots alone cannot prove behavior.
2. Return per-artifact pass/fail/unable-to-verify findings with location, severity, observable evidence, and correction. Separate measured defects from preferences.
3. Route repairs through the immediate manager to the assigned editor, who produces a stable new revision. Reinspect changed assets and dependent visual regions under [shared validation rules](../guides/validation.md#shared-validation-rules).
4. Allow at most three repair/recheck retries after the initial failure, subject to tighter user/assignment limits. This replaces the [shared recovery default](../guides/subagent-recovery.md#stalls-and-recovery); other recovery mechanics remain there.

For inaccessible or inadequate inputs, report the exact limitation and complete unaffected checks; do not mark visual acceptance passed without usable visual evidence.
