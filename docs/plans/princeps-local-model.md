# Plan: Princeps, the local model, and the Emperor and Consul harness profiles

Revision: 2026-11-26, revision 4 (the NCM session's relayed user decisions: no releases yet, so an
interim user-built path and a deferred stage 2b; weights, licence and GPU scope decided; revision 3
folded in the re-review's R1, trusted-profile and escalation
settings, and R2, every runtime file verified and held at launch; revision 2 folded in the plan
review's B1 to B4, should-fix 1 to 7 and nits, and the NCM session's answers, from its branch
`feat/ncm-speed-2` at `61350bc`); written against `experimental` at `9ce002c5` (last updated).
Revision 1 was `7d4f11c5`, revision 2 `6d1cdf5c`, revision 3 `10b67a66`.
Status: planned; stage 1 in progress in the Imperium Princeps (NCM) session (`feat/ncm-runtime`), no other stage started. Roadmap [item 67](../12-roadmap.md#next-in-order).

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

**Protocol versions.** Stages 3, 4, 5 and 6 change the protocol. Each takes the next protocol
minor free when it merges; none is reserved here (0.63 went to #301, and #304 holds 0.64).

## Goals

Each goal is boolean, with the check that decides it. "CI" is this repository's CI on the pull
request; "ncm-live" is the opt-in workflow [stage 2a](#stage-2a-the-user-built-ncm-server-interim)
adds, which runs a CPU build only (the CUDA path has no CI check; see [risks](#risks)).

1. A model file `princeps-ncm-250m.safetensors` in a model folder is listed as
   `ncm:princeps-ncm-250m` and loads through the supervisor like a llama.cpp model. Check: CI
   tests over a fake `ncm-server`; ncm-live loads a CPU build of `ncm-server` with `--random-weights`.
2. Until releases exist, `ncm-server` runs only from a path the user set and approved, with its
   hash re-checked at every launch (stage 2a). Once releases exist, it is obtained only from a
   pinned Imperium-Models release whose attestation was verified when it was pinned, every asset is
   checked against a SHA-256 pinned in this repository, unpacked under the llama.cpp extraction
   rules, installed only on an explicit user action, and re-checked at every launch (stage 2b).
   Check: CI tests for each refusal.
3. One loaded Princeps serves the Emperor and the Consul profile in the same session from one
   process. Check: a CI test with a fake server sees one launch and two profiles' calls on it.
4. A harness profile can be selected for the root, for a subagent and for a headless run, and no
   profile widens permissions or replaces the base and safety instructions. Check: the stage 4
   tests.
5. The rejection count and escalation hook of item 64 exist, unbound by default, bindable only by
   the user. Check: the stage 5 tests.
6. `imperium run --headless --json --profile <p> --model ncm:<id>` records the profile and the
   server's timings. Check: ncm-live runs one headless task on random weights and asserts the
   JSON fields.

## Decisions

### D1. Model ids, spec and format

- **Spec prefix `ncm:`**, beside `llamacpp:` and `vllm:`. The id is the weight file's stem:
  `ncm:princeps-ncm-250m`, `ncm:princeps-tok-250m`. One runtime serves both builds; the
  architecture is read from the file, never from the id.
- **A new `Format::NcmSafetensors`.** The scanner recognises a `.safetensors` file whose header
  `__metadata__` has `ncm_format = "ncm-safetensors-v1"` (stable, per the NCM session). It reads
  the 8-byte header length and then at most 1 MiB of JSON header. The 1 MiB cap is for scanning
  only: an NCM header holds a few hundred tensor entries, far below the 64 MiB that the
  safetensors format itself allows. A file whose header is larger is listed with a `problem`
  (*header too large to read*), not silently skipped. Hugging Face folders (`config.json` plus
  safetensors) stay vLLM's; a lone safetensors file without the marker stays unlisted, as today. A
  marked file with no `tokenizer.json` beside it is listed with a `problem`, since the server
  needs it.
- **The header is untrusted input.** `ncm_config` (TOML), `ncm_quant` (`q8`, `q4-g64`, with or
  without a quantised head) and, once added, `ncm_model_id` and `ncm_model_version` are parsed
  with size limits per field. Every number taken from them (layers, widths, vocabulary, context)
  is clamped to a sane range and every footprint product uses checked arithmetic; an overflow or
  out-of-range value lists the model with a `problem` rather than producing a footprint. The
  server's own bounds checks are an [open question](#open-questions-for-the-ncm-session) (Q14).
- **Catalogue rows.** When an `ncm:` model is found, the daemon offers it as a local row with id
  the stem, tags `princeps`, `no-vision`, `reasoning`, tier `basic` until the stage 6 evals say
  otherwise, and `context_window` from the clamped config. Users override it in
  `[[models.catalog]]` as for any row.
- **`princeps-tok-250m` is an eval build.** It is listed when present, but the first-run card
  offers only the shipping build (expected `princeps-ncm-250m`; the research plan scales only the
  winning architecture).
- **The `emperor` tag migrates.** `is_emperor` (knowledge agents prefer that row,
  [21](../21-knowledge-agents.md)) matches the `princeps` tag first; `emperor` stays accepted as a
  deprecated alias for one minor version. `routing.knowledge_model` is unchanged.

Rejected: reusing `llamacpp:` with a flag (the spec would lie about the runtime and the llama.cpp
install hooks would fire); detecting by file name (a renamed file would load into the wrong
runtime).

### D2. Runtime backend

`ncm-server` already takes llama-server's launch line and endpoints (Imperium-Models
[`models/ncm/runtime/README.md`](https://github.com/Densetsu152637/Imperium-Models/blob/main/models/ncm/runtime/README.md)):
`/health` answers 503 while loading and 200 when ready, the process exits non-zero on a failed
load, and `/v1/chat/completions` streams content, `reasoning_content`, tool calls and usage. So
the integration is a third `RuntimeBackend`, `Ncm` (id `ncm`), after llama.cpp and vLLM in
`backend::builtin`:

- `accepts(Format::NcmSafetensors)`; `available()` when an `ncm-server` path is resolved.
- `launch_args`: `--model <path> --tokenizer <dir>/tokenizer.json --host 127.0.0.1 --ctx-size
  <pool> --parallel 1 --n-gpu-layers <0 or 999> --device <cpu|cuda> --alias <id>` plus
  `runtime.ncm.extra_args`; the supervisor appends the port. No cache-type, flash-attention,
  draft, `--cache-ram` or slot-save flags (llama.cpp's). `--speculate` stays off (the NCM
  session's advice); a quantised file loads by its own layout, and `--quant` is left to
  `extra_args`.
- **`extra_args` cannot move the server.** `--host`, `--port`, their abbreviations and their
  `=`-forms are refused at config time and dropped at launch (as for vLLM in WSL), and the server
  is always bound to `127.0.0.1`.
- `place`: wholly on the GPU or wholly on the CPU (no layer split). `place_on_cpu` is supported,
  so the CPU lane works. Footprint: weights from the file size, cache from the clamped header
  config by the NCM session's formula (`ncm_model::cache_bytes_with`, mirrored in Imperium and
  tested against its published values: about 1 GiB for the NCM and 5 GiB for the token baseline
  at 1M tokens in bf16), plus the usual overhead and calibration from `/props` `cache_bytes`.
- `slot_aware() = false`, no `--cache-ram`: the server keeps its own prompt-prefix cache (on a
  branch, default on), which needs nothing from Imperium.
- `check_program`: runs `ncm-server --version` with the `runtime_check` timeout. Until the server
  has `--list-devices` (Q3), CUDA use is confirmed from the first `/props` (`ncm.device`).
- **Environment: an allowlist.** The server starts with only `PATH`, `SYSTEMROOT` and `WINDIR`,
  `TEMP` and `TMP`, `HOME` and `USERPROFILE`, `LANG`, `CUDA_PATH` (kept: CubeCL reads NVRTC headers
  from it until they ship in the archive, Q2) and `CUDA_VISIBLE_DEVICES` when the planner sets it.
  Every other variable, `CUBECL_*` and secrets included, is dropped.
- **Requests.** Every call sets `max_tokens` explicitly (the agent's output budget), because the
  server otherwise generates to the end of the context. A cancelled call drops its connection;
  whether that stops generation is Q16, and stage 1 tests that Imperium releases the slot either
  way and that a server which keeps generating is treated as busy until its stream ends.
- **Capabilities Imperium must cope without** (NCM answer 10): `tool_choice` is only `none` or
  `auto`, there is no `response_format` or schema enforcement and no `stop` strings. So `ncm:`
  rows never get a forced tool call (paths that set `tool_choice: required` or a named tool fall
  back to `auto` with an instruction and one retry), structured outputs are parsed leniently and
  re-asked once, and stop strings are not sent. The server stops at its own end markers.
- **Reasoning is shown.** `reasoning_content` is read and shown like other models' reasoning.
- **Supervision.** Unchanged: free loopback port, wait for `/health`, log to
  `<data>/logs/ncm-<id>.log`, idle unload, stop on daemon exit. The context pool grows by restart
  between calls as llama.cpp's does; `ncm-server` allocates its whole cache at launch, which is
  exactly that model. The first request compiles kernels (0.4 to 0.65 s); load time is not yet
  measured.

Rejected: linking `ncm-model` into `imperium-local` in-process. The research decision
(Imperium-Research `docs/next-concept-modelling/runtime.md`) keeps the runtime beside the training
code while the architecture changes, it would pull Burn and CUDA into Imperium's build, and a
crash would take the daemon down. Revisit when the architecture is frozen after R4 stage 6.

### D3. Obtaining, verifying and launching `ncm-server`

The security bar is the llama.cpp runtime's ([04](../04-models-and-routing.md#runtimes)), raised
because Imperium-Models is our own repository: the pin in this repository governs, the release's
provenance is verified when pinning, and nothing installs without the user.

**Interim: no releases yet (user decision 2026-11-26).** Imperium-Models publishes no
`ncm-server` releases for now; the binary is built from source. Until releases exist, the only
way to run Princeps is a **user-built binary at a path the user sets**, and the release download
below is deferred (stage 2b).

- **Decision: a user-set path, not `imperium runtime ncm build`.** Building from source needs a
  Rust toolchain and, for CUDA, a CUDA toolkit; Imperium installs neither, and a build command
  would run a large compile with network access on the user's machine. The user builds
  `ncm-server` themselves (`cargo build --release -p ncm-server [--features cuda]` in
  Imperium-Models at a commit they choose) and points Imperium at it. Revisit a build command
  only if releases stay absent once the weights ship.
- **Set only by the user.** `runtime.ncm.custom_path` (or *Choose ncm-server…* on the runtime
  card, or `imperium runtime ncm use <path>`) is floored as below: agents, mod frames and paired
  phones cannot set it, in any mode. The path must be absolute and name a regular file (no
  symbolic link at the file).
- **Copy on approval.** When the user sets the path, the card shows the file's path, size, SHA-256
  and `--version` output, and the user approves it. Imperium then copies the binary into the
  protected `<data>/runtimes/ncm-server/user/<sha256>/` (hashing the copy, which must match) and
  records the hash, the original path and the approval time in
  `<data>/runtimes/ncm-server/user.json`. Imperium **always launches that copy, never the
  original**, with the child's working folder set to the copy's folder. Windows searches the
  executable's own folder first for DLLs, so a DLL planted beside the original binary (in a
  project or a build folder agents can write) is never loaded; the copy's folder holds only the
  copy. Because the original is never run, it may live anywhere, a project or worktree included.
- **Re-checked at launch.** Every launch re-hashes the copy through a held handle, as in the
  tamper checks below; a mismatch refuses the launch. Rebuilding the original changes nothing until
  the user approves the new file, which makes a new copy (one earlier copy is kept until the new one
  has loaded a model). Libraries the binary loads from elsewhere (NVRTC from the user's CUDA
  toolkit on a CUDA build) are not hashed; this is recorded as a residual.
- **Revocation, pins and attestations** do not apply to a user build; they start with releases.

- **Source (deferred until releases exist).** GitHub releases of `Densetsu152637/Imperium-Models`, tag `ncm-server-v<semver>`,
  archives `ncm-server-<version>-<target>-<cpu|cuda>.{zip,tar.gz}` for `windows-x64` and
  `linux-x64` (CPU and CUDA) and `macos-arm64` (CPU), as the NCM session proposes (user decision
  pending, Q1). AMD, Intel and Apple GPUs get the CPU build until Burn wgpu builds come (a later
  user decision). Besides the interim user path there is no other source: no PATH search, no
  package manager, no Downloads-folder candidate run by itself.
- **Pin.** `runtimes/ncm-server.env` holds `NCM_SERVER_REF` and one `NCM_SERVER_SHA256_<target>`
  per asset, raised only by pull request, with a CODEOWNERS entry requiring the owner's review.
  CODEOWNERS binds only if branch protection on `experimental` and `main` requires code-owner
  review and makes `ncm-pin` a required check. Those are repository settings the user must enable
  (stage 2 lists them in its handoff); without them the entry is advisory.
  The pin governs: an asset whose SHA-256 differs from the pin is refused; GitHub's published
  `digest` is only a consistency check (a mismatch is also refused); an asset with no pin is
  refused. The installer reads the release through the GitHub API at an injectable base URL (so
  tests use a local server) and downloads assets through their API URLs.
- **Provenance, mandatory at pin time.** A CI check (`ncm-pin`) on any pull request that changes
  `runtimes/ncm-server.env` downloads each pinned asset, checks its hash, and runs
  `gh attestation verify` with the signer workflow pinned to Imperium-Models' release workflow and
  the source ref to `refs/tags/ncm-server-v*`. No attestation, another signer or another ref fails
  the check. The client trusts the pinned hash, which that check vouched for.
- **Revocation.** `runtimes/ncm-server.env` also lists `NCM_SERVER_REVOKED` refs. An installed
  build of a revoked ref is never launched; the runtime card says to install the pinned one. The
  list ships in the pinned env file, so a revocation takes effect only once Imperium itself is
  updated to a release carrying it; there is no online revocation check.
- **Signing.** Authenticode follows [item 59](../12-roadmap.md#next-in-order); until then the
  Windows binary is unsigned and the pinned hash covers it. On macOS the archive is unpacked by
  Imperium, not the browser, so no quarantine attribute is set and Gatekeeper does not assess the
  binary; that is acceptable only because of the pinned hash, and a notarised build is a follow-up
  once releases are signed.
- **Unpacking.** Reuse the llama.cpp extractor, moved out of `runtime.rs` into a shared
  `imperium-local` module with no behaviour change: staging folder, regular files and folders
  only, no absolute paths, `..`, drive prefixes, NUL, devices, FIFOs or zip symlinks, the 8 GiB,
  20 000 entry and 200:1 ratio caps, and nothing replaced on failure (the rules of #313 and #316).
- **Consent.** No auto-install. `ncm-server` installs only when the user presses *Install* (the
  Princeps card or the runtime card) or runs `imperium runtime ncm install`; the card states the
  source, version, size and that it is Imperium's own build. A missing runtime fails a load with
  *No ncm-server yet: open Local models → Runtime to install it*.
- **Updates.** `runtime.ncm.update` is `ask` by default: every new pinned ref asks, on the runtime
  card, before anything is downloaded. Only `auto`, which the user must set, installs a new ref by
  itself at the next load. `off` never offers. One earlier build is kept until the new one has
  loaded a model; a new build that fails its check is removed and the earlier one stays.
- **Where and which.** `<data>/runtimes/ncm-server/<ref>/<target>/ncm-server[.exe]` with an
  `imperium-runtime.json` record of every file's SHA-256. Order: `runtime.ncm.custom_path` (a
  developer build; only a path the user sets), then the installed build of the pinned ref, then an
  earlier installed, unrevoked ref until the pinned one is installed.
- **Tamper checks.** `<data>/runtimes/` becomes a protected folder (`protect_dir`, as for
  `<config>/mods` and `plugins`), so no agent tool or command writes there, **and** every launch
  verifies the whole build folder against `imperium-runtime.json`. A `custom_path` build is
  checked against the hash the user approved (the interim rule above).
  - Every file in the folder, NVRTC and its builtins included, must be in the record with a
    matching SHA-256, and a file not in the record refuses the launch (it could be loaded by
    search order). A mismatch or an extra file says to reinstall.
  - **Windows:** each file is opened for hashing without `FILE_SHARE_WRITE` or `FILE_SHARE_DELETE`,
    and the handles stay open for the child's whole life, so nothing can replace a file between
    the check and its use (NVRTC is loaded lazily, at the first kernel compile). The handles close
    when the supervised process exits.
  - **Unix:** the files are hashed through descriptors held open for the child's life, and where
    available (`fexecve` on Linux) the server is executed from the hashed descriptor itself. On
    macOS the executable is hashed and run by path, so the protected folder is the defence there.
  - **Residual:** this defends against agent tools, agent commands and accidents. It does not
    defend against another process of the same user running outside Imperium's tools, which can
    write the data folder; that is the same boundary as for the rest of the data folder.
- **Agents cannot drive it.** These need a person's approval in every permission mode, `auto` and
  bypass included, with a test per mode: `imperium runtime ncm install|remove|update|use`, and
  `imperium config set|unset` of `runtime.ncm.custom_path`, `runtime.ncm.update` and
  `runtime.ncm.extra_args`. The matching daemon methods refuse agent, mod-frame and paired-phone
  callers ([#304](https://github.com/Densetsu152637/Imperium-AI/pull/304) lands the same refusals
  for extension installs).
- **Build choice.** CUDA on an NVIDIA GPU, else CPU. The CUDA build loads the driver and NVRTC
  dynamically (no cuBLAS or cudart) and needs a CUDA 13.x driver (minimum unverified, Q2); NVRTC
  and its builtins ship in the archive or a pinned companion asset, once NVIDIA's redistribution
  terms are verified (before any release, Q20). A CUDA build that fails its
  first `/props` check falls back to the CPU build, as llama.cpp's builds do.

### D4. One loaded model, two profiles

- **One process per Princeps build.** Profiles name a model; the supervisor keys servers by model,
  so the Emperor and Consul profiles on `ncm:princeps-ncm-250m` share one `ncm-server`.
- **One slot, queued in Imperium.** `ncm-server` runs requests one at a time, `--parallel N` only
  divides the context, and no batching is planned (NCM answer 4). So Imperium launches it with
  `--parallel 1` and the full context, and concurrency is the residency controller's slot queue:
  a Consul call waits while an Emperor call runs, with the usual wait reason and `max_wait_s`.
- **Prefix reuse.** The server's prompt-prefix reuse (on a branch, default on) helps a profile
  whose next call extends its last prompt. Alternating profiles evicts that prefix; stage 6
  measures the cost, and if it matters the fix is in the server (a prefix per profile), not a
  second process.
- **Memory.** Weights are about 470 MiB in bf16 (150 to 310 MiB quantised) and the NCM cache tens
  of MiB at 32k and about 1 GiB at 1M tokens. Default context: `runtime.ncm.context = 32768` per
  call (the NCM session agrees), because prefill, not memory, is the cost (128k prefill took 23 to
  28 s); `auto` and up to the model's 1M ceiling are settings.

Rejected: two processes, one per profile. It doubles weights and cache for no throughput, since
batch-1 decode on one GPU is the bottleneck and escalation is sequential anyway. Revisit if the
stage 6 evals show the Consul queue wait above 20 % of turn wall time.

### D5. Harness profiles before Ratio

A **harness profile** is a named bundle: `model`, `system_prompt` (the file body), `tools` (an
allowlist), `workflow` (workflow-contract rule files, [18](../18-workflow-contract.md)), `harness`
(the agent-loop engine: `default` today) and `role: root | subagent | both`. It is item 58's
per-subagent harness, prompt and workflow under one name, so stage 4 is item 58's first slice.

- **Format.** A named-agent file ([06](../06-extensibility.md#named-agents-implemented)) with the
  new fields `workflow`, `harness` and `role`. Profiles and named agents share one namespace and
  the existing trust rules (project files only in trusted projects).
- **Built-in profiles.** `emperor` and `consul` ship in a new lowest-precedence source
  (`crates/imperium-core/profiles/`). Emperor uses the vendored `emperor-root.md`; Consul ships a
  minimal critic prompt marked as a placeholder. The bodies belong to the Harnesses session, which
  replaces them by pull request; the mechanism, tests and names are this plan's.
- **Reserved names and the root role.** `emperor` and `consul` are reserved: only a file in the
  user's `<config>/agents/` replaces them. A plugin or project file with a reserved name, or with
  `role: root` or `both`, loads with a notice but is never used as the root or as a takeover
  target unless the user opts it in. Such a file can still be a subagent through `spawn_agent`, as
  today.
- **The opt-in, `agents.root_trusted`.** A list of entries `{ name, origin, sha256 }`:
  - `origin` is either `plugin:<plugin id>` (the installed plugin's id, `name@marketplace`) or
    `project:<absolute path>` (the project folder's canonical absolute path); nothing else
    matches, and a relative or non-canonical path is refused when the setting is written.
  - `sha256` is the content hash of the profile file and of every `system_prompt` and `workflow`
    file it references, taken when the user approves it. A file whose hash no longer matches is
    not trusted (it falls back to subagent-only use with a notice) until the user approves it
    again, as [#304](https://github.com/Densetsu152637/Imperium-AI/pull/304) does for extensions.
  - It is read **only from the user's own config file**: project config, environment config,
    environment variables and `models.toml` layers are ignored for this key, with a notice when
    one sets it.
  - Writing it is floored (below).
- **Agent folders are on the safety floor** ([05](../05-tools-and-isolation.md)). Writes by agents
  to every agent folder (`<config>/agents/`, `.claude/agents/`, `.imperium/agents/`, plugins'
  `agents/`) and to the built-in source are refused in every mode, extending the project-folder
  floors and `<config>` refusals that
  [#304](https://github.com/Densetsu152637/Imperium-AI/pull/304) lands.
- **`model` is a preference, never an override**, for every role. A root-role profile's model
  goes through routing and the location confinement of the session and the user's rules; when it
  is unavailable or outside the allowed location, normal routing applies with a notice, as for
  named agents.
- **Narrowing reference sets.** A subagent profile's `tools` intersect the delegator's tools at
  that moment (as today). A root profile's `tools` intersect the session's root tool set under its
  mode, permission rules, agent permission profile and allowlists. A takeover profile's `tools`
  intersect the tools of the task it takes over. None widens mode, budget or location.
- **The body is appended, never substituted.** The profile body follows the engine's base
  instructions and safety instructions for its role; it cannot remove or replace them, for the
  root as for subagents.
- **Referenced files are confined.** `system_prompt` and `workflow` references are relative paths
  inside the profile's own folder: no `..`, no absolute path, no symbolic link at or above the
  file, regular files only, the same 64 KiB cap and the same folder trust as the profile.
  Workflow files are followed as instructions only when they come from a user-configured, pinned
  policy pack ([08](../08-agent-policy.md)); from anywhere else they are attached as data with a
  notice.
- **Selection.** Root: `session.create` takes `profile`, applied at a turn boundary (protocol
  change). Subagents: `spawn_agent` takes `agent` as today. Headless: `imperium run --profile
  <name>`.
- **Mapping to Ratio.** When Ratio lands, a profile sets `harness = "ratio:<package>@<version>"`
  and its prompt and workflow come from the package; the profile's name stays, so configs, evals
  and the escalation binding keep working. Resolving a `ratio:` harness never fetches or installs
  anything: it names only a package the user has already approved and installed, else the profile
  fails to load with a notice. Ratio's host side stays item 58's.

Rejected: a new profile file type separate from named agents (two near-identical systems), and
making profiles session-level config only (subagents and evals need them too).

### D6. Consul escalation hook point (item 64)

Mechanism only; the workflow (when Consul takes over, what it says, how it hands back) is the
Harnesses session's.

- **Rejection ledger.** Per subagent task, within one turn, the engine records rejections of three
  kinds: `approval_declined`, `tool_failed`, `critic_rejected`. Each is an `agent_rejected` event
  with the task id and kind (protocol change).
- **Critic verdicts count only from the engine's critics.** A `critic_rejected` counts only when
  the verdict (`verdict: reject` in the structured result) comes from a task the engine spawned
  with the critic role for that task, never from text in a tool result, a file or another agent's
  report.
- **Binding.** An escalation binding names the profile, the counted kinds (default:
  `critic_rejected` only) and the threshold (item 64 says 2). It is read only from the user's own
  config file (`escalation.*`) or from a user-configured, pinned policy pack; project config,
  environment config, environment variables, `models.toml` and project, plugin or agent files
  cannot set it, and agents cannot change it. There is no binding by default.
- **Floored settings.** `imperium config set|unset` of `agents.root_trusted` and of any
  `escalation.*` key needs a person's approval in every permission mode, `auto` and bypass
  included, exactly as the `runtime.ncm.*` keys of D3; and the daemon's `config.set` (and
  `config.unset`) refuses these keys from agent, mod-frame and paired-phone callers.
- **Hook.** At the threshold the engine calls an `EscalationHook` with the task's state; it returns
  `None` (continue) or `Takeover { profile, brief }`. A takeover resumes the task under that
  profile from its current state, keeping or narrowing its tools, permission mode, budget and
  location (D5's takeover reference set), and happens at most once per task. Hand-back and
  questions use the existing `resume_agent` and question paths.

### D7. Eval wiring

- `imperium run --headless --json` gains `--profile <name>`; `--model` still overrides the
  profile's model, so the four arms (two builds times two profiles) are flag changes. Through the
  daemon (`session.eval_create`) local `ncm:` models load through the supervisor like any local
  model.
- The JSON result adds `profile` (name, source and a hash of the resolved profile) and, per model
  call, the server's `timings` (`prompt_per_second`, `predicted_per_second`, `cache_bytes`) when the
  server sends them (a protocol change for the eval fields), so R4 stage 9 measures tokens per
  second inside the agent loop.
- The scored evals themselves are the research sessions' work, run through this harness.

## Current state (found on `experimental` at `0cbc6570`, unchanged at `9ce002c5`)

- `imperium-local`: `RuntimeBackend` with `LlamaCpp` and `Vllm` (`backend.rs`); `Format` has
  `Gguf` and `Safetensors` (`scan.rs`); the hardened installer and extractor live in `runtime.rs`;
  vLLM in WSL2 in `wsl.rs` and `vllm_env.rs`. Spec prefixes are matched in
  `imperium-core/src/{model_config.rs,bench.rs,suggestions.rs}`. Protected folders are set with
  `protect_dir` in `imperium-core/src/state.rs`.
- House models: `apps/desktop/src/models/houseModels.ts` lists Emperor and Consul with no source;
  `HouseModelCard.tsx` shows them on first run. `ModelSuggestionCard.tsx` and
  `[[models.suggestions]]` offer Hugging Face GGUF repositories only.
- Emperor in the engine: `is_emperor` in `imperium-engine/src/model.rs`; `emperor-root.md` in
  `imperium-protocol/schemas/prompts/`.
- Named agents (item 48) and headless evals with `--agent` (item 53) are implemented; item 58
  (harness switch) and item 64 (escalation) are planned with no code.
- `ncm-server` (Imperium-Models `models/ncm/runtime/`, merged in #40; newer work on
  `feat/ncm-speed-2`): llama-server's flags and endpoints, random weights for tests, one weight
  file with an `ncm_format` header, `tokenizer.json` beside it. No release builds, no published
  weights; CI builds CPU only on Ubuntu. Further training is on hold until the Harnesses session
  has built the harnesses.

## Scope and non-goals

In scope: D1 to D7. Not in scope: training, weights and their quality; the harness prompts and
workflow content; Ratio and its host bindings; vision; GPU support beyond CUDA; offering Princeps
through the generic download suggestions.

## Assumptions

- `ncm-server` keeps its llama-server-compatible launch line and endpoints; changes are announced
  by the NCM session and adopted by pinning a new ref.
- Trained weights will be published at a pinned revision with per-file SHA-256 (Q5).
- The research harness suites already run through `imperium run --headless --json`.

## Relevant paths

`crates/imperium-local/src/{backend.rs,scan.rs,runtime.rs,runtime_check.rs,supervisor.rs,planner.rs}`,
a new `crates/imperium-local/src/{ncm.rs,extract.rs}`, `runtimes/ncm-server.env`,
`.github/CODEOWNERS`, `crates/imperium-config/src/{settings.rs,registry.rs}`,
`crates/imperium-core/src/{state.rs,local.rs,model_config.rs,bench.rs,suggestions.rs}`,
`crates/imperium-core/profiles/`, `crates/imperium-engine/src/{model.rs,agent/delegation.rs}`,
`crates/imperium-cli/src/` (`runtime ncm`, `run --profile`), `apps/desktop/src/models/houseModels.ts`,
`apps/desktop/src/panels/{HouseModelCard.tsx,LocalModelsPanel.tsx}`,
`.github/workflows/{ncm-live.yml,ncm-pin.yml}`.

## Related docs

[04](../04-models-and-routing.md) (runtimes; wins on current behaviour),
[05](../05-tools-and-isolation.md) (safety floor), [06](../06-extensibility.md#named-agents-implemented),
[12](../12-roadmap.md) items 44, 53, 58, 59 and 64 (win on scope), Imperium-Models
`models/ncm/runtime/README.md` (wins on the server's contract), Imperium-Research
`docs/next-concept-modelling/` (wins on the models).

## Stages

Each stage is one pull request against `experimental` with its docs, CI green before the next.
Stages 1, 2a, 4 and 5 need nothing from the NCM session. Stage 2b waits for releases (Q1, Q2,
Q13, Q19, Q20, all deferred); stage 3's download waits for published weights (Q5, after
training). Stages 3 to 6 change the protocol and take the next free minor at merge.

**Owners.** On the user's instruction, the Imperium Princeps (NCM) session implements stage 1 on
its branch `feat/ncm-runtime`. Stage 2b (releases) is deferred. Stages 2a and 3 to 6 stay with
the Imperium Implementation session, which also reviews and merges stage 1.

### Stage 1: `ncm` backend and format

- Owner: the Imperium Princeps (NCM) session, branch `feat/ncm-runtime` (user instruction,
  2026-11-26). The work must meet the bar of D1 to D3: the format, header limits and catalogue
  rows of D1; the backend, launch arguments, environment allowlist, request rules and capability
  fallbacks of D2; and D3's rules where stage 1 touches them (`runtime.ncm.custom_path` set only by
  the user, the floored `runtime.ncm.*` keys, and **copy on approval**: stage 1 launches only the
  approved copy under `<data>/runtimes/ncm-server/user/<sha256>/`, with that folder as the working
  folder, never the configured path itself). The Imperium Implementation session reviews it
  against this plan and merges it.
- Objective: D1 and D2 with `runtime.ncm.custom_path` as the only way to name a server.
- Depends on: none.
- Owned paths: `imperium-local` `backend.rs`, `scan.rs`, new `ncm.rs`; spec prefixes in
  `imperium-core`; `runtime.ncm.*` settings and registry; docs 04 and 10.
- Acceptance checks (CI): the scanner lists a marked file as `ncm:<stem>`, ignores an unmarked
  file and lists an oversized header with a `problem`; out-of-range or overflowing header numbers
  give a `problem`, never a panic or a footprint; launch arguments match a golden list; `--host`
  and `--port` in `extra_args` are refused and dropped; the child's environment holds only the
  allowlist (with `CUDA_PATH`); a fake `ncm-server` answering 503 then 200 on `/health` is started
  once and served, and every request carries `max_tokens`; a cancelled call releases its slot, and
  a fake that keeps streaming after the disconnect holds the slot until it ends; a fake that exits
  non-zero fails the load with its log tail; `place` never splits; the CPU lane accepts an `ncm:`
  row; a forced tool choice on an `ncm:` row falls back to `auto`.
- Status: in progress (Imperium Princeps session, `feat/ncm-runtime`).

### Stage 2a: the user-built `ncm-server` (interim)

- Objective: D3's interim path: `imperium runtime ncm use|status` and *Choose ncm-server…* on the
  runtime card, the approval with path, size, SHA-256 and `--version`, the copy on approval and
  `user.json` in a protected `<data>/runtimes/`, the launch-time re-hash through held handles, the
  floored commands and config keys, and the opt-in `ncm-live` workflow (manual and nightly) that
  builds `ncm-server` for the CPU with `cargo build --locked --release` from a commit pinned in
  `runtimes/ncm-server.env` (`NCM_SERVER_COMMIT`, for CI only) and loads it with
  `--random-weights`. The workflow has `permissions: contents: read` and no secrets.
- Depends on: stage 1.
- Owned paths: `imperium-local` `runtime_check.rs`, new `ncm_runtime.rs`; `imperium-core`
  `state.rs`; `imperium-cli` runtime commands; `runtimes/ncm-server.env`;
  `.github/workflows/ncm-live.yml`; docs 04 and 11.
- Acceptance checks (CI): a path that is relative, a symbolic link or not a regular file is
  refused; nothing runs before the user approves the hash; the approved file is copied into
  `user/<sha256>/` and the launched program is that copy, with that folder as its working folder; a
  DLL (or, on Unix, a shared library) planted beside the original binary is never loaded (Windows
  test with a fake server that reports the DLL it loaded); changing the original after approval
  changes nothing until it is approved again; a copy changed after approval is refused at launch;
  on Windows, replacing or deleting the copy while the server runs fails (handle held without write
  or delete sharing) and the handle is released when it exits; on Linux the server is executed from
  the hashed descriptor; `config set|unset runtime.ncm.*` and `runtime ncm use` need approval in
  each permission mode (one test per mode), and the daemon methods refuse agent, mod-frame and
  phone callers; agent writes into `<data>/runtimes/` are refused. a CI script check
  (like `check-monty-pin.sh`) fails unless `ncm-live.yml` builds with `--locked`, sets only
  `contents: read` and references no secrets. ncm-live: build at the pinned commit,
  `/health` 200, one chat completion.
- Status: pending.
- Handoff notes: the CUDA build is untested in CI (ncm-live builds for the CPU); a CUDA build's
  NVRTC comes from the user's toolkit and is not hashed (residual, D3).

### Stage 2b: release download and verification (deferred until releases exist)

- Objective: the rest of D3: `imperium runtime ncm install|remove|update`, the shared extractor,
  `NCM_SERVER_REF` and per-asset SHA-256 pins with CODEOWNERS, the `ncm-pin` attestation check,
  revocation, updates, whole-folder verification with held handles, and ncm-live switched to the
  pinned release.
- Depends on: stage 2a; Imperium-Models releases with attestations (Q1, Q13, Q19) and NVRTC
  redistribution verified (Q20).
- Owned paths: `imperium-local` `runtime.rs` (extractor moved out), new `extract.rs`;
  `imperium-cli` runtime commands; `.github/workflows/{ncm-pin.yml,ncm-live.yml}`,
  `.github/CODEOWNERS`; docs 04 and 11.
- Acceptance checks (CI, against a local fake GitHub API): the llama.cpp extractor tests pass
  unchanged against the moved module; an asset whose hash differs from the pin, or from GitHub's
  digest, or with no pin, is refused and nothing is replaced; hostile archives are refused; a
  revoked ref is not launched; a binary, an NVRTC library or a builtin changed after install is
  refused at launch, and so is an extra file in the build folder; on Windows, replacing or deleting
  a verified file while the server runs fails, and the handles are released when it exits; nothing
  installs without the explicit call, and under `update = "ask"` a new ref waits for the user;
  `runtime ncm install|remove|update` need approval in each permission mode (one test per mode);
  `ncm-pin` fails on a missing attestation and on another signer (fixture). ncm-live: install,
  `/health` 200, one chat completion.
- Status: deferred (user decision 2026-11-26: no releases yet).
- Handoff notes: ask the user to enable, in the repository settings, branch protection on
  `experimental` and `main` that requires code-owner review and makes `ncm-pin` a required check;
  until then the CODEOWNERS entry is advisory.

### Stage 3: Princeps in the catalogue and the app

- Objective: one Princeps entry replaces Emperor and Consul in `houseModels.ts`, with its pinned
  file set (weights, `tokenizer.json`) and SHA-256s; a daemon method `models.install_house`
  downloads the set, verifies every file and offers to install `ncm-server` in the same consent;
  the Local models panel shows `ncm:` models and an `ncm-server` runtime card; the `princeps` tag
  and the `emperor` alias (D1).
- Depends on: stages 1 and 2a; Q5 for the real source (Hugging Face under the user's account,
  Apache-2.0, pinned revision and per-file SHA-256, after training; the card keeps *not yet
  available* until then). The card's offer to install `ncm-server` in the same consent waits for
  stage 2b; until then it points to the runtime card's *Choose ncm-server…*.
- Owned paths: `houseModels.ts`, `HouseModelCard.tsx`, `LocalModelsPanel.tsx`, the protocol method
  and its TS types, `imperium-engine` `model.rs`; docs 04, 07 and 12 item 44.
- Acceptance checks (CI): catalogue parsing tests; a download whose file hash differs is deleted
  and reported; `models.install_house` is refused to agents (no tool reaches it), mod frames and
  paired phones; knowledge routing prefers a `princeps` row and still honours `emperor`; the card
  never shows a download without a source.
- Status: pending.

### Stage 4: harness profiles

- Objective: D5: the profile fields on agent files, the built-in source with `emperor` and
  `consul`, reserved names, the root opt-in, agent folders on the floor, root selection on
  `session.create`, `--profile` on `imperium run`.
- Depends on: none for the mechanism; stage 1 for the shared-launch test.
- Owned paths: agent-file parsing and the built-in source in `imperium-core`, the floor rules,
  `session.create`, `imperium-cli` run; docs 03, 05, 06 and 12 item 58.
- Acceptance checks (CI): a file named `consul` in `<config>/agents/` replaces the built-in, and
  one in a plugin or project does not; a plugin or project `role: root` profile is not used as the
  root or a takeover target without the opt-in, and is with it; an opt-in whose hash no longer
  matches the file (or a referenced prompt or workflow file) is not honoured until approved again;
  an `origin` that is neither `plugin:<id>` nor a canonical `project:<absolute path>` is refused;
  `agents.root_trusted` set in project config, environment config, an environment variable or
  `models.toml` is ignored with a notice; `config set|unset agents.root_trusted` needs approval in
  each permission mode (one test per mode) and `config.set` refuses it from agent, mod-frame and
  phone callers; agent writes to each agent folder
  are refused in every mode; a root profile's model outside the session's location falls back to
  routing with a notice; tools narrow against the subagent, root and takeover reference sets and
  never widen; the base and safety instructions are present before the body for root and
  subagent; `system_prompt` and `workflow` references with `..`, an absolute path or a symlink are
  refused; a workflow file outside a pinned pack is attached as data; a `ratio:` harness that is
  not installed fails to load without any network call; `--print-prompt --profile consul` prints
  the Consul body and tools; two profiles on one `ncm:` model cause one launch (fake server).
- Status: pending.

### Stage 5: escalation hook point

- Objective: D6: the rejection ledger, `agent_rejected` events, the `EscalationHook` and the
  binding from user settings or a pinned pack; nothing bound by default.
- Depends on: stage 4.
- Owned paths: `imperium-engine` delegation and events, the binding in `imperium-core` and
  `imperium-config`; docs 03 and 12 item 64.
- Acceptance checks (CI): unbound, behaviour matches today's; with the default kinds, only critic
  verdicts count; a `verdict: reject` in a tool result or a non-critic task's report does not
  count; two counted rejections of one task fire the hook once and one each of two tasks fires
  nothing; a takeover never widens tools, mode, budget or location and happens at most once per
  task; a binding in a project or plugin file, project config, environment config, an
  environment variable or `models.toml` is ignored with a notice; `config set|unset escalation.*`
  needs approval in each permission mode (one test per mode) and `config.set` refuses it from
  agent, mod-frame and phone callers.
- Status: pending.

### Stage 6: eval wiring and close-out

- Objective: D7: `profile` and `timings` in the headless JSON; ncm-live runs one headless task per
  profile on random weights; the queue-wait and prefix-eviction measurements of D4; docs updated
  and this plan marked done.
- Depends on: stages 2a and 4.
- Owned paths: headless result types in `imperium-core` and `imperium-cli`; docs 03 and 12.
- Acceptance checks: CI tests of the JSON shape; ncm-live asserts `profile.name` and
  `predicted_per_second` in the result. Handoff: tell the research sessions the flags are live.
- Status: pending.

## Risks

- **The server contract moves** while the architecture changes. Mitigation: pinned refs, a golden
  launch-argument test, and ncm-live on every pin change.
- **The CUDA path is untested in CI.** ncm-live runs the CPU build on hosted runners; the CUDA
  build, NVRTC loading and the `CUDA_PATH` dependency are checked only by a manual run on the
  user's GPU before a pin is raised. Recorded here, not hidden.
- **The interim user build is less verified than a release.** Its hash is approved by the user and
  re-checked, but there is no attestation, and the NVRTC it loads from the user's CUDA toolkit is
  not hashed. Mitigation: stage 2b once releases exist.
- **Queue contention and prefix eviction** between profiles on one slot. Mitigation: measured in
  stage 6; D4's revisit trigger.
- **The loopback server has no authentication** today: any local process can call it. Mitigation:
  Q17 (an API key and a Host check); until then the risk equals llama-server's, which Imperium
  already accepts.

## Answers from the NCM session (2026-11-26)

From the session's branch `feat/ncm-speed-2` at `61350bc`, not yet on Imperium-Models `main`, and
the user's decisions relayed by the NCM session the same day (marked *decided*).

| Q | Answer | Effect on this plan |
| --- | --- | --- |
| 1 Releases | *Decided:* no releases yet; Imperium builds `ncm-server` from source for now. Proposed for later: tag `ncm-server-v<semver>`, archives `ncm-server-<ver>-<target>-<cpu or cuda>`, Windows and Linux x64 CPU and CUDA, macOS arm64 CPU. Deferred. | D3 interim user-built path; stage 2b deferred. |
| 2 CUDA | Dynamic via cudarc 0.19: driver and NVRTC only; CUDA 13.4 symbols, so a 13.x driver (minimum unverified); CubeCL reads `CUDA_PATH` for NVRTC headers; NVRTC and builtins to ship. | D2 keeps `CUDA_PATH`; D3 ships NVRTC. |
| 3 Readiness | `--version` prints 0.0.0; no `--list-devices`, `--check` or footprint flag yet (proposed). Cache size is `ncm_model::cache_bytes_with`; `/props` reports the actual. | D2 mirrors the formula and uses `/props`. |
| 4 Concurrency | `--parallel` splits context only; sequential; prompt-prefix reuse on the branch (default on, `--no-prefix-reuse`); no batching planned. | D4: one slot, Imperium queues. |
| 5 Weights | *Decided:* Hugging Face under the user's account, Apache-2.0, pinned revisions and per-file SHA-256, only after training; file names to be decided. Tokenizer from `--tokenizer`, else beside the weights, else an error. | Stage 3's download waits for training. |
| 6 Format | `ncm-safetensors-v1` is stable; the header also has `ncm_config` and `ncm_quant`; `ncm_model_id` and `ncm_model_version` proposed; exact-architecture loading. | D1. |
| 7 Quantisation | Separate quantised files, or dense plus `--quant q8` or `q4` at load. | D2: by file layout; `--quant` via `extra_args`. |
| 8 Flags | `--speculate` off by default (leave off); 8-bit indexer keys on by default. | D2. |
| 9 Context | 32k is fine (tens of MiB); the first request compiles kernels in 0.4 to 0.65 s; load time unmeasured. | D4 default 32k. |
| 10 Tools | `tool_choice` only `none` or `auto`; no `response_format` or schema; no stop strings; `reasoning_content` implemented, show it. | D2 copes; reasoning shown. |
| 11 GPUs | *Decided:* CUDA and CPU now; AMD, Intel and Apple GPUs later through Burn wgpu. | D3: CPU build on those GPUs for now. |
| 12 Network, env | No outbound network; binds `--host:--port`, default `127.0.0.1`; reads `CUBECL_*`, `BENCH_*` and `CUDA_PATH`. | D2 allowlist with `CUDA_PATH`. |
| 13 Provenance | `SHA256SUMS` and attestations planned with releases; deferred with them. | D3 makes attestation mandatory (stage 2b). |
| 20 Licences | *Decided:* the Imperium-Models repository and `ncm-server` are Apache-2.0. NVRTC redistribution is to be verified before any release. | D3 ships NVRTC only once verified. |
| Queue | *Decided:* Q2, Q3, Q6, Q9 and Q14 to Q18 are queued as `ncm-server` work after the NCM session's current benchmark. | Stage 1 uses the fallbacks in D1 and D2 until they land. |

## Open questions for the NCM session

Status: Q1, Q13 and Q19 are **deferred** until releases exist; Q2, Q3, Q6, Q9 and Q14 to Q18 are
**queued** as `ncm-server` work after the NCM session's current benchmark; Q5 and Q11 are decided
(answers table); Q20 is half decided (Apache-2.0) with NVRTC redistribution still to verify.

Still open from the first round (numbers as in the answers table):

- **Q1, Q13 (deferred). Release decision.** Confirm the tag scheme, targets, and that releases ship
  `SHA256SUMS` and attestations, once releases are scheduled.
- **Q2. CUDA minimum.** The minimum NVIDIA driver, and whether NVRTC headers ship so `CUDA_PATH` is
  no longer needed.
- **Q3. Introspection.** A real `--version` (version, commit, features), `--list-devices` or
  `--check`, and a `--print-footprint`.
- **Q5 (decided except file names).** The weight and tokenizer file names, once training ends.
- **Q6. Header fields.** Add `ncm_model_id` and `ncm_model_version`, and say which server versions
  load which weight files.
- **Q9. Load time.** How long a restart with a larger cache takes.

New, from the plan review:

- **Q14. Config bounds and fuzzing.** Does the server bound every header and config value (layers,
  widths, vocabulary, context) with checked arithmetic, and is the header parser fuzzed? A weight
  file is untrusted input.
- **Q15. Tokenizer safety.** Can the header carry the tokenizer's SHA-256 so a mismatched
  `tokenizer.json` is refused, and is the tokenizer's regex (the `onig` feature) safe against
  pathological input (catastrophic backtracking)?
- **Q16. Disconnect cancels.** Does a dropped client connection stop generation at once, freeing
  the server for the next request?
- **Q17. Loopback authentication.** Can the server take `--api-key` (read from a file or the
  environment, never the command line) and refuse requests whose `Host` header is not a loopback
  address (a DNS-rebinding defence)?
- **Q18. Autotune cache.** Does CubeCL write an autotune or kernel cache at run time, and where? It
  should go under a folder Imperium passes (inside `<data>`), not the user's home or the binary's
  folder, and must not be trusted across versions.
- **Q19 (deferred). Release integrity.** Are `ncm-server-v*` tags protected so only the release workflow can
  create them, are releases made only by CI, and which workflow path should Imperium pin as the
  attestation signer?
- **Q20 (NVRTC part open).** The binary and repository are Apache-2.0; still to verify: the
  licences of bundled third-party code, and whether NVRTC and its builtins may be redistributed in
  the archive under NVIDIA's terms, before any release.

## Questions for the Harnesses session (for later relay)

- The critic's structured result: is `verdict: accept | reject` plus a reason enough for D6's
  ledger, or does the Consul harness need more fields?
- Which tool set should each profile start with (D5 ships Emperor with the root's tools and Consul
  read-only)?
- Without forced tool calls or schemas on Princeps (D2), should the Consul verdict be a plain-text
  convention the engine parses rather than a structured output?
