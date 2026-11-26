# Plan: Princeps, the local model, and the Emperor and Consul harness profiles

Revision: 2026-11-26, revision 1; written against `experimental` at `243602ab` (last updated).
Status: planned, no stage started. Roadmap [item 67](../12-roadmap.md#next-in-order).

**User decision (2026-11-26).** Emperor and Consul stop being two models. There is one local
model, **Princeps**, and Emperor and Consul become two **harness profiles** over it: the Emperor
profile is the root agent (plans, delegates, checks) and the Consul profile is the critic (of
plans, diffs and agent reports). Princeps has two planned builds, `princeps-ncm-250m` (a Next
Concept Model, a non-transformer model trained from scratch) and `princeps-tok-250m` (a token
baseline of the same size), both served by `ncm-server`. Ownership:

| Who | Owns |
| --- | --- |
| Imperium Implementation (this plan) | Local model naming, obtaining and launching `ncm-server`, the runtime backend, the catalogue, harness profiles as a mechanism, the escalation hook point, eval wiring |
| NCM session (Imperium-Models `models/ncm/`, Imperium-Research R4) | `ncm-server` itself, its release builds, the weights and their publication |
| Imperium Harnesses session | The content of the Emperor and Consul harnesses (prompts, workflows, critic rules), later written in [Ratio](https://github.com/Densetsu152637/Ratio) |

## Goals

Each goal is boolean, with the check that decides it. "CI" is this repository's CI on the pull
request; "ncm-live" is the opt-in workflow [stage 2](#stage-2-obtaining-and-verifying-ncm-server)
adds.

1. A model file `princeps-ncm-250m.safetensors` in a model folder is listed as
   `ncm:princeps-ncm-250m` and loads through the supervisor like a llama.cpp model. Check: CI
   tests over a fake `ncm-server`; ncm-live loads the real release build with `--random-weights`.
2. `ncm-server` is obtained only from a pinned Imperium-Models release, every asset is checked
   against a SHA-256 pinned in this repository, unpacked under the llama.cpp extraction rules,
   and installed only on an explicit user action. Check: CI tests for refusal of a wrong hash, a
   missing hash, a hostile archive and an install without consent.
3. One loaded Princeps serves the Emperor and the Consul profile in the same session from one
   process. Check: a CI test with a fake server sees one launch and two profiles' calls on it.
4. A harness profile (model, system prompt, tool set, workflow) can be selected for the root, for
   a subagent and for a headless run. Check: CI tests; `imperium run --profile consul
   --print-prompt` prints the Consul prompt and tools.
5. The rejection count and escalation hook of item 64 exist with no workflow bound. Check: CI
   tests that two rejections of one subagent task fire the hook once, and that unbound it does
   nothing.
6. `imperium run --headless --json --profile <p> --model ncm:<id>` records the profile and the
   server's timings. Check: ncm-live runs one headless task on random weights and asserts the
   JSON fields.

## Decisions

### D1. Model ids, spec and format

- **Spec prefix `ncm:`**, beside `llamacpp:` and `vllm:`. The id is the weight file's stem:
  `ncm:princeps-ncm-250m`, `ncm:princeps-tok-250m`. One runtime serves both builds; the
  architecture is read from the file, never from the id.
- **A new `Format::NcmSafetensors`.** The scanner recognises a `.safetensors` file whose header
  `__metadata__` has `ncm_format = "ncm-safetensors-v1"` (the marker `ncm_model::weights` writes).
  It reads only the 8-byte header length and the JSON header, capped at 1 MiB, so an unrelated or
  hostile file costs nothing. Hugging Face folders (`config.json` plus safetensors) stay vLLM's;
  a lone safetensors file without the marker stays unlisted, as today. A file with the marker but
  no `tokenizer.json` beside it is listed with a `problem`.
- **Catalogue rows.** When an `ncm:` model is found, the daemon offers it as a local row with
  id `princeps-ncm-250m` (the stem), tags `princeps`, `no-vision`, `reasoning`, tier `basic`
  until the stage 6 evals say otherwise, and `context_window` from the header's config. Users
  override it in `[[models.catalog]]` as for any row.
- **`princeps-tok-250m` is an eval build.** It is listed when present but the first-run card
  offers only the shipping build (expected `princeps-ncm-250m`; the research plan scales only the
  winning architecture).
- **The `emperor` tag migrates.** `is_emperor` (knowledge agents prefer that row,
  [21](../21-knowledge-agents.md)) matches the `princeps` tag first; `emperor` stays accepted as
  a deprecated alias for one minor version. `routing.knowledge_model` is unchanged.

Rejected: reusing `llamacpp:` with a flag (the spec would lie about the runtime and the llama.cpp
install hooks would fire); detecting by file name (a renamed file would load into the wrong
runtime).

### D2. Runtime backend

`ncm-server` already takes llama-server's launch line and endpoints (Imperium-Models
[`models/ncm/runtime/README.md`](https://github.com/Densetsu152637/Imperium-Models/blob/main/models/ncm/runtime/README.md)):
`/health` answers 503 while loading and 200 when ready, the process exits non-zero on a failed
load, and `/v1/chat/completions` streams content, tool calls and usage. So the integration is a
third `RuntimeBackend`, `Ncm` (id `ncm`), after llama.cpp and vLLM in `backend::builtin`:

- `accepts(Format::NcmSafetensors)`; `available()` when an `ncm-server` path is resolved.
- `launch_args`: `--model <path> --tokenizer <dir>/tokenizer.json --host 127.0.0.1 --ctx-size
  <pool> --parallel <slots> --n-gpu-layers <0 or 999> --alias <id>` plus
  `runtime.ncm.extra_args`; the port is appended by the supervisor as for llama.cpp. No cache-type,
  flash-attention, draft, `--cache-ram` or slot-save flags: they are llama.cpp's.
- `place`: wholly on the GPU or wholly on the CPU (no layer split). `place_on_cpu` is supported,
  so the CPU lane works. Footprint: weights from the file size, cache from the header's config by
  the formula the NCM session confirms ([Q3](#open-questions-for-the-ncm-session)), plus the
  usual overhead and calibration.
- `slot_aware() = false`: `ncm-server` resets its caches on every request, so slot pinning and
  `--cache-ram` would buy nothing.
- `check_program`: runs `ncm-server --version` (and `--list-devices` once it exists, Q3) with
  the same timeout as `runtime_check`, so a CPU-only build on a GPU machine is reported the way
  llama.cpp's is.
- **Environment.** The server starts with the daemon's environment minus variables the NCM session
  lists as changing kernels or devices (Q12), the same treatment as llama.cpp's `DRAFT_ENV`.
- **Supervision.** Unchanged: free loopback port, wait for `/health`, log to
  `<data>/logs/ncm-<id>.log`, idle unload, stop on daemon exit. The context pool grows by restart
  between calls as llama.cpp's does; `ncm-server` allocates its whole cache at launch, which is
  exactly that model.

Rejected: linking `ncm-model` into `imperium-local` in-process. The research decision
(Imperium-Research `docs/next-concept-modelling/runtime.md`) keeps the runtime beside the training
code while the architecture changes, it would pull Burn and CUDA into Imperium's build, and a
crash would take the daemon down. Revisit when the architecture is frozen after R4 stage 6.

### D3. Obtaining, verifying and launching `ncm-server`

The security bar is the llama.cpp runtime's ([04](../04-models-and-routing.md#runtimes)), with
two changes because Imperium-Models is our own repository: hashes are pinned here rather than
trusted from the release, and nothing installs without the user.

- **Source.** GitHub releases of `Densetsu152637/Imperium-Models`, tag `ncm-server-v<version>`,
  one archive per build: `ncm-server-<version>-<os>-<arch>-<backend>.{zip,tar.gz}`
  (`windows-x64-cuda`, `windows-x64-cpu`, `linux-x64-cuda`, `linux-x64-cpu`,
  `macos-arm64-cpu`; Q1 and Q11 settle the list). The NCM session builds them in Imperium-Models CI
  (Q1). There is no other source: no PATH search, no package manager, no Downloads-folder
  candidate run by itself.
- **Pin.** `runtimes/ncm-server.env` holds `NCM_SERVER_REF` and one
  `NCM_SERVER_SHA256_<build>` per asset, raised by pull request when a release is adopted. The
  installer reads the release through the GitHub API, downloads each asset through its API URL
  (as for llama.cpp, avoiding the flaky `releases/download` links), and refuses an asset whose
  SHA-256 differs from the pinned one or from GitHub's published `digest`. An asset missing from
  the pin is refused. A compromised release can therefore not change what installs.
- **Provenance.** If the release workflow publishes GitHub artifact attestations (Q13), the
  pinning pull request's CI checks them with `gh attestation verify`; the client relies on the
  pinned hash. Code signing (Authenticode) follows [item 59](../12-roadmap.md#next-in-order);
  until then the Windows binary is unsigned, which the pinned hash covers.
- **Unpacking.** Reuse the llama.cpp extractor, moved out of `runtime.rs` into a shared
  `imperium-local` module with no behaviour change: staging folder, regular files and folders
  only, no absolute paths, `..`, drive prefixes, NUL, devices, FIFOs or zip symlinks, the 8 GiB,
  20 000 entry and 200:1 ratio caps, and nothing replaced on failure (the rules of #313 and #316).
- **Consent.** No `auto_install`. `ncm-server` installs only when the user presses *Install*
  (the Princeps card or the runtime card) or runs `imperium runtime ncm install`; the card states
  the source, version, size and that it is Imperium's own build. An agent's shell command
  `imperium runtime ncm install|remove` needs a person's approval, like `runtime vllm setup`. A
  missing runtime fails a load with *No ncm-server yet: open Local models → Runtime to install
  it*.
- **Where and which.** `<data>/runtimes/ncm-server/<ref>/<build>/ncm-server[.exe]` with an
  `imperium-runtime.json` record. Order: `runtime.ncm.custom_path` (a developer build; only a
  path the user sets), then the installed build of the pinned ref, then an earlier installed ref
  until the pinned one is installed. Update and rollback follow
  [llama.cpp updates](../04-models-and-routing.md#llamacpp-updates): a new pin installs on the
  next user-approved install or the next load after the user has once approved installs for this
  runtime (`runtime.ncm.update = "ask"` by default, `auto` only if the user sets it), and one
  earlier build is kept until the new one has loaded a model.
- **Build choice.** CUDA on an NVIDIA GPU whose driver meets the build's minimum (Q2), otherwise
  CPU; the readiness check decides as for llama.cpp. GPU builds that need NVRTC or other CUDA
  libraries ship them in the archive or a pinned companion asset (Q2), as llama.cpp's `cudart`
  archive does.

### D4. One loaded model, two profiles

- **One process per Princeps build.** Profiles name a model; the supervisor keys servers by model,
  so the Emperor and Consul profiles on `ncm:princeps-ncm-250m` share one `ncm-server`. Nothing new
  is needed for the sharing itself.
- **One slot, queued in Imperium.** `ncm-server` runs requests one at a time and `--parallel N`
  only divides the context. So Imperium launches it with `--parallel 1` and the full context, and
  concurrency is the residency controller's slot queue: a Consul call waits while an Emperor call
  runs, with the usual wait reason and `max_wait_s`. When `ncm-server` gains real batching (Q4),
  slots rise to the batch size.
- **Memory.** Weights are about 470 MiB in bf16 (150 to 310 MiB quantised) and the NCM cache about
  1 GiB at 1M tokens (R4 measurements, random weights). Default context: `runtime.ncm.context =
  32768` per call, because prefill, not memory, is the cost (128k prefill took 23 to 28 s); `auto`
  and up to the model's 1M ceiling are settings.
- **No cross-profile cache thrash.** The server keeps no prompt cache between requests, so
  alternating profiles costs nothing extra today. If prompt-cache reuse lands (Q4), each profile
  gets its own slot so their prefixes do not evict each other.

Rejected: two processes, one per profile. It doubles weights and cache for no throughput, since
batch-1 decode on one GPU is the bottleneck and escalation is sequential anyway. Revisit if the
stage 6 evals show the Consul queue wait above 20 % of turn wall time and batching is not
available.

### D5. Harness profiles before Ratio

A **harness profile** is a named bundle: `model` (a spec, a catalogue id, or the alias
`princeps`, the best installed Princeps build), `system_prompt` (a file), `tools` (an allowlist,
narrowing only), `workflow` (workflow-contract rule files, [18](../18-workflow-contract.md)) and
`harness` (the agent-loop engine: `default` today). It is item 58's per-subagent harness, prompt
and workflow, packaged under one name, so this stage is item 58's first slice and item 58 builds on
it rather than beside it.

- **Format.** A named-agent file ([06](../06-extensibility.md#named-agents-implemented)) with two
  new front-matter fields, `workflow` and `harness`, and `role: root | subagent | both`. Profiles
  and named agents share one namespace, precedence and trust rules; a profile is a named agent
  that may also run as the root.
- **Built-in profiles.** `emperor` and `consul` ship in a new lowest-precedence source
  (`crates/imperium-core/profiles/`), so a user or project file of the same name replaces them.
  Emperor uses the vendored `emperor-root.md`; Consul ships a minimal critic prompt marked as a
  placeholder. Both bodies belong to the Harnesses session, which replaces them by pull request;
  the mechanism, tests and names are this plan's.
- **Selection.** Root: `session.create` and the composer's model menu take `profile`, applied at
  a turn boundary. Subagents: `spawn_agent` takes `agent` as today (a profile is an agent). Headless:
  `imperium run --profile <name>`. A profile never widens permissions, budgets or the permission
  mode, exactly as named agents.
- **Mapping to Ratio.** When Ratio lands, a profile sets `harness = "ratio:<package>@<version>"`
  and its prompt and workflow come from the package; the profile's name stays, so configs, evals
  and the escalation binding keep working. Ratio's host side stays item 58's.

Rejected: a new profile file type separate from named agents (two near-identical systems), and
making profiles session-level config only (subagents and evals need them too).

### D6. Consul escalation hook point (item 64)

Mechanism only; the workflow (when Consul takes over, what it says, how it hands back) is the
Harnesses session's.

- **Rejection ledger.** Per subagent task, within one turn: the user declined an approval; a
  tool or command failed; a critic returned `verdict: reject` in its structured result. Each is an
  `agent_rejected` event with the task id and kind.
- **Hook.** At a configurable count per task (the workflow's, not a user setting; item 64 says 2)
  the engine calls an `EscalationHook` with the task's state; it returns `None` (continue) or
  `Takeover { profile, brief }`, after which the task resumes under that profile from its current
  state. Hand-back and questions use the existing `resume_agent` and question paths.
- **Unbound by default.** No built-in workflow binds the hook; a workflow file binds it to a
  profile (`consul`). Tests use a fake binding.

### D7. Eval wiring

- `imperium run --headless --json` gains `--profile <name>`; `--model` still overrides the
  profile's model, so the four arms (two builds times two profiles) are flag changes. Through the
  daemon (`session.eval_create`) local `ncm:` models load through the supervisor like any local
  model.
- The JSON result adds `profile` (name, source and a hash of the resolved profile) and, per model
  call, the server's `timings` (`prompt_per_second`, `predicted_per_second`, `cache_bytes`) when the
  server sends them, so R4 stage 9 measures tokens per second inside the agent loop.
- The scored evals themselves are the research sessions' work, run through this harness.

## Current state (found on `experimental` at `243602ab`)

- `imperium-local`: `RuntimeBackend` with `LlamaCpp` and `Vllm` (`backend.rs`); `Format` has
  `Gguf` and `Safetensors` (`scan.rs`); the hardened installer and extractor live in `runtime.rs`;
  vLLM in WSL2 in `wsl.rs` and `vllm_env.rs`. Spec prefixes are matched in
  `imperium-core/src/{model_config.rs,bench.rs,suggestions.rs}`.
- House models: `apps/desktop/src/models/houseModels.ts` lists Emperor and Consul with no source;
  `HouseModelCard.tsx` shows them on first run. `ModelSuggestionCard.tsx` and
  `[[models.suggestions]]` offer Hugging Face GGUF repositories only.
- Emperor in the engine: `is_emperor` in `imperium-engine/src/model.rs`; `emperor-root.md` in
  `imperium-protocol/schemas/prompts/`.
- Named agents (item 48) and headless evals with `--agent` (item 53) are implemented; item 58
  (harness switch) and item 64 (escalation) are planned with no code.
- `ncm-server` (Imperium-Models `models/ncm/runtime/`, merged in #40): llama-server's flags and
  endpoints, random weights for tests, one weight file with an `ncm_format` header, `tokenizer.json`
  beside it. No release builds, no published weights; CI builds CPU only. Further training is on
  hold until the Harnesses session has built the harnesses.

## Scope and non-goals

In scope: D1 to D7. Not in scope: training, weights and their quality; the harness prompts and
workflow content; Ratio and its host bindings; vision; quantised-weight selection beyond passing
the file the user installed; offering Princeps through the generic download suggestions.

## Assumptions

- `ncm-server` keeps its llama-server-compatible launch line and endpoints; changes are announced
  by the NCM session and adopted by pinning a new ref.
- Trained weights will be published at a pinned Hugging Face revision with per-file SHA-256 (Q5).
- The research harness suites already run through `imperium run --headless --json`.

## Relevant paths

`crates/imperium-local/src/{backend.rs,scan.rs,runtime.rs,runtime_check.rs,supervisor.rs,planner.rs}`,
a new `crates/imperium-local/src/{ncm.rs,extract.rs}`, `runtimes/ncm-server.env`,
`crates/imperium-config/src/{settings.rs,registry.rs}`, `crates/imperium-core/src/{local.rs,model_config.rs,bench.rs,suggestions.rs}`,
`crates/imperium-core/profiles/`, `crates/imperium-engine/src/{model.rs,agent/delegation.rs}`,
`crates/imperium-cli/src/` (`runtime ncm`, `run --profile`), `apps/desktop/src/models/houseModels.ts`,
`apps/desktop/src/panels/{HouseModelCard.tsx,LocalModelsPanel.tsx}`, `.github/workflows/ncm-live.yml`.

## Related docs

[04](../04-models-and-routing.md) (runtimes; wins on current behaviour),
[06](../06-extensibility.md#named-agents-implemented), [12](../12-roadmap.md) items 44, 53, 58, 59
and 64 (win on scope), Imperium-Models `models/ncm/runtime/README.md` (wins on the server's
contract), Imperium-Research `docs/next-concept-modelling/` (wins on the models).

## Stages

Each stage is one pull request against `experimental` with its docs, CI green before the next.
Stages 1, 4 and 5 need nothing from the NCM session; stage 2 needs a release (Q1, Q2); stage 3
needs published weights (Q5) only for its last step.

### Stage 1: `ncm` backend and format

- Objective: D1 and D2 with `runtime.ncm.custom_path` as the only way to name a server.
- Depends on: none.
- Owned paths: `imperium-local` `backend.rs`, `scan.rs`, new `ncm.rs`; spec prefixes in
  `imperium-core`; `runtime.ncm.*` settings and registry; docs 04 and 10.
- Acceptance checks (CI): the scanner lists a marked file as `ncm:<stem>` and ignores an unmarked
  or oversized header; launch arguments match a golden list; a fake `ncm-server` script answering
  503 then 200 on `/health` is started once and served; a fake that exits non-zero fails the load
  with its log tail; `place` never splits; the CPU lane accepts an `ncm:` row.
- Status: pending.

### Stage 2: obtaining and verifying `ncm-server`

- Objective: D3: `imperium runtime ncm install|status|remove`, `runtime.status` reporting the
  `ncm` runtime, the shared extractor, `runtimes/ncm-server.env`, and the opt-in `ncm-live`
  workflow (manual and nightly) that installs the pinned release and loads it with
  `--random-weights`.
- Depends on: stage 1; an Imperium-Models release (Q1, Q2).
- Owned paths: `imperium-local` `runtime.rs` (extractor moved out), new `extract.rs`,
  `runtime_check.rs`; `imperium-cli` runtime commands; `.github/workflows/ncm-live.yml`; docs 04
  and 11.
- Acceptance checks (CI): the llama.cpp extractor tests pass unchanged against the moved module;
  an asset whose hash differs from the pin, or from GitHub's digest, or with no pin, is refused and
  nothing is replaced; hostile archives are refused; no install runs without the explicit call;
  an agent's `imperium runtime ncm install` is classed as needing approval. ncm-live: install,
  `/health` 200, one chat completion.
- Status: pending.

### Stage 3: Princeps in the catalogue and the app

- Objective: one Princeps entry replaces Emperor and Consul in `houseModels.ts`, with its pinned
  file set (weights, `tokenizer.json`) and SHA-256s; a daemon method `models.install_house`
  downloads the set, verifies every file and offers to install `ncm-server` in the same consent
  (protocol 0.63); the Local models panel shows `ncm:` models and an `ncm-server` runtime card;
  the `princeps` tag and the `emperor` alias (D1).
- Depends on: stages 1 and 2; Q5 for the real source (the card keeps *not yet available* until
  then).
- Owned paths: `houseModels.ts`, `HouseModelCard.tsx`, `LocalModelsPanel.tsx`, the protocol method
  and its TS types, `imperium-engine` `model.rs`; docs 04, 07 and 12 item 44.
- Acceptance checks (CI): catalogue parsing tests; a download whose file hash differs is deleted
  and reported; knowledge routing prefers a `princeps` row and still honours `emperor`; the card
  never shows a download without a source.
- Status: pending.

### Stage 4: harness profiles

- Objective: D5: the profile fields on agent files, the built-in source with `emperor` and
  `consul`, root selection on sessions, `--profile` on `imperium run`.
- Depends on: none (stage 1 for the `princeps` alias to resolve to a real build).
- Owned paths: agent-file parsing and the built-in source in `imperium-core`, `session.create`,
  `imperium-cli` run; docs 03, 06 and 12 item 58.
- Acceptance checks (CI): a user file named `consul` replaces the built-in; a profile's tools
  narrow and never widen; `--print-prompt --profile consul` prints the Consul body and tools; two
  profiles on one `ncm:` model in one session cause one launch (fake server).
- Status: pending.

### Stage 5: escalation hook point

- Objective: D6: the rejection ledger, `agent_rejected` events, the `EscalationHook` and a binding
  from workflow files; nothing bound by default.
- Depends on: stage 4.
- Owned paths: `imperium-engine` delegation and events, the workflow binding in `imperium-core`;
  docs 03 and 12 item 64.
- Acceptance checks (CI): two rejections of one task fire the hook once and one rejection each of
  two tasks fires nothing; a `Takeover` resumes the task under the named profile with its state;
  unbound, behaviour matches today's.
- Status: pending.

### Stage 6: eval wiring and close-out

- Objective: D7: `profile` and `timings` in the headless JSON; ncm-live runs one headless task per
  profile on random weights; docs updated and this plan marked done.
- Depends on: stages 2 and 4.
- Owned paths: headless result types in `imperium-core` and `imperium-cli`; docs 03 and 12.
- Acceptance checks: CI tests of the JSON shape; ncm-live asserts `profile.name` and
  `predicted_per_second` in the result. Handoff: tell the research sessions the flags are live.
- Status: pending.

## Risks

- **The server contract moves** while the architecture changes. Mitigation: pinned refs, a golden
  launch-argument test, and ncm-live on every pin change.
- **CUDA runtime libraries** on users' machines (NVRTC, driver minimum). Mitigation: Q2, the
  readiness check, and the CPU build as fallback.
- **Queue contention** between profiles on one slot. Mitigation: measured in stage 6; D4's revisit
  trigger.

## Open questions for the NCM session

1. **Release builds.** Will Imperium-Models publish `ncm-server` as GitHub release assets, with
   which tag scheme and archive names, for which targets (windows-x64 and linux-x64, CUDA and CPU;
   macOS arm64 CPU)? CI builds CPU only today: can a release job build the `cuda` feature (a CUDA
   toolkit on the runner, or a self-hosted runner)?
2. **CUDA at run time.** Which libraries does the CUDA build load (driver, NVRTC, cuBLAS or
   others), what is the minimum NVIDIA driver, and can the redistributable ones ship in the
   archive the way llama.cpp ships `cudart`?
3. **Readiness and sizing without loading.** Can `ncm-server` add `--list-devices` (or `--check`)
   reporting whether CUDA works, a stable `--version` (version, commit, features), and either a
   `--print-footprint --ctx-size N` (weights and cache bytes as JSON) or a documented formula from
   the header's `ncm_config`, so Imperium plans memory before launch?
4. **Concurrency and prompt cache.** Is batching two sequences (real `--parallel 2`) or
   prompt-cache reuse across requests planned, and when? D4 depends on it.
5. **Weights publication.** Which Hugging Face repository (or other host), file names for bf16,
   8-bit and 4-bit, licence, pinned revision, and per-file SHA-256? Is `tokenizer.json` always
   beside the weights?
6. **Format and compatibility.** Is `ncm_format = "ncm-safetensors-v1"` the stable detection key?
   Can the header also carry a model id and version (`princeps-ncm-250m`, training revision) for
   display? Which server versions load which weight files, so a weights pin can name its minimum
   server ref?
7. **Quantised weights.** Are 8-bit and 4-bit weights separate files in the same format, and does
   the server pick the kernels from the header or from a flag?
8. **Speculation and quantised cache.** Which flags will enable self-speculation and an 8-bit
   cache, and should Imperium pass them by default?
9. **Context.** What context should Imperium allocate by default for Princeps (D4 proposes 32k),
   and how long does a restart with a larger cache take (weights load plus allocation)?
10. **Tool calls and reasoning.** Timeline for `tool_choice: "required"` or a named tool, schema
    enforcement, and `stop` strings; and should Imperium show `reasoning_content` from Princeps?
11. **Other GPUs.** Are AMD, Intel and Apple GPUs (Burn wgpu) in scope, or CPU-only there?
12. **Network and environment.** Confirm the server makes no network calls and binds only
    `--host`; list environment variables that change its kernels or device (`CUBECL_*` and
    similar) so Imperium can scrub them.
13. **Provenance.** Can the release workflow publish `SHA256SUMS` and GitHub artifact attestations
    for each asset?

## Questions for the Harnesses session (for later relay)

- The critic's structured result: is `verdict: accept | reject` plus a reason enough for D6's
  ledger, or does the Consul harness need more fields?
- Which tool set should each profile start with (D5 ships Emperor with the root's tools and Consul
  read-only)?
