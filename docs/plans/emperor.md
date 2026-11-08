# Plan: Emperor, an open-weight delegation model

Revision: 2026-11-08 r7 (r2: local-only training, the name Emperor, separate repositories,
teacher licences; r3: strategy and planning from vague prompts, clarify-or-research, G10–G11;
r4: context tiers from 64K to 256K and up, G12; r5: one loaded model shared by many
sessions, G13, stage 2c; r6: a 192K tier; r7: subagent-first model swapping, G14, stage 2d;
task-class routing and single-call cloud tasks, G15, stage 2e; stage 0 and stage 1 in progress)

**Emperor** is a small open-weight model, fine-tuned to be the **root agent** of an Imperium
session on a 16 GB GPU. It reads the task, plans, writes compact briefs, and hands the work to
hosted (and local) subagents through the existing `spawn_agent` / `await_tasks` tools instead
of doing the work itself. The daemon keeps doing what it already does well (routing, keys,
budgets, waiting at zero token cost), so the model only has to learn the decisions.

## Character: a master strategist

Emperor's defining skill is **strategy**: turning the vaguest prompt into the most complete,
executable plan, then running it through others.

- **Decompose.** From a one-line request it infers the real outcome, sets measurable goals, and
  breaks the work into dependency-ordered stages with owners (subagents), acceptance checks, and
  risks, using Imperium's plan tools (`update_plan` now; `plan.propose` / `plan.advance` as they
  land). Independent steps become parallel subagent tasks.
- **Fill gaps itself first.** For every unknown it decides: infer it from context or a
  sensible default, look it up (`web_search` / `web_fetch`, reading the project, or a research
  subagent), or ask. Most gaps close without bothering the user; each assumption it makes is
  written into the plan.
- **Ask only what matters.** It calls `ask_user` only for decisions that are the user's to make
  or that would change the plan materially and cannot be researched (scope, budget, taste,
  authority), batched into one short set of questions with a recommended answer each, and keeps
  working on what does not depend on the answers.
- **Re-plan on evidence.** When a subagent's report contradicts an assumption, it revises the
  plan and says what changed.

G10 and G11 measure this; stage 4 builds the data for it.

## Repositories

Work is split across three places, so no one basket holds all the eggs:

| Where | Holds | Status |
| ----- | ----- | ------ |
| This repository (Imperium-AI) | Engine features Emperor needs (stages 1, 2a, 7), integration, docs, and this plan of record | exists |
| `Densetsu152637/emperor` (GitHub, new) | Training code: dataset builders, training recipes, eval harness, export scripts (Python, `uv`) | created (private) in stage 0 |
| `Imperium.AI/emperor` (Hugging Face, placeholder org) | Released weights, model card, eval report; GGUF quants in `Imperium.AI/emperor-GGUF`; private datasets in `Imperium.AI/emperor-traces` | to create in stage 8 |

When the `emperor` repository exists, stages 2b to 6 move into its own `docs/plans/` and this
plan links to them; until then this file is the single plan of record.

## Goals

The eval harness (stage 2b) checks each goal against the frozen held-out set, unless stated
otherwise. Numeric targets are provisional until stage 3 measures the baselines; revise them
there, once, and record why.

| ID | Goal | Verified by |
| -- | ---- | ----------- |
| G1 | ≥ 98 % of the model's tool calls parse and validate against the protocol schemas | `eval --metric tool_validity` |
| G2 | Delegation decisions (delegate or answer, how many subagents, tier and tags) match the reference on ≥ 85 % of held-out tasks, and beat the untuned base by ≥ 15 points | `eval --metric routing` |
| G3 | End-to-end task success with Emperor as root is within 5 points of a frontier root on the same suite, at ≤ 50 % of that run's total cost | `eval --suite e2e` (cost from the session ledger) |
| G4 | Every brief is ≤ 2,000 tokens and self-contained (a judge model answers the subagent's task from the brief alone on ≥ 90 % of samples) | `eval --metric brief` |
| G5 | Zero credentials, keys, endpoint URLs, or auth scopes in model output across the held-out set | `eval --metric leakage` |
| G6 | Every supported [context tier](#context-tiers) (64K, 96K, 128K, 192K, 256K) runs within its listed VRAM target with its listed KV cache type, and scores ≥ 90 % on a retrieval test (RULER-style) at its full length (≥ 85 % at 192K and 256K) | `imperium models local` fit per tier, plus the `nvidia-smi` peak during `eval --suite longctx --context-tier <tier>` |
| G7 | Weights, GGUF quants, model card, eval report, and recipe are on `Imperium.AI/emperor` under Apache-2.0, and every teacher's licence permits it | The Hub pages; the datasheet's licence table |
| G8 | A fresh install selects Emperor as the session root with one config change, and docs 04, 12, and 13 describe it | `imperium chat` with `models.default = "llamacpp:emperor-…"`; docs diff |
| G9 | Every training stage runs on the local 16 GB GPU; no rented GPUs | Training logs record the device; the recipe lists no cloud GPU step |
| G10 | From vague one-line prompts, Emperor's plans score ≥ 4.0 of 5 on a rubric (measurable goals, complete decomposition, correct dependencies, stated assumptions, executable steps) judged blind against a frontier root's plans, and score no worse than them on ≥ 70 % of prompts | `eval --metric plan` |
| G11 | On prompts with labelled gaps, Emperor asks the user about ≥ 90 % of the gaps labelled "user's decision", and about ≤ 10 % of those labelled "researchable" or "safe default", which it resolves itself and records as assumptions; it never asks more than one batch before starting independent work | `eval --metric clarify` |
| G12 | Emperor adapts to its tier: on the held-out suite, task success at the 64K tier is within 5 points of the 256K tier (it delegates and chunks earlier instead of failing), and no tier ever overflows its window | `eval --suite heldout --context-tier 64k` vs `--context-tier 256k`; overflow count in the report |
| G13 | One loaded Emperor serves many sessions: with a single slot, three sessions whose roots are waiting on subagents all make progress, because no agent holds a slot while it waits; a root resuming on a different slot re-reads ≤ 10 % of its context when the host prompt cache is on | `cargo test -p imperium-core shared_slot`; llama.cpp `timings.prompt_n` vs the context length on resume |
| G14 | On a GPU too small for Emperor and a local subagent model together, a delegated local task runs on the GPU: the idle orchestrator is swapped out (its KV saved), the subagent finishes, and the orchestrator is reloaded with its context restored (re-reading ≤ 10 % of it); a running subagent is never evicted for the orchestrator | `cargo test -p imperium-local swap` (fake launcher, budget fits one model); a measured run on the 16 GB GPU logs the swap times and `timings.prompt_n` on resume |
| G15 | Delegations follow task class: on a labelled set, ≥ 90 % of menial tasks (refactoring, tests, UI) go to local small models and ≥ 90 % of hard tasks (deep architecture, maths) go to a cloud frontier model; ≥ 80 % of frontier tasks complete in one model call | `eval --metric routing` per class; call counts per task from the session ledger |

## Scope and non-goals

- **In scope:** a fine-tuned 4–8B model, the dataset and eval harness that make it, the small
  engine features it needs (chunked delegation of large inputs, structured briefs), packaging
  for llama.cpp (all platforms) and vLLM (Linux/WSL2), and publication.
- **Not in scope:** pre-training or architecture changes (see [NCM](#next-concept-modeling-ncm)),
  a new runtime, new special tokens or a new wire protocol, letting the model handle secrets, or
  making it a strong coder or writer itself (that is what the subagents are for).

## Decisions so far

1. **Fine-tune or framework?** Both, but the framework already exists: Imperium is the
   orchestration layer. Emperor is trained on *Imperium's own tool schemas*, so no
   LangChain, AutoGen, or CrewAI layer is needed.
2. **Language.** The orchestration stays in Rust (`imperium-core`, `imperium-models`); training
   and evaluation are Python (Unsloth and TRL, `uv`-managed) in the `emperor` repository.
3. **Local-only training (G9).** No cloud GPU budget, so nothing may need one. That rules out a
   long-context training stage and makes the base model's *native* context decisive.
4. **Base model: Qwen3-4B-Instruct-2507 is the lead candidate.** Apache-2.0, native
   262,144-token context (`max_position_embeddings`, no RoPE scaling needed), and small enough
   to QLoRA-train with long sequences on 16 GB. Qwen3-8B (Apache-2.0, 32K native, 131K with YaRN)
   is the fallback if the 4B is more than 3 points worse in stage 3. Newer bases are welcome if
   they beat both on the stage-3 harness and are Apache-2.0 or MIT.
5. **Teachers** come only from the [allowed list](#teacher-licences).

## Corrections to the optimisation guide

The guide's direction is sound (small model, smart token management, delegate the bulk), but
several specifics are wrong for this project or this hardware. This plan wins over the guide.

| Guide says | Problem | This plan |
| ---------- | ------- | --------- |
| FlashAttention-3 | Hopper-only (H100/H200); it does not run on 16 GB consumer cards | llama.cpp `--flash-attn` (all backends); vLLM picks its own backend |
| vLLM with an INT4 KV cache | vLLM offers an FP8 KV cache, not INT4; and vLLM is Linux/WSL2-only here ([D5](../13-decisions.md)) | llama.cpp is the primary runtime: `--cache-type-k q8_0 --cache-type-v q8_0`; vLLM with AWQ and `--kv-cache-dtype fp8` on Linux |
| Raise the RoPE base to 1M–5M | Changing θ needs long-context continued pre-training; the lead base already ships θ = 5M with a 262K window | Keep the base's θ; no context-extension training |
| 4-bit GaLore *or* QLoRA in 16 GB | True for short sequences only | QLoRA (Unsloth) on the 4B base; sequence length set by measured VRAM in stage 5 |
| Payload carries the endpoint and authorization scopes | Model output must never hold credentials; keys live in the keychain ([D7](../13-decisions.md)) | The model emits `tier`, `tags`, `location`, and `requirements`; the router resolves the endpoint and key |
| The model compresses a 100K log into a 2K "State Packet" | To compress 100K tokens the model must first hold them, which is the cost being avoided | The daemon stores large inputs as artefacts; the model delegates chunks (map), then merges the reports (reduce); briefs are the state packet (G4) |
| A `<WAIT_FOR_AGENT_ID>` token | Already solved: `await_tasks` suspends the loop at zero tokens and resumes on completion events | Train on `spawn_agent` / `await_tasks`; no new tokens |
| 64K–128K context "natively" | Memory capacity is not quality; small models degrade well before their window | Each [context tier](#context-tiers) is measured for quality, not just fit (G6); beyond a tier's window, delegate |

## Context tiers

Emperor ships as **one set of weights** that runs at several active-context sizes, so weaker
hardware runs a smaller window instead of a different model. A tier is a runtime setting (the
per-slot context and KV cache type llama.cpp is started with), not a separate checkpoint. All
tiers up to 256K are inside the base's native 262,144-token window, so none needs RoPE scaling.

KV cache per token, for the lead base's attention shape (36 layers × 8 KV heads × 128 head dim =
73,728 elements; Qwen3-8B has the same shape): about 144 KiB at f16, 77 KiB at q8_0, and 41 KiB
at q4_0. Totals below assume the 4B at Q4_K_M (~2.5 GB of weights) plus 1.5–2.5 GB of compute
buffers and CUDA context. They are estimates until stage 6 measures them; the 8B fallback
adds ~2.5 GB to every row.

| Tier | KV at q8_0 | Total at q8_0 | KV at q4_0 | Total at q4_0 | Smallest GPU (KV type) | Status |
| ---- | ---------- | ------------- | ---------- | ------------- | ---------------------- | ------ |
| 32K (floor, optional) | ~2.6 GB | ~6.6 GB | ~1.4 GB | ~5.4 GB | 6 GB (q4_0) | Optional, for 6 GB cards |
| 64K | ~5.1 GB | ~9.1 GB | ~2.7 GB | ~6.7 GB | 8 GB (q4_0), 10 GB (q8_0) | Supported |
| 96K | ~7.7 GB | ~11.7 GB | ~4.1 GB | ~8.1 GB | 10 GB (q4_0), 16 GB (q8_0) | Supported |
| 128K | ~10.3 GB | ~14.3 GB | ~5.4 GB | ~9.4 GB | 12 GB (q4_0), 16 GB (q8_0) | Supported |
| 192K | ~15.4 GB | ~19.9 GB | ~8.2 GB | ~12.7 GB | 16 GB (q4_0), 24 GB (q8_0) | Supported |
| 256K | ~20.5 GB | ~25.0 GB | ~10.9 GB | ~15.4 GB | 16 GB (q4_0, tight), 32 GB (q8_0) | Supported |
| 512K and up | ~41 GB | ~46 GB | ~21.7 GB | ~26.7 GB | 32 GB (q4_0) | Experimental: beyond the native window, needs YaRN scaling; quality unverified, and the 16 GB development GPU cannot measure it |

- **Choosing a tier.** Imperium's memory planner picks the largest tier that fits the free
  VRAM, preferring q8_0 KV and dropping to q4_0 only when that fits a larger tier; the user can
  pin a tier. On CPU or partial offload, smaller tiers keep it usable but slow.
- **Tier-aware behaviour (G12).** Emperor is told its window in the system prompt
  (`context budget: 64K`) and trained across all tiers, so on a small tier it delegates and
  chunks earlier rather than filling its window. The 75 % artefact threshold in stage 2a follows
  the tier automatically.
- **Weight quants are a second axis.** Q4_K_M is the default; Q5_K_M and Q8_0 trade VRAM for
  quality on bigger cards, and IQ4_XS or Q3_K_M can rescue a tier on a card just short of it.
  The model card publishes the full tier × quant table.

## Teacher licences

Training traces come from teacher models playing root in real Imperium sessions. Researched
2026-11-08 from the licence files on Hugging Face and the providers' terms. This is not legal
advice; re-check a teacher's licence before using it.

| Teacher | Licence | Allowed? | Why |
| ------- | ------- | -------- | --- |
| Qwen3 family (e.g. Qwen3-235B-A22B-Instruct-2507) | Apache-2.0 | Yes | No restriction on outputs |
| DeepSeek-V3.2 | MIT | Yes | No restriction on outputs |
| GLM-4.6 | MIT | Yes | No restriction on outputs |
| gpt-oss-120b / gpt-oss-20b | Apache-2.0 plus a usage policy (comply with the law) | Yes | gpt-oss-20b also fits 16 GB, so it is a free *local* teacher |
| Mistral Small 3.2 and other Apache-2.0 Mistral models | Apache-2.0 | Yes | No restriction on outputs |
| Kimi K2 | Modified MIT | Yes | Only adds UI attribution above 100M monthly users or $20M monthly revenue |
| Llama 3.x | Llama Community Licence | **No** | A model trained on Llama outputs must have a name starting with "Llama", which conflicts with "Emperor" |
| Gemma (≤ 3) | Gemma Terms of Use | **No** | Distilling from Gemma outputs makes the result a "Model Derivative" bound by Gemma's terms, not Apache-2.0 (Gemma 4 has its own licence; unchecked) |
| OpenAI, Anthropic, Google Gemini, and xAI APIs | Provider terms | **No** | Their terms forbid using outputs to develop competing models |

Two caveats:

- **The host's terms apply too.** Running an open model through a hosted API (OpenRouter,
  DeepInfra, Together, ...) adds that provider's terms on top of the model licence. Before
  stage 4, check each provider used for teacher traces. A teacher run locally (gpt-oss-20b)
  avoids this.
- **API spend is not GPU spend.** No cloud GPUs are rented, but hosted teachers and the G3
  end-to-end suite cost API tokens. Keep a ceiling (default $50 for the whole project, recorded
  in the datasheet) and prefer the local teacher where its quality is enough.

## Next-Concept Modeling (NCM)

Asked 2026-11-08: could Emperor use the NCM architecture instead? **Not for this version.** NCM
remains a research track (stage R), with Emperor kept ready to switch.

- **What it is.** [Next-Concept Modeling](https://arxiv.org/abs/2610.14162) (Khare et al.,
  Stanford and Together AI, October 2026) works in two stages. A multiscale residual VQ-VAE
  encodes text into K levels of discrete codes, from short coarse levels to a full-length fine
  one. A transformer then generates one level at a time, all codes within a level in parallel,
  and the VQ-VAE decoder turns the result back into tokens. At about 100M parameters it is 10–31×
  faster than diffusion language models of matched size, at equal or better quality, and needs
  4–5× fewer training tokens than they do.
- **Why it cannot be done now.**
  - **It is a different architecture, not a fine-tune.** It needs its own tokenizer (the
    VQ-VAE) and a generator trained from scratch; neither can be grafted onto a Qwen checkpoint
    with LoRA. The 16 GB, local-only budget (G9) cannot pre-train a usable base.
  - **No usable base exists.** The paper trains a separate ~100M tokenizer and generator per
    dataset (LM1B, OpenWebText, XSum, WMT14, Sudoku). These are research checkpoints, not
    instruction-tuned, tool-calling, long-context bases, and the paper compares against
    diffusion models, not against autoregressive models at scale.
  - **No runtime support.** Neither llama.cpp nor vLLM runs it, so Imperium could not serve it
    without a new runtime backend.
  - **Unproven for this job.** The output length is fixed per generation (the finest level has
    L codes), while tool calls vary in length. Every token passes through a lossy VQ
    reconstruction, and the paper itself notes that one wrong token can break code, which is just
    as true of JSON. Prompt conditioning with long (64K) contexts is untested.
  - **The speedup matters least here.** Emperor writes short outputs: tool calls and briefs of
    at most 2K tokens. Its cost is reading context (prefill), which NCM does not speed up.
- **How Emperor stays future-proof.** Everything that carries the value is independent of the
  architecture: the dispatch contract (stage 1), the dataset (stage 4), and the eval harness
  (stage 2b). When an open-weight NCM (or diffusion) base with tool calling and runtime support
  appears, it gets the same SFT data and is scored on the same harness against Emperor. Imperium
  only needs a new `RuntimeBackend` ([04](../04-models-and-routing.md#runtimes)) to serve it.

## Assumptions

- One 16 GB NVIDIA GPU on Windows, so llama.cpp is the runtime; the training stack runs under
  WSL2 or native Windows, whichever Unsloth supports better at stage 5.
- `Imperium.AI` is a placeholder Hugging Face organisation: confirm the name is valid and free
  when creating it (Hub names may not accept the dot), and rename before release if needed.

## Relevant paths

- `crates/imperium-core`: agent loop, `spawn_agent` / `await_tasks`, artefacts, compaction
- `crates/imperium-models`: router, model table, pricing
- `crates/imperium-local`: llama.cpp / vLLM supervision, memory planner, KV estimates
- `crates/imperium-protocol`: tool and event schemas (the source of the training schemas)
- `policy/agent-custom-workflow`: delegation policy the model is trained to follow
- `emperor` repository (new): `data/`, `train/`, `eval/`, `export/`, `schemas/` (vendored from
  this repository)

## Related docs

[03 Agent engine](../03-agent-engine.md) (reactive delegation, task contract, context
management), [04 Models and routing](../04-models-and-routing.md), [08 Agent
policy](../08-agent-policy.md), [18 Workflow contract](../18-workflow-contract.md). Those docs
win on current behaviour; this plan wins on what Emperor adds. Stage 8 folds the result into
them.

## Stage 0: Create the repositories

- Objective: create the GitHub repository `Densetsu152637/emperor` (Apache-2.0, `uv` project,
  CI running lint and the eval harness's dry run); reserve the Hugging Face organisation.
- Depends on: none
- Owned paths: the new repository
- Acceptance checks: `gh repo view Densetsu152637/emperor` succeeds; `uv run eval --help` runs in
  CI.
- Status: in progress (GitHub repository done; Hugging Face organisation not yet created)
- Outcome: `Densetsu152637/emperor` created **private** on 2026-11-08 with a `uv` project, an
  `eval` CLI (`--help`, and `--dry-run` printing the report shape), tests, `scripts/check.sh`,
  and a Checks workflow running it on pull requests and pushes to `main`.
- Handoff notes: the repository stays private until there is something to release; making it
  public is the user's call. Hugging Face organisations are created on the website, so the
  user creates `Imperium.AI` (or the final name) there.

## Stage 1: Dispatch contract

- Objective: freeze what the model emits and sees, so data, training, and the engine agree.
  Export JSON Schemas for every tool Emperor uses: `spawn_agent` (brief, `requirements`,
  `budget`, `result`), `await_tasks`, `update_plan`, `web_search`, `web_fetch`, and the new
  `delegate_chunks` and `ask_user` (stage 2a), from `imperium-protocol`. Define the brief
  template (goal, context, constraints, acceptance, output format, ≤ 2K tokens), the plan
  template (goals, stages with dependencies and acceptance checks, assumptions, open
  questions), the question format (batched, each with options and a recommended answer), the
  context-budget line that tells Emperor its tier, and
  the root system prompt Emperor is trained with. The `emperor` repository vendors the schemas
  at a pinned Imperium version.
- Depends on: none
- Owned paths: `crates/imperium-protocol`, `docs/03-agent-engine.md` (task contract section)
- Acceptance checks: schemas generated by `cargo test -p imperium-protocol` match the checked-in
  copies (drift check in `scripts/check-rust.sh`); the brief template is documented in 03.
- Status: in progress (branch `feat/dispatch-contract`, 2026-11-08)
- Outcome:
- Handoff notes: no credential or endpoint fields in any model-facing schema (G5).

## Stage 2a: Engine support for large inputs and questions (Imperium)

- Objective: (1) `delegate_chunks(artefact_id, chunk_tokens, brief, reduce?)` in
  `imperium-core` splits a stored artefact on structure boundaries and spawns one subagent per
  chunk, within the session's concurrency and budget, plus an optional reduce subagent; the
  root sees only the reports. Inputs over a threshold (default 75 % of the root's window)
  arrive as an artefact handle with an outline, never inline. (2) `ask_user` (documented in
  [05](../05-tools-and-isolation.md) but not yet implemented): pauses the agent at zero tokens
  until the user answers a batch of questions in any client, like `await_tasks`, while
  already-spawned subagents keep running.
- Depends on: stage 1
- Owned paths: `crates/imperium-core`, `crates/imperium-tools`, clients' question UI
- Acceptance checks: `cargo test -p imperium-core delegate_chunks` passes against a fake model
  server, and a 300K-token input never enters the root's context; `cargo test -p imperium-core
  ask_user` shows a paused root making no model calls until the answer arrives.
- Status: pending
- Outcome:
- Handoff notes:

## Stage 2c: One loaded model, many sessions (Imperium)

This is general Imperium work, useful for any local model; Emperor depends on it most, because
a root is busy only at the start of a prompt cycle (planning, delegating) and at the end
(merging reports, verifying), and sleeps in between.

**Today (checked 2026-11-08).** Discovery already works: the daemon has one `Supervisor`
(`crates/imperiumd/src/local.rs`), loaded models are keyed by id, and every session that asks
for `llamacpp:<id>` reuses the same `llama-server`; `models.local` reports `slots` and
`slots_in_use`. The problem is **slot holding**: `crates/imperium-core/src/agent.rs` takes the
slot lease once per *turn* ("held for the whole turn"), so a root sleeping in `await_tasks`, in
`ask_user`, or in a usage-limit pause keeps its slot, and with `--parallel N` just N sleeping
roots block every other session. Nothing passes `id_slot`, `cache_prompt`, or a host prompt
cache to llama.cpp, so a root that lands on another slot would re-read its whole context.

- Objective:
  1. **Lease per model call, not per turn.** Acquire the slot before each streamed request and
     release it when the request completes; tools, `await_tasks`, `ask_user`, and pauses run
     without a slot. A loaded model stays loaded while any session uses it or within
     `idle_unload_s`.
  2. **Slot affinity.** Remember each agent's last slot and request it with llama.cpp's
     `id_slot` when it is free, so a waking root usually finds its KV cache intact; otherwise
     take any free slot (the semaphore queues waiters first-come, first-served).
  3. **KV continuity when the slot changed.** Send `cache_prompt: true` and start
     `llama-server` with a host-memory prompt cache (`--cache-ram`, sized by the memory planner
     from free RAM), so a root's prefix is restored from RAM rather than re-computed. For long
     waits, evaluate saving a slot to disk (`--slot-save-path`, `POST /slots/{id}?action=save`
     and `restore`). Verify each flag against the pinned runtime pack before relying on it.
  4. **Visibility.** `models.local` and the desktop Models panel show, per loaded model, slots in
     use, the queue length, and which sessions are using it; the router treats a long queue
     like a busy model and sends subagents elsewhere (the root keeps its model).
  5. **Slots × tier.** The memory planner sizes `--parallel` and the [context
     tier](#context-tiers) together (`--ctx-size` = tier × slots); for Emperor the default is
     the largest tier that still allows 2 slots, with a setting to prefer more slots or a
     bigger window.
- Depends on: none (independent of stages 1 and 2a; may land first)
- Owned paths: `crates/imperium-core/src/agent.rs`, `crates/imperium-local`
  (`supervisor.rs`, `backend.rs`, `planner.rs`), `crates/imperiumd/src/local.rs`,
  `crates/imperium-protocol` (status fields), `docs/04-models-and-routing.md` (runtimes section)
- Acceptance checks: G13; `cargo test -p imperium-core shared_slot` (fake model server, one
  slot, three sessions whose roots await subagents, all finish); `cargo test -p imperium-local`
  covers affinity and queue order; `bash scripts/check-rust.sh` passes; doc 04 no longer says a
  slot is held for the whole turn.
- Status: pending
- Outcome:
- Handoff notes: per-call leasing adds a queue wait before each call, so emit the wait in
  telemetry. With the host prompt cache, context switching costs RAM, not re-computation: at
  q8_0 a 64K root's cache is ~5 GB, so the planner caps `--cache-ram` and evicts the oldest
  idle sessions first. Verified 2026-11-08 against the pinned runtime pack (`LLAMA_CPP_REF=b12180`,
  `tools/server/README.md`): `--cache-ram` exists and defaults to 8192 MiB, `--slot-save-path` with
  `POST /slots/{id}?action=save|restore`, `id_slot` (-1 = any idle slot), and `cache_prompt`
  (default true) are all available.

## Stage 2d: Subagent-first model swapping (Imperium)

On a machine that cannot hold the orchestrator and a local subagent model at once, the work
should still run locally: the subagent has priority while it works, and the orchestrator,
which is asleep in `await_tasks` anyway, steps aside.

**Today (checked 2026-11-08).** The supervisor already unloads *idle* models to make room and
never evicts one in use (`crates/imperium-local/src/supervisor.rs`), and the router skips a
local row that cannot fit. Two things block swapping: an awaiting orchestrator still holds its
slot (fixed by stage 2c), and unloading throws its KV cache away, so reloading it would re-read
its whole context.

- Objective:
  1. **Priority.** A model with a slot in use (a running subagent) is never evicted. A loaded
     model with no slot in use and an agent waiting on it in `await_tasks` (the sleeping
     orchestrator) is evictable when a subagent's model needs the room; the router counts that
     memory as available for local subagent rows.
  2. **Swap out with state.** Before unloading, save each occupied slot's KV with `POST
     /slots/{id}?action=save` (`--slot-save-path` under the daemon's data folder) and record
     which agent owned it.
  3. **Swap back.** When the orchestrator's `await_tasks` resolves, the subagent's model, now
     idle, is unloaded (or kept if both fit), the orchestrator is reloaded, and its slot is
     restored with `action=restore` into the affinity slot before its next call. Model weights
     are memory-mapped, so a recent model reloads from the OS page cache, not from disk.
  4. **Cache, don't discard.** Saved slot files and recently used subagent models are kept in an
     LRU cache bounded by disk and RAM settings (`runtime.llamacpp.swap_cache_mb`), so a second
     subagent task of the same kind reloads warm.
  5. **Cost awareness.** The router compares the swap cost (measured reload seconds, kept per
     model) with the hosted alternative's price for that task's class (stage 2e); menial tasks
     prefer swapping, and a task whose deadline cannot absorb the swap goes to the cloud.
  6. **Visibility.** Swap events (`model.swapped_out`, `model.swapped_in`, with seconds and
     bytes) reach telemetry and the Models panel.
- Depends on: stage 2c (slots must be free while waiting)
- Owned paths: `crates/imperium-local` (`supervisor.rs`, `backend.rs`, `planner.rs`),
  `crates/imperium-core` (resume hook after `await_tasks`), `crates/imperium-models` (router
  cost), `crates/imperium-protocol` (events), `docs/04-models-and-routing.md`
- Acceptance checks: G14; `cargo test -p imperium-local swap` with a fake launcher and a budget
  that fits one model at a time; `bash scripts/check-rust.sh` passes.
- Status: pending
- Outcome:
- Handoff notes: concurrent sessions complicate "the orchestrator is asleep": evict only when
  every agent using that model is waiting. A slot file can be stale if the model or its
  settings changed; key files by model id, file hash, and context size.

## Stage 2e: Task-class routing and single-call cloud tasks (Imperium)

Emperor should send menial work to small local models and keep cloud frontier models for the
hard parts, briefed so one call settles them.

- Objective:
  1. **Default rules.** Ship default `[[routing.rules]]` for the Emperor preset: tags
     `refactor`, `tests`, `ui`, `format`, `docs` → `location = "local"`, tier `basic` or
     `standard`; tags `architecture`, `math`, `proof`, `algorithm`, `design` →
     `location = "cloud"`, tier `frontier`. Users can override them.
  2. **Single-call tasks.** Add an optional `single_call: true` to `spawn_agent` (in the stage-1
     tool contract): the subagent gets the brief and no tools, answers in one model call, and
     the result returns as the report. Emperor sets it for frontier tasks (its root prompt
     already says to brief them for one call).
  3. **Training and evaluation.** Stage 4's synthetic routing cases label task class; the eval
     scores G15 per class.
- Depends on: stage 1 (tool contract); independent of 2c and 2d
- Owned paths: `crates/imperium-protocol` (the `spawn_agent` definition and exported schema),
  `crates/imperium-core` (the single-call path), `crates/imperium-config` (default rules),
  `docs/04-models-and-routing.md`, `docs/03-agent-engine.md`
- Acceptance checks: G15 (after training); now: `cargo test -p imperium-core single_call` shows
  one model call and no tool offers; the default rules route a `tests` task local and a `math`
  task to a cloud frontier row in `cargo test -p imperium-models`; the exported `spawn_agent`
  schema includes `single_call`; `bash scripts/check-rust.sh` passes.
- Status: pending
- Outcome:
- Handoff notes: the emperor-root prompt's "Choosing where work runs" section was added during
  stage 1.

## Stage 2b: Eval harness (emperor repository)

- Objective: replay held-out tasks through `imperiumd` with recorded subagent replies
  (deterministic and free) and, for `--suite e2e`, real hosted subagents; score G1–G6, G10, and G11 into a
  JSON report.
- Depends on: stages 0, 1 (2a for the long-input cases)
- Owned paths: `emperor/eval/`
- Acceptance checks: `uv run eval --model <any> --dry-run` produces a report with every metric.
- Status: pending
- Outcome:
- Handoff notes: only `e2e` spends money; cap it with the API ceiling.

## Stage 3: Baselines and base-model choice

- Objective: run the harness on Qwen3-4B-Instruct-2507, Qwen3-8B, and up to three newer
  Apache-2.0/MIT candidates, zero-shot with the stage-1 prompt, and on a frontier root for the
  G3 reference; pick the base; set the final numeric targets for G1–G4, G10, and G11.
- Depends on: stage 2b
- Owned paths: `emperor/eval/results/`, this plan (goal revision)
- Acceptance checks: a results table of every candidate × metric is committed; the choice and
  any goal revision are recorded here with reasons.
- Status: pending
- Outcome:
- Handoff notes: a frontier root may be *evaluated* through its API (that is not training on
  its outputs), but its traces must never enter the dataset.

## Stage 4: Dataset

- Objective: 20,000–50,000 training trajectories, disjoint from the held-out set by task family.
  Sources: (1) teacher traces, where an [allowed teacher](#teacher-licences) plays root in real
  Imperium sessions on generated tasks (code, research, data, docs, ops), with journals exported
  and kept only when the task succeeded under budget; (2) synthetic routing cases mapping a task
  to the reference decision from the router rules and policy, including "answer directly" and
  "never send `exec` off-machine"; (3) long-input cases built on `delegate_chunks`; (4) negative
  examples (leaked secrets, over-delegation, briefs missing context, needless questions, plans
  missing goals or dependencies) rewritten into correct turns; (5) vague-prompt planning: terse
  requests derived from real tasks (strip a detailed spec down to one line), paired with the
  teacher's full plan and its execution, kept when the plan led to success; (6)
  clarify-or-research cases: prompts with gaps labelled "user's decision", "researchable", or
  "safe default", where the reference turn asks about the first kind (batched, with
  recommendations) and resolves the others with search or defaults recorded as assumptions.
  Every trajectory carries a context budget drawn from the [tiers](#context-tiers), and
  trajectories that would overflow a small budget are re-run (or rewritten) with earlier
  delegation and chunking, so each behaviour is seen at every tier.
- Depends on: stages 1, 3
- Owned paths: `emperor/data/`; the dataset itself goes to `Imperium.AI/emperor-traces` (private)
- Acceptance checks: `uv run data validate` reports 100 % of samples parsing against the stage-1
  schemas, 0 secret-pattern hits, and passing dedup and held-out overlap checks; the datasheet
  lists every source with its licence and provider terms.
- Status: pending
- Outcome:
- Handoff notes: record the teacher and host for every sample, so a licence problem can be
  removed later.

## Stage 5: Supervised fine-tuning (local)

- Objective: QLoRA SFT of the chosen base with Unsloth on the 16 GB GPU, with loss on assistant
  turns only, gradient checkpointing, and a checkpoint sweep on the dev split. Measure the
  longest sequence that fits first and bucket the data to it.
- Depends on: stage 4
- Owned paths: `emperor/train/`
- Acceptance checks: the best checkpoint meets G1 and G2 on the held-out set; no regression over
  2 points on a small general instruction-following set; G9 holds.
- Status: pending
- Outcome:
- Handoff notes: log runs with Trackio; record exact library versions in the recipe.

## Stage 6: Preference tuning, packaging, and measurement (local)

- Objective: DPO with pairs ranked by verifiable signals (schema validity, routing match, brief
  judge, plan cost from the router's prices, leakage); GRPO only if DPO falls short and a local
  run fits. Merge the adapter, export GGUF (Q4_K_M, Q5_K_M, Q8_0, IQ4_XS, Q3_K_M) and AWQ, and
  measure memory and long-context quality at every [context tier](#context-tiers) and KV type.
  Smaller cards are emulated on the 16 GB GPU: the peak `nvidia-smi` usage is the measurement,
  whatever the card's size.
- Depends on: stage 5
- Owned paths: `emperor/train/`, `emperor/export/`
- Acceptance checks: G1, G2, G4, G5, G6, G10, G11, and G12 met; G3 measured on the `e2e` suite;
  the measured tier × quant table replaces the estimates in this plan.
- Status: pending
- Outcome:
- Handoff notes: skip the preference step if stage 5 already meets G1–G5. If the memory planner
  still assumes an f16 KV cache, teach it q8_0/q4_0 (a `crates/imperium-local` change).

## Stage 7: Imperium integration

- Objective: a bundled model-table seed entry and an "Emperor root" preset (session model =
  Emperor, `routing.prefer = "cheapest"`, cloud rows for `strong` and `frontier`);
  `imperium models pull Imperium.AI/emperor-GGUF` works; the desktop Models panel shows it.
  The memory planner learns context tiers: it picks the largest tier and KV type that fit (see
  [Context tiers](#context-tiers)), a config setting pins one, and the chosen tier reaches
  Emperor's system prompt as its context budget.
- Depends on: stage 6
- Owned paths: `crates/imperium-config`, `crates/imperium-models`, `crates/imperium-local`,
  `apps/desktop` (preset UI)
- Acceptance checks: G8: with a fresh `IMPERIUM_HOME` and one config line, `imperium chat`
  completes a delegation task; with `[hardware] vram_mb` set to 8192, 12288, and 16384 the
  planner picks the 64K, 128K, and 256K tiers respectively (`cargo test -p imperium-local
  context_tier`); `bash scripts/check-rust.sh` and `scripts/check-web.sh` pass.
- Status: pending
- Outcome:
- Handoff notes:

## Stage 8: Release and documentation

- Objective: publish the weights, quants, model card (intended use, limits, eval table, VRAM
  table, data sources and licences), and recipe; fold this plan into docs 03, 04, 12 (roadmap
  row), and 13 (decision record, including the NCM decision); delete this plan.
- Depends on: stage 7
- Owned paths: `docs/`, the Hugging Face repositories
- Acceptance checks: G7 and G8; `markdownlint-cli2` is clean.
- Status: pending
- Outcome:
- Handoff notes: publishing needs the user's go-ahead.

## Stage R: Next-Concept Modeling watch (research, not blocking)

- Objective: each quarter, check for an open-weight NCM or diffusion base with tool calling,
  ≥ 32K context, and a runnable inference stack. When one appears, run stage 3's harness on it
  zero-shot, then SFT it on the stage-4 data if it is competitive.
- Depends on: stages 2b and 4
- Owned paths: `emperor/eval/results/`
- Acceptance checks: a dated note per check in the `emperor` repository; a candidate that beats
  Emperor on G1–G4, G10, and G11 at equal or lower VRAM triggers a plan revision.
- Status: pending
- Outcome:
- Handoff notes: serving it in Imperium needs a new `RuntimeBackend`.

## Open questions

| Question | Owner | Needed by |
| -------- | ----- | --------- |
| Which hosted providers serve the teachers, and do their terms allow training on outputs? | Maintainers | Stage 4 |
| API spend ceiling for teacher traces and the e2e suite (default $50) | User | Stage 2b |
| Final Hugging Face organisation name (placeholder `Imperium.AI`) | User | Stage 8 |
