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
models panel list what was found and whether each fits. Watching folders for changes, network
and SSH paths, Hugging Face directories, and Hub downloads are later work.

- Users configure any number of **model folders** (local paths, network shares, or paths on an
  SSH host). The daemon scans them recursively and watches for changes.
- Recognised: GGUF files (including split `-00001-of-0000N` sets and `mmproj` projectors) for
  llama.cpp; Hugging Face directories (`config.json` + safetensors) for vLLM; diffusion model
  folders for the media sidecar.
- GGUF headers are parsed for architecture, layer count, head counts, embedding size, context
  length, and quant type — enough to compute the memory footprint without loading the model.
- Optional Hugging Face Hub search and download into a chosen folder.

## Runtimes

**Implemented (phase 4):** `imperium runtime install [--pack <id>]` downloads the pack for
this machine (or `runtime.llamacpp.backend`) from the `runtime-llama.cpp-<ref>` release,
checks it against the release's `SHA256SUMS`, and unpacks it under
`<data>/runtimes/llama.cpp/<ref>/`. `runtime.llamacpp.custom_path` points at your own
`llama-server` instead. The daemon starts one `llama-server` per loaded model on a free
loopback port with `--parallel` slots, waits for `/health`, logs its output to
`<data>/logs/llama-<id>.log`, and stops it when it has been idle for `idle_unload_s` or the
daemon exits. An agent holds a slot for its whole turn; a model whose slots are all busy
queues further turns until one frees. vLLM is later work.

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

- **llama.cpp:** runtime packs per backend (CPU, CUDA, Vulkan, ROCm, Metal) are built by CI
  ([11](11-ci-cd.md#runtime-packs)) and downloaded on demand, verified by SHA-256. Users can
  point at their own `llama-server` build instead.
- **vLLM:** installed into a managed `uv` environment on Linux/WSL2, or run from the official
  Docker image, or used on a remote host. Imperium never bundles vLLM.
- Each loaded model is a supervised process with a port, health check, idle timeout, and
  memory reservation. Multiple slots (`--parallel`) let one loaded model serve several
  subagents concurrently.

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
placed by the memory planner. A local row is considered only while it can run: loaded, or
fitting in the memory not held by models in use (idle models are unloaded to make room). In
each ranking step a loaded model beats one that would have to be loaded, and nothing in use is
evicted, so when memory is short the choice falls to the next suitable row, typically hosted.
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
