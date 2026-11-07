# Subagent profiles

Profiles describe execution focus, not delegation authority. Every leaf assignment names exactly one profile from these tables; if none fits exactly, name the closest and report the recurring gap as a candidate profile. Choose [manager or leaf](../workflows/manager-delegation.md#manager-or-leaf-selection) first; a leaf loads the [leaf workflow](../workflows/AGENTS.leaf.md). Managers may use these profiles for direct execution without becoming leaves.

| Assignment | Profile | Expected result |
| ---------- | ------- | --------------- |
| Locate and explain relevant code/evidence | [Explorer](AGENTS.explorer.md) | Focused path/dependency map |
| Implement an agreed change | [Implementer](AGENTS.implementer.md) | Cohesive change and focused checks |
| Write or update docs from the final implementation | [Documenter](AGENTS.documenter.md) | Doc paths or "no documentation impact" reason, plus defects found |
| Author tests and regression reproductions | [Tester](AGENTS.tester.md) | Behavior tests, run results, coverage gaps |
| Execute acceptance checks | [Validator](AGENTS.validator.md) | Criterion-specific verdicts |
| Challenge correctness and consequential assumptions | [Critic](AGENTS.critic.md) | Substantiated findings and resolving checks |

Combine the generic profiles with the [applicable domain workflows](../AGENTS.md#workflows); the manager supplies owners in the instruction manifest. A domain workflow creates no new role and grants no permission to change the artifact.

## Domain profiles

Each domain profile pre-binds one workflow (or modality set) to a recurring assignment. It still creates no new role or permission; the workflow and assignment limits govern.

| Assignment | Profile | Workflow | Expected result |
| ---------- | ------- | -------- | --------------- |
| Gather and assess external evidence for a subquestion | [Researcher](AGENTS.researcher.md) | [Research](../workflows/AGENTS.research.md) | Cited evidence ledger with confidence |
| Reproduce and establish a failure's cause | [Debugger](AGENTS.debugger.md) | [Debugging](../workflows/AGENTS.debugging.md) | Reproduction, evidenced cause, proposed repair |
| Analyse data and statistical claims | [Analyst](AGENTS.analyst.md) | [Data analysis](../workflows/AGENTS.data-analysis.md) | Reproducible answer with uncertainty |
| Design options and decide on an architecture | [Architect](AGENTS.architect.md) | [Architecture](../workflows/AGENTS.architecture.md) | Decision record and migration plan |
| Review threats and vulnerabilities | [Auditor](AGENTS.auditor.md) | [Security](../workflows/AGENTS.security.md) | Substantiated findings and coverage |
| Drive real user journeys | [Navigator](AGENTS.navigator.md) | [Browser](../workflows/AGENTS.browser.md) | Per-journey verdicts with evidence |
| Inspect actual visual, audio, or video media | [Inspector](AGENTS.inspector.md) | [Vision](../workflows/AGENTS.vision.md), [Audio](../workflows/AGENTS.audio.md), [Video](../workflows/AGENTS.video.md) | Per-artifact verdicts with coverage |

Add a profile only for a recurring distinct responsibility.
