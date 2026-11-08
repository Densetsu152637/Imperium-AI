# Plan: Imperium Dispatcher, an open-weight delegation model

Revision: 2026-11-08 (first draft, no stage started)

A small open-weight model, fine-tuned to be the **root agent** of an Imperium session on a
16 GB GPU: it reads the task, plans, writes compact briefs, and hands the work to hosted
(and local) subagents through the existing `spawn_agent` / `await_tasks` tools, instead of
doing the work itself. The daemon keeps doing what it already does well — routing, keys,
budgets, waiting at zero token cost — so the model only has to learn the decisions.

## Goals

Each goal is checked by the evaluation harness from stage 2 (`training/dispatcher/eval`),
against the frozen held-out set, unless stated otherwise. Numeric targets are provisional
until stage 2 measures the baselines; revise them there, once, and record why.

| ID | Goal | Verified by |
| -- | ---- | ----------- |
| G1 | ≥ 98 % of the model's tool calls parse and validate against the protocol schemas | `eval --metric tool_validity` |
| G2 | Delegation decisions (delegate or answer, how many subagents, tier and tags) match the reference on ≥ 85 % of held-out tasks, and beat the untuned base by ≥ 15 points | `eval --metric routing` |
| G3 | End-to-end task success with the Dispatcher as root is within 5 points of a frontier root model on the same task suite, at ≤ 50 % of that run's total cost | `eval --suite e2e` (cost from the session ledger) |
| G4 | Every brief the model writes is ≤ 2,000 tokens and self-contained (a judge model answers the subagent's task from the brief alone on ≥ 90 % of samples) | `eval --metric brief` |
| G5 | Zero credentials, keys, endpoint URLs, or auth scopes appear in model output across the whole held-out set | `eval --metric leakage` |
| G6 | The Q4_K_M GGUF serves 64K tokens per slot on a 16 GB GPU with peak VRAM ≤ 15.0 GB, and keeps ≥ 90 % on a 64K retrieval test (RULER-style) | `imperium models local` fit + `nvidia-smi` peak during `eval --suite longctx` |
| G7 | Weights, GGUF quants, model card, eval report, and training recipe are published on Hugging Face under Apache-2.0 | The Hub page lists all of them |
| G8 | A fresh install can select the Dispatcher as the session root with one config change, and docs 04, 12, and 13 describe it | `imperium chat` with `models.default = "llamacpp:imperium-dispatcher-…"`; docs diff |

## Scope and non-goals

- **In scope:** a fine-tuned 4–9B model; the dataset and eval harness that make it; the small
  engine features it needs (chunked delegation of large inputs, structured briefs); packaging
  for llama.cpp (all platforms) and vLLM (Linux/WSL2); publication.
- **Not in scope:** pre-training or architecture changes; a new runtime; new special tokens or a
  new wire protocol; letting the model handle secrets; making it a strong coder or writer
  itself (that is what the subagents are for).

## Answers to the guide's two questions

1. **Fine-tune or framework?** Both, but the framework already exists: Imperium is the
   orchestration framework (daemon, router, budgets, reactive delegation). The new work is a
   fine-tuned model trained on *Imperium's own tool schemas*, so no LangChain/AutoGen/CrewAI
   layer is needed.
2. **Language?** The orchestration stays in Rust (`imperium-core`, `imperium-models`). Training
   and evaluation are Python (Unsloth/TRL, `uv`-managed) under a new `training/dispatcher/`
   folder, the same split the media sidecar uses ([D8](../13-decisions.md)). This also seeds the
   "fine-tuning plugin (Unsloth)" roadmap candidate ([12](../12-roadmap.md)).

## Corrections to the optimisation guide

The guide's direction is sound (small model, smart token management, delegate the bulk), but
several specifics are wrong for this project or this hardware. This plan wins over the guide.

| Guide says | Problem | This plan |
| ---------- | ------- | --------- |
| FlashAttention-3 | Hopper-only (H100/H200); does not run on 16 GB consumer cards | llama.cpp `--flash-attn` (all backends); vLLM picks its own backend |
| vLLM with INT4 KV cache | vLLM offers FP8 KV, not INT4; and vLLM is Linux/WSL2-only here ([D5](../13-decisions.md)) | llama.cpp is the primary runtime: `--cache-type-k q8_0 --cache-type-v q8_0`; vLLM + AWQ + `--kv-cache-dtype fp8` on Linux |
| Raise RoPE base to 1M–5M | Current bases (Qwen3) already use 1M; changing θ needs long-context continued pre-training | Keep θ; extend with YaRN only if the base's native window is short, and verify (G6) |
| 4-bit GaLore *or* QLoRA in 16 GB | Fine for short sequences only; long-sequence training will not fit | QLoRA (Unsloth) locally at ≤ 16K tokens; the long-context stage on a rented 48–80 GB GPU |
| Payload carries endpoint and authorization scopes | Model output must never hold credentials; keys live in the keychain ([D7](../13-decisions.md)) | Model emits `tier`, `tags`, `location`, `requirements`; the router resolves endpoint and key |
| Model compresses a 100K log into a 2K "State Packet" | To compress 100K tokens the model must first hold them, which is the cost being avoided | The daemon stores large inputs as artefacts; the model delegates chunks (map), then merges reports (reduce); briefs are the state packet (G4) |
| `<WAIT_FOR_AGENT_ID>` token | Already solved: `await_tasks` suspends the loop at zero tokens and resumes on completion events | Train on `spawn_agent` / `await_tasks`; no new tokens |
| 64K–128K context "natively" | Memory capacity is not quality; small models degrade well before their window | Target 64K per slot, prove it (G6); beyond that, delegate |

## VRAM budget (16 GB, Qwen3-8B-class model, llama.cpp)

KV per token = layers × KV heads × head dim × 2 × bytes. For 36 × 8 × 128 × 2 = 73,728
elements: ~144 KiB at f16, ~77 KiB at q8_0, ~41 KiB at q4_0. Figures are estimates until the
memory planner and `nvidia-smi` measure them in stage 7.

| Component | 64K, q8_0 KV (target) | 128K, q4_0 KV (stretch) |
| --------- | --------------------- | ----------------------- |
| Weights (Q4_K_M) | ~5.0 GB | ~5.0 GB |
| KV cache | ~5.0 GB | ~5.3 GB |
| Compute buffers + CUDA context | ~1.5 GB | ~2.0 GB |
| Total | ~11.5 GB | ~12.3 GB |

The headroom is deliberate: a second slot (two concurrent sessions) or a small local subagent
fits beside it. A 4B base halves the weights and roughly halves the KV, which is worth testing
in stage 3.

## Assumptions

- The target machine has one 16 GB NVIDIA GPU (Windows, so llama.cpp is the runtime).
- Training beyond 16K-token sequences rents cloud GPUs; budget to be agreed (open question).
- The base model is chosen in stage 3 from Apache-2.0 instruct models of 4–9B with native tool
  calling and ≥ 32K native context (Qwen3-8B / Qwen3-4B-Instruct-2507 class, or a newer
  release that beats them on the stage-2 harness). Llama-licensed bases are excluded because
  their licence conflicts with an Apache-2.0 release.
- Teacher traces come from models whose terms allow training on their outputs (open-weight
  teachers such as large Qwen/DeepSeek/GLM releases). Closed-API teachers are used only after
  checking their terms (open question).

## Relevant paths

- `crates/imperium-core` — agent loop, `spawn_agent` / `await_tasks`, artefacts, compaction
- `crates/imperium-models` — router, model table, pricing
- `crates/imperium-local` — llama.cpp / vLLM supervision, memory planner, KV estimates
- `crates/imperium-protocol` — tool and event schemas (source of the training schemas)
- `policy/agent-custom-workflow` — delegation policy the model is trained to follow
- `training/dispatcher/` — new: dataset builders, training configs, eval harness (Python, `uv`)

## Related docs

[03 Agent engine](../03-agent-engine.md) (reactive delegation, task contract, context
management), [04 Models and routing](../04-models-and-routing.md), [08 Agent
policy](../08-agent-policy.md), [18 Workflow contract](../18-workflow-contract.md). Those docs
win on current behaviour; this plan wins on what the Dispatcher adds. Stage 9 folds the result
into them.

## Stage 1: Dispatch contract

- Objective: freeze what the model emits and sees, so data, training, and the engine agree.
  Export JSON Schemas for `spawn_agent` (brief, `requirements`, `budget`, `result`),
  `await_tasks`, and the new `delegate_chunks` (stage 2) from `imperium-protocol`; define the
  brief template (goal, context, constraints, acceptance, output format, ≤ 2K tokens); define
  the root system prompt the model is trained with.
- Depends on: none
- Owned paths: `crates/imperium-protocol`, `docs/03-agent-engine.md` (task contract section),
  `training/dispatcher/schemas/`
- Acceptance checks: schemas generated by `cargo test -p imperium-protocol` match the checked-in
  copies (drift check in `scripts/check-rust.sh`); brief template documented in 03.
- Status: pending
- Outcome:
- Handoff notes: no credentials or endpoint fields in any model-facing schema (G5).

## Stage 2: Engine support for large inputs and an eval harness

- Objective: (a) `delegate_chunks(artefact_id, chunk_tokens, brief, reduce?)` in
  `imperium-core`: splits a stored artefact on structure boundaries, spawns one subagent per
  chunk within the session's concurrency and budget, and optionally a reduce subagent; the root
  sees only the reports. Inputs over a threshold (default 75 % of the root's window) arrive as an
  artefact handle with an outline, never inline. (b) The eval harness: replays held-out tasks
  through `imperiumd` with recorded or fake subagents (deterministic) and, for `--suite e2e`, real
  hosted ones; scores G1–G6 into a JSON report.
- Depends on: stage 1
- Owned paths: `crates/imperium-core`, `crates/imperium-tools`, `training/dispatcher/eval/`
- Acceptance checks: `cargo test -p imperium-core delegate_chunks` passes (fake model server, a
  300K-token input never enters the root's context); `uv run eval --model <any> --dry-run`
  produces a report with every metric.
- Status: pending
- Outcome:
- Handoff notes: subagent recordings make evaluation cheap and repeatable; only `e2e` spends money.

## Stage 3: Baselines and base-model choice

- Objective: run the harness on 3–5 candidate bases (zero-shot, with the stage-1 prompt) and on a
  frontier root for the G3 reference; pick the base; set final numeric targets for G1–G4.
- Depends on: stage 2
- Owned paths: `training/dispatcher/eval/results/`, this plan (goal revision)
- Acceptance checks: a results table with every candidate × metric is committed; the choice and
  any goal revision are recorded here with reasons.
- Status: pending
- Outcome:
- Handoff notes: include a 4B candidate; if it is within 3 points of the 8B, prefer it (more KV room).

## Stage 4: Dataset

- Objective: ~20–50K training trajectories, disjoint from the held-out set by task family.
  Sources: (1) teacher traces — a strong permitted teacher plays root in real Imperium sessions on
  generated tasks (code, research, data, docs, ops), journals exported, kept only when the task
  succeeded under budget; (2) synthetic routing cases — task → reference decision from the
  router rules and policy, including "answer directly" and "refuse to send `exec` off-machine";
  (3) long-input cases built on `delegate_chunks`; (4) negative examples — leaked secrets,
  over-delegation, briefs missing context — rewritten into correct turns.
- Depends on: stages 1, 3
- Owned paths: `training/dispatcher/data/`
- Acceptance checks: `uv run data validate` — 100 % of samples parse against the stage-1 schemas;
  0 secret-pattern hits; dedup and held-out overlap check pass; a datasheet lists sources and licences.
- Status: pending
- Outcome:
- Handoff notes: store datasets on the Hub (private until release), not in Git.

## Stage 5: Supervised fine-tuning

- Objective: QLoRA SFT of the chosen base with Unsloth on the 16 GB GPU, sequences ≤ 16K,
  loss on assistant turns only; checkpoint sweep on the dev split.
- Depends on: stage 4
- Owned paths: `training/dispatcher/train/`
- Acceptance checks: the best checkpoint meets G1 and G2 on held-out; no regression > 2 points on a
  small general instruction-following set.
- Status: pending
- Outcome:
- Handoff notes: log runs with Trackio; record exact library versions in the recipe.

## Stage 6: Preference and outcome tuning

- Objective: GRPO (or DPO if compute is short) with verifiable rewards: schema validity, routing
  match, brief self-containment (judge), cost of the chosen plan from the router's prices,
  leakage penalty.
- Depends on: stage 5
- Owned paths: `training/dispatcher/train/`
- Acceptance checks: G1, G2, G4, G5 met; G3 measured on the `e2e` suite.
- Status: pending
- Outcome:
- Handoff notes: skip if stage 5 already meets G1–G5 and G3 is within target.

## Stage 7: Long context and packaging

- Objective: if the base's native window is < 64K, a short YaRN long-context stage on rented GPUs
  with long-input trajectories. Merge the adapter, export GGUF (Q4_K_M, Q5_K_M, Q8_0) and AWQ;
  measure memory and long-context quality.
- Depends on: stage 6 (or 5)
- Owned paths: `training/dispatcher/export/`
- Acceptance checks: G6 — `imperium models local` reports a GPU fit at 64K × 1 slot with q8_0 KV;
  measured `nvidia-smi` peak ≤ 15.0 GB during `eval --suite longctx`; retrieval ≥ 90 % at 64K.
- Status: pending
- Outcome:
- Handoff notes: teach the memory planner the KV cache type (q8_0/q4_0 change bytes per token)
  if it still assumes f16; that is a `crates/imperium-local` change.

## Stage 8: Imperium integration

- Objective: a bundled model-table seed entry and a "Dispatcher root" preset (session model =
  Dispatcher, `routing.prefer = "cheapest"`, cloud rows for `strong`/`frontier`); `imperium models
  pull` works for the published repo; the desktop Models panel shows it.
- Depends on: stage 7
- Owned paths: `crates/imperium-config`, `crates/imperium-models`, `apps/desktop` (preset UI)
- Acceptance checks: G8 — fresh `IMPERIUM_HOME`, one config line, `imperium chat` completes a
  delegation task; `bash scripts/check-rust.sh` and `scripts/check-web.sh` pass.
- Status: pending
- Outcome:
- Handoff notes:

## Stage 9: Release and documentation

- Objective: publish weights, quants, model card (intended use, limits, eval table, VRAM table,
  data sources), and recipe; fold this plan into docs 03, 04, 12 (roadmap row), and 13 (decision
  record); delete this plan.
- Depends on: stage 8
- Owned paths: `docs/`, Hugging Face repo
- Acceptance checks: G7 and G8; `markdownlint-cli2` clean.
- Status: pending
- Outcome:
- Handoff notes:

## Open questions

| Question | Owner | Needed by |
| -------- | ----- | --------- |
| Cloud GPU budget for long-context and RL stages | User | Stage 6 |
| Which teachers' terms allow training on outputs; are closed-API teachers in or out? | User | Stage 4 |
| Name and Hugging Face organisation for the release | User | Stage 9 |
| Train in this repo (`training/`) or a separate repository? | User | Stage 2 |
