# 04 — Models and routing

## Model sources

| Source               | Transport                                  | Notes                                       |
| -------------------- | ------------------------------------------ | ------------------------------------------- |
| Hosted endpoint      | OpenAI-compatible Chat Completions / Responses | OpenRouter, Together, Groq, DeepInfra, self-hosted, vendor APIs |
| Anthropic-compatible | Messages API                               | Optional adapter                            |
| llama.cpp            | Supervised `llama-server` (OpenAI-compatible) | Native on all platforms                  |
| vLLM                 | Supervised `vllm serve` (OpenAI-compatible) | Linux, WSL2, Docker, or remote host        |
| External local       | Ollama, LM Studio, TabbyAPI, etc.          | Registered as endpoints; not supervised     |

Everything speaks OpenAI-compatible HTTP internally, so routing and telemetry are uniform.

## Model registry

Each entry records:

- **Identity:** id, display name, source, family, quantisation, file path or endpoint.
- **Capabilities:** tool calling (native / grammar), vision, audio, context length,
  structured output, reasoning mode.
- **Quality tier:** `basic`, `standard`, `strong`, `frontier` — user-editable, seeded from
  bundled defaults for well-known families.
- **Price:** USD per million input/output/cached tokens for hosted; for local, an optional
  energy cost per hour (default 0) plus an occupancy cost (memory held).
- **Footprint (local):** weights size, KV-cache bytes per token, runtime overhead.

Hosted catalogues are refreshed from endpoints that list models (`/v1/models`, OpenRouter's
pricing API); users can pin or hide entries.

## Local model discovery

**Implemented (phase 4):** `models.folders` are scanned recursively when the daemon starts
(symbolic links to folders are not followed). GGUF files, split sets (`name-00001-of-0000N`,
counted once with the parts' total size, skipped if a part is missing), and `mmproj`
projectors (attached to the other models in their folder) are recognised. Each model's id is
its file name without `.gguf` or the split suffix; use it as `llamacpp:<id>`. GGUF headers are
read for architecture, layers, head counts, embedding and head sizes, context length, and
quantisation, skipping the tokenizer arrays. `imperium models local` and the desktop's Local
models panel list what was found and whether each fits. Hugging Face folders (`config.json`
and safetensors) are found too, for vLLM. Watching folders for changes and network and SSH
paths are later work.

- Users configure any number of **model folders** (local paths, network shares, or paths on an
  SSH host). The daemon scans them recursively and watches for changes.
- Recognised: GGUF files (including split `-00001-of-0000N` sets and `mmproj` projectors) for
  llama.cpp; Hugging Face directories (`config.json` + safetensors) for vLLM; diffusion model
  folders for the media sidecar.
- GGUF headers are parsed for architecture, layer count, head counts, embedding size, context
  length, and quant type — enough to compute the memory footprint without loading the model.
- Optional Hugging Face Hub search and download into a chosen folder.

## Downloading from Hugging Face

**Implemented:** the daemon downloads models from the Hugging Face Hub into
`models.download_dir` ([configuration](10-configuration.md#implemented-settings)), under
`<owner>/<repo>/`, where the scanner finds them. The folder is scanned with `models.folders`
(when it is not already inside one), and again when a download finishes, so the model is in
`models.local` at once without a restart.

```bash
imperium models files Qwen/Qwen3-8B-GGUF          # quantisations, sizes, and whether each fits
imperium models pull Qwen/Qwen3-8B-GGUF            # Q4_K_M by default
imperium models pull Qwen/Qwen3-8B-GGUF --quant q8_0
imperium models pull unsloth/gemma-3-4b-it-GGUF --mmproj
imperium models pull owner/repo --file model-Q5_K_M-00001-of-00002.gguf --revision main
imperium models pull Qwen/Qwen3-8B                 # a safetensors folder, for vLLM
imperium models cancel dl-1
```

- **What is chosen.** The Hub's file listing (`/api/models/<repo>/tree/<revision>`) gives each
  file's size and, for large files, its SHA256. In a GGUF repository each quantisation is one
  choice; it is read from the file name (`Q4_K_M`, `IQ4_XS`, `BF16`, `MXFP4`, ...) or, failing
  that, the folder, and matched without regard to case. Split sets (`-00001-of-0000N`) are
  downloaded whole, even when `--file` names one part. With neither `--quant` nor `--file`, the
  only quantisation is taken, else `Q4_K_M`, else the safetensors folder. `mmproj` projectors
  are left out unless asked for (`--mmproj` takes the F16 one when there are several). In a
  safetensors repository the top-level weights, `*.json` (config, generation config,
  tokenizer, index), and tokenizer files (`tokenizer.model`, `*.tiktoken`, `merges.txt`,
  `vocab.txt`, `*.jinja`) are downloaded; `.bin` and `.pth` checkpoints and subfolders such as
  `original/` are not.
- **Fit.** `models.hf_files` estimates whether each choice fits this machine from its size
  alone (the header is not read yet): the KV cache is taken as a fifth of the weights, and the
  answer is `gpu`, `partial`, `cpu`, or `none`, as in `models.local`. vLLM folders need Linux
  and the GPU.
- **Downloading.** Each file streams to `<file>.part`; an interrupted download resumes from it
  with a `Range` request (from scratch if the server ignores the range). The SHA256 is checked
  when the listing gave one, and a mismatching file is deleted. Only then is the file renamed
  into place. A file already present with the listed size is kept. Downloads run in the
  daemon, so closing the terminal does not stop them; `imperium models cancel` does, leaving
  the `.part` files to resume from. One download per repository runs at a time.
- **Tokens.** Gated and private repositories need a token, sent as `Authorization: Bearer`:
  `HF_TOKEN` in the daemon's environment, or a keychain secret named `huggingface`
  (`imperium secret set huggingface`). A request without one that the Hub refuses says so; the
  token is never logged. Accept a gated model's terms on its Hub page first. `HF_ENDPOINT`
  points the daemon at a mirror, as it does for `huggingface_hub`.
- **Protocol.** `models.hf_files` (`repo`, `revision?`) returns the variants, the default, the
  projectors, every file, and the destination. `models.download` (`repo`, `quant?`, `file?`,
  `revision?`, `mmproj?`) checks the request against the listing, then returns a
  `DownloadJob` whose `job_id` the `models.download_progress` notifications carry (file,
  bytes done and total, `state` of `running`, `done`, `failed` with a `message`, or
  `cancelled`). `models.downloads` lists jobs since the daemon started;
  `models.download_cancel` (`job_id`) stops one. The desktop's Local models panel has the same
  form, with progress bars.

## Runtimes

**Implemented (phase 4):** `imperium runtime install [--pack <id>]` downloads the pack for
this machine (or `runtime.llamacpp.backend`) from the `runtime-llama.cpp-<ref>` release,
checks it against the release's `SHA256SUMS`, and unpacks it under
`<data>/runtimes/llama.cpp/<ref>/`. `runtime.llamacpp.custom_path` points at your own
`llama-server` instead. The daemon starts one `llama-server` per loaded model on a free
loopback port with `--parallel` slots, waits for `/health`, logs its output to
`<data>/logs/llama-<id>.log`, and stops it when it has been idle for `idle_unload_s` or the
daemon exits. vLLM is later work.

**Implemented (stage 2c):** one loaded local model serves many sessions at once. The daemon has
one supervisor, loaded models are keyed by id, and every session that asks for `llamacpp:<id>`
reuses the same `llama-server`. What changed is how long a slot is held:

- **A lease per model call.** An agent takes a slot immediately before each streamed request and
  releases it when the request ends, whether it succeeded or failed. Tool calls, `await_tasks`,
  waiting for the user, and usage-limit pauses hold no slot, so any number of roots asleep in
  `await_tasks` leave the slots to the sessions that are working, even on a model with a single
  slot. The model is loaded on the first call, and stays loaded within `idle_unload_s` of the last
  call that used it. Hosted endpoints are unaffected.
- **A queue.** Calls that find every slot busy wait first-come, first-served. `models.local`,
  `imperium models local`, and the desktop's Local models panel show each loaded model's slots in
  use and `queued` (calls waiting), and each `model_call_completed` event carries `queue_wait_ms`,
  the time that call waited for a slot (`duration_ms` and the time to first token start after it;
  absent for hosted models).
- **Slot affinity.** The supervisor remembers which agent (session and agent id) used each slot
  last. A call goes to the free slot its agent used last, whose KV cache still holds the agent's
  prompt, unless another agent has used it since; otherwise to the slot that has been free the
  longest, so the prompts used most recently survive longest. The lease carries the slot index
  and the request is pinned to it with llama.cpp's `id_slot` (also accepted on
  `/v1/chat/completions`), together with `cache_prompt: true` so only the part of the prompt the
  slot has not seen is processed. vLLM chooses its own sequences and receives neither field.
- **A prompt cache in host memory.** `llama-server` starts with `--cache-ram`, set from
  `runtime.llamacpp.cache_ram_mb` (default 8192 MiB, llama.cpp's own default) and capped to the
  RAM left in the budget once the model and every model that stays loaded beside it are counted,
  so committed RAM never exceeds the budget; `0` turns it off. The cache counts as RAM held by that
  model, so models loaded later are planned without it. When a pinned slot holds another agent's
  prompt, llama.cpp saves that prompt to the cache and restores the requesting agent's own by
  prefix, so a switch costs host memory instead of re-reading the prompt. This relies on the
  server's default `--slot-prompt-similarity` (Imperium never sets it to 0, which would skip both
  the save and the restore).

**Implemented (stage 2d): subagent-first model swapping.** On a machine that cannot hold an
orchestrator's model and its subagents' model at once, the work still runs locally: the
orchestrator, asleep in `await_tasks`, is swapped out with its context saved, the subagents run,
and the orchestrator comes back with its context restored. The daemon's one supervisor is the
memory arbiter for every session (`crates/imperium-local/src/arbiter.rs` holds the policy):

- **Who may be swapped out.** A model with a slot in use, or a call queued for one, is never
  unloaded. Agents tell the runtime what they are doing: an agent is attached to its model for
  its turn, and marked waiting while it is in `await_tasks`. A loaded model is *free* when none
  of the agents that took a slot on it since it was loaded is mid-turn (all are waiting or done),
  and *claimed* otherwise.
- **When.** A model that fits beside the loaded models loads beside them. Free models are
  swapped out, least recently used first and as few as needed, only when the newcomer does not
  fit beside them, or would get less than half the GPU memory beside them that it gets once they
  are gone (so a subagent model does not run mostly from RAM next to a sleeping orchestrator). If
  only a claimed model is in the way, it waits for that model's agent to start waiting or finish
  its turn (an orchestrator that spawns a subagent usually starts waiting moments later), for up
  to `min_residency_s`. After that it loads beside the claimed model if it fits at all, and swaps
  the claimed model out only when nothing else fits and that model has been loaded at least
  `min_residency_s`.
- **Bounded waits.** A call whose model is in the way of models in use waits for them and then
  loads, including a model swapped out between one agent's calls, instead of failing. The wait
  is bounded: a model too big for the whole budget fails at once; a model that does not fit
  beside the loaded ones while swapping is paused (the `max_swaps_per_min` limit was reached
  within the last minute, or it is 0) fails at once with the same not-enough-memory error; and
  any call, whether waiting for room or for a slot, fails after `max_wait_s` (default 600 s) with
  an error naming the setting.
- **Swap out with state.** Before a model is unloaded, each slot holding the prompt of an agent
  that can still come back (mid-turn, or waiting for its subagents) is saved with `POST /slots/{id}?action=save` into `<data>/swap`, which `llama-server` is started
  with as `--slot-save-path`. Files are keyed by the agent and by the model: its id, the file's
  path, size, and modification time (not a hash of the contents, which would read gigabytes on
  every load), the per-slot context, and the cache type, so a stale file is never restored. When
  the model loads again, each saved agent gets its old slot back while it is free, and its file
  is restored with `action=restore` into the slot it leased before its next request is sent; a
  failed restore only means the prompt is read again. The slots of agents whose turn is over,
  such as finished subagents, are not saved; nor is a root agent's between its turns, which reads
  its conversation again if its model was swapped out meanwhile. Models unloaded after
  `idle_unload_s` are saved the same way. If `<data>/swap` cannot be created, saving is off for
  that run (a warning is logged) and `llama-server` is started without `--slot-save-path`.
- **Cache, do not discard.** Saved files are kept up to `swap_cache_mb` of disk. Past that, the
  files of agents that can no longer come back go first; only when none is left do files of
  agents that can, the oldest of the session holding the most, so one busy session cannot push
  every other session's saved context out. Files a previous daemon run left are removed at start.
  Weights are memory-mapped, so a model swapped out recently reloads from the OS page cache.
- **Batch by model.** Calls for a model that is not loaded wait in arrival order, so priority
  ages with waiting time, and only the earliest may load. A loaded model keeps serving the agents
  already in its batch (those that took a slot since it was loaded), but a new agent joins only
  while no earlier call for another model is waiting. Three sessions' subagent tasks for one
  model therefore run in a single load, and an orchestrator that wakes is served right after the
  batch ahead of it.
- **No thrashing.** Loads and unloads run one at a time (they contend for the disk and the PCIe
  bus); a call that arrives mid-swap waits its turn. A swapped-out server is stopped and its
  process has exited before the next model starts, and a launch that fails is tried once more.
  At most `max_swaps_per_min` swaps happen per minute; beyond that, calls that need a swap fail
  (see bounded waits), and the router treats such models as unavailable so new tasks go to the
  next suitable row.
- **The prompt cache yields.** When a model is swapped out only because another model's
  `--cache-ram` took the RAM it needs, the newcomer's cache is sized to leave room for it to come
  back without its cache, so after one restart both stay loaded with smaller caches instead of
  taking turns.
- **Visibility.** Every client receives `model.swapped_out` and `model.swapped_in` notifications
  (`ModelSwap`: `model_id`, `seconds`, `bytes`, `slots`, and a `reason`: `room` or `idle` out,
  `swap` or `restore` in). Out counts the slot bytes written and the time to save and stop; in
  counts the memory taken and the time to start, and `slots` is how many saved slots wait for
  their agents.

Not done in stage 2d: per-session ceilings on slots and on `--cache-ram` (llama.cpp manages its
prompt cache itself); shrinking the `--cache-ram` of a model that is in use or claimed, which
needs a restart of its server (only free models give their cache up, by being swapped out); the
longest wait per session; swap events in the desktop's Models panel; and the measured run on a
16 GB GPU.

**Slots × context (stage 2c, partly open).** `--ctx-size` is the per-slot context times the
slots (`runtime.llamacpp.context` × `runtime.llamacpp.parallel`), and the memory planner counts
the KV cache for all of it, so more slots or a bigger window both cost memory. There is no
setting yet that picks the largest window that still allows N slots for a given amount of free
memory, or that prefers more slots over a bigger window; that belongs with the context tiers of
the Emperor plan.

**Implemented (phase 7):** Hugging Face model folders (a `config.json` with safetensors files)
in `models.folders` are discovered as `vllm:<folder name>`, their shape read from `config.json`
(including `text_config` for multimodal models). They are planned GPU-only (vLLM does not
offload to system memory) and served by a supervised `vllm serve <folder>
--served-model-name <id>` on a free loopback port, with `--max-model-len` (the per-slot
context), `--max-num-seqs` (the slots), and a `--gpu-memory-utilization` sized to the model's
share of free VRAM plus 5%, capped by `runtime.vllm.max_gpu_memory_utilization`. vLLM is
available on Linux (including WSL2) through `runtime.vllm.command` (default `vllm` on `PATH`);
elsewhere such models are listed but unavailable, so routing falls back. Managed `uv`
environments and the Docker image are later work; on another platform, run Imperium on a
Linux host and use it through [SSH](05-tools-and-isolation.md#ssh-hosts).

The supervisor does not know either runtime by name: it holds an ordered list of runtime
backends (`RuntimeBackend` in `imperium-local`), llama.cpp then vLLM by default, and serves a
model with the first available one that accepts its format. A backend decides where a model
fits (GPU and RAM, or GPU only), the server's arguments, and checks its executable, which the
process launcher looks up by the backend's id. Adding a runtime means implementing the trait
and passing it to `Supervisor::with_backends`.

- **llama.cpp:** runtime packs per backend (CPU, CUDA, Vulkan, ROCm, Metal) are built by CI
  ([11](11-ci-cd.md#runtime-packs)) and downloaded on demand, verified by SHA-256. Users can
  point at their own `llama-server` build instead.
- **vLLM:** installed into a managed `uv` environment on Linux/WSL2, or run from the official
  Docker image, or used on a remote host. Imperium never bundles vLLM.
- Each loaded model is a supervised process with a port, health check, idle timeout, and
  memory reservation. Multiple slots (`--parallel`) let one loaded model serve several
  agents concurrently; an agent holds a slot only while a model call runs.

## Hardware awareness

**Implemented (phase 4):** at start the daemon probes system memory and cores (`sysinfo`) and
NVIDIA GPUs (`nvidia-smi`); Apple Silicon is planned as unified memory. `[hardware] vram_mb`
and `ram_mb` override the probe for GPUs it cannot read, or to plan as if memory were smaller.
The budget is the free VRAM and the available RAM less `runtime.llamacpp.ram_headroom_mb`,
measured once at start; loaded models reserve what the planner estimated (the runtime overhead
is a fixed 512 MiB). AMD/Intel/Metal probes, live re-measurement, and calibration against
measured usage are later work. `backend = "auto"` picks `cuda13` for an NVIDIA driver of the
580 series or newer, `cuda12` for older drivers, `metal` on Apple Silicon, and `cpu`
otherwise; set `vulkan` or `rocm` explicitly.

The daemon maintains a live **resource map**:

- GPUs: vendor, VRAM total/free (NVML, ROCm SMI, DXGI, Metal), compute backend support.
- System RAM, unified memory (Apple Silicon), CPU cores.
- Reservations held by loaded models and pending loads.

**Memory planner** for a candidate local model:

```text
required = weights + kv_bytes_per_token × context × parallel_slots + runtime_overhead
fits     = required ≤ free_vram (full offload)  or  partial offload within RAM headroom
```

In the implementation, `context` is per slot, so llama.cpp is started with
`--ctx-size context × parallel`. When a model does not fit on the GPU, as many whole layers as
fit (after the overhead) are offloaded and the rest must fit in the RAM budget; otherwise the
model does not fit.

KV bytes per token come from GGUF metadata (layers × KV heads × head dim × 2 × cache-type
bytes). Estimates are calibrated against measured usage after each load and stored per model.

## Routing

**Implemented (phase 3, router v1):** routing uses your **model table**, a list of
`[[models.catalog]]` rows that delegation rules and agents refer to by id:

```toml
[[models.catalog]]
id = "qwen-local"                     # unique; used by rules and spawn_agent
spec = "llamacpp:qwen3-coder-30b"     # <endpoint>:<model>
location = "local"                    # local | cloud (required)
tier = "standard"                     # basic | standard | strong | frontier
tags = ["code"]

[[models.catalog]]
id = "sonnet"
spec = "openrouter:anthropic/claude-sonnet"
location = "cloud"
tier = "strong"
context_window = 200000               # optional; for context breakdowns (local models report theirs)

[routing]
prefer = "local"                      # cheapest (default) | local | cloud

[[routing.rules]]                     # first matching rule wins
when_tag = "code"                     # optional; also when_tier = "strong"
location = "local"                    # optional restriction
use = ["qwen-local"]                  # optional ids, tried in order
```

A subagent's model is chosen as follows:

1. A model the agent named (`spawn_agent`'s `model`, by id or spec), if it is in the table.
2. The first rule whose `when_tier` (exact) and `when_tag` match the task: its `use` ids are
   tried in order (tier is not checked for an explicit choice; the location restriction is),
   then its `location` restricts the remaining choice.
3. The suitable rows (at or above the requested tier, default `standard`, and in the requested
   or rule location) ranked by `prefer`, then by input plus output price, with unpriced rows
   counted as free.
4. The session's own model.

Agents pass `tier`, `location`, `tags`, and `model` to `spawn_agent`. Ids must be unique and
rules may only name ids in the table; the daemon refuses to start otherwise. Rows whose
endpoint is unknown or whose key is missing are skipped with a warning. `models.list` returns
the table, and the desktop app shows it in a searchable, sortable Models panel.

**Implemented (phase 4):** rows with spec `llamacpp:<id>` (location must be `local`) are
placed by the memory planner. A local row is considered only while it can run: loaded, fitting
beside the loaded models, or fitting once loaded models nobody has a slot on are swapped out
(stage 2d; not while the swap limit is reached). In each ranking step a loaded model beats one
that would have to be loaded, which beats one that needs a swap, and nothing in use is evicted,
so when memory is short the choice falls to the next suitable row, typically hosted. A row that
needs a swap carries the expected seconds (the model's last measured load, or its size at
2 GiB/s), for stage 2e's comparison of the swap with a hosted model's price per task class; until
then a local row that can swap wins on price like any local row.
A session's own model can also be `llamacpp:<id>`. The cost scoring with load latency and
queue delay, capability filters, and escalation below are later work.

For each task, the router builds the candidate set and scores it:

1. **Filter:** capabilities, context length, quality tier ≥ required, policy allow-list,
   endpoint health, and budget.
2. **Placement for local candidates:**
   1. Already loaded with a free slot → cost of reuse ≈ 0.
   2. Fits in free memory → load it.
   3. Doesn't fit → **do not evict** a model another agent is using. Prefer an already-loaded
      model that meets requirements (add a slot), otherwise a hosted endpoint.
3. **Score:** expected cost (estimated tokens × price), plus load latency, queue delay, and a
   quality margin penalty. Lowest score wins; ties go to local.
4. **Escalate on failure:** if a model fails a task (tool-call errors, refusals, budget burn
   without progress), the router may retry once on the next tier up.

Users can override per session, per agent role, or per task with explicit rules:

```toml
[[routing.rules]]
match = { role = "reviewer" }
prefer = ["openrouter:qwen/qwen3-235b-a22b"]

[[routing.rules]]
match = { tools = ["exec"] }
require = { source = "local" }      # never send code-execution tasks off-machine
```

## Cost accounting

Every call records prompt, cached, and completion tokens, latency, time-to-first-token,
tokens/sec, and cost. The ledger aggregates per call, agent, task, session, model, and day,
and is exposed through `telemetry.*` ([06](06-extensibility.md#telemetry-api)).
