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

**Implemented (layouts):** the scanner also understands the layouts models are usually left in.

- **LM Studio and the download folder:** `<publisher>/<repo>/<file>.gguf`. The two folders above
  a file name its repository (`lmstudio-community/Qwen3.5-9B-GGUF`), shown beside the id.
- **The Hugging Face cache:** a `models--<org>--<name>` folder (such as
  `hub/models--unsloth--Qwen3-4B-Instruct-2507-GGUF`) is the repository `<org>/<name>`; only the
  snapshot `refs/main` names is scanned (every snapshot when there is no `refs/main`), and the
  cache's `blobs` and `.no_exist` placeholders are skipped. A safetensors model in a snapshot is
  named after its repository (`Qwen3-0.6B`), not its revision hash.
- **Projectors:** with one `mmproj` file in a folder it goes with every model there; with
  several, each model gets the one whose name matches its own best.
- **Left out, with the reason:** hidden folders (`.locks`, `.cache`), diffusers pipelines (a
  folder with `model_index.json`, whose `text_encoder` and `vae` folders are not chat models),
  and GGUF files that are not language models (architectures such as `flux` or `qwen_image`,
  or a header with neither layers nor context length). These are listed as skipped, not as
  models that do not fit.
- A vLLM model on a machine that cannot run vLLM (anything but Linux and WSL2) says so
  (`problem` in `models.local`) instead of reading *does not fit*.

## Setting up models from the app

**Implemented:** everything needed to use models already on disk is available over the protocol,
the desktop's Local models panel and Settings → Models, where model folders are added
([07](07-desktop-ui.md#setting-up-models-implemented)), and the CLI. Each change is written to the user's `config.toml` through the settings table and applied to
the running daemon at once; the configuration layers kept for later model-settings saves are
read again, so such a save never brings an old value back.

| Method | CLI | Does |
| --- | --- | --- |
| `models.add_folder` `{path}` | `imperium models folders add <path>` | Adds an absolute folder to `models.folders` (once), scans at once (starting the local runtime if it was not running), returns `models.local` |
| `models.remove_folder` `{path}` | `imperium models folders remove <path>` | Removes it and scans again; nothing in the folder is touched |
| `models.local` | `imperium models folders`, `imperium models local` | Also reports `configured_folders` (removable), `default_model`, and per model `repo`, `mmproj`, and `problem` |
| `models.use` `{model}` | `imperium models use <id>` | Sets `models.default` from a local model id (`llamacpp:<id>`), a model-table id (its spec), or a spec |
| `models.test` `{model, prompt?}` | `imperium models test <id>` | Loads the model if needed (as any session would, through the supervisor) and sends one short prompt; returns the reply, completion tokens, seconds, tokens per second (llama.cpp's own measurement when it reports one), and the load wait, or why it failed. Works for hosted models too |
| `models.bench` `{model?, cold?}`, `models.bench_results` `{model?}`, `models.bench_cancel` `{run_id}` | `imperium models bench [<id>] [--results] [--warm]` | The [local model benchmark](#local-model-benchmark-implemented): starts a run (one run at a time), lists saved results and this session's runs, stops a run |
| `models.add_endpoint` `{id, base_url, api_key?, model?, make_default}` | — | Writes `[endpoints.<id>]` with the key as a keychain reference (`<id>-api-key`), optionally making `<id>:<model>` the default |
| `runtime.status` | `imperium runtime status` | See [runtime readiness](#runtime-readiness) |
| `runtime.set_server` `{path}` | `imperium runtime use [<path>]` | Sets (or with null clears) `runtime.llamacpp.custom_path`; models loaded from then on use it |
| `runtime.install` | `imperium runtime install` | Installs llama.cpp's own build for this machine from its GitHub release in the background with `runtime.install_progress` notifications, falling back to the next build when one does not run ([runtimes](#runtimes)); new loads use it unless a custom server is set |
| `runtime.update_external` | — | Runs the package manager's upgrade of the `llama-server` in use (winget, scoop, Chocolatey, Homebrew; the llama.cpp package only) in the background with `runtime.update_progress` notifications; refused while a model is loaded from it ([updates](#llamacpp-updates)) |

`models.default`, `models.folders`, and `runtime.llamacpp.custom_path` set with `config.set`
(Settings) apply at once in the same way.

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

## Model download suggestions

**Implemented:** when a session needs a model and the best one available is poor for the task,
the daemon offers to download a suitable local model from Hugging Face. Nothing is downloaded
without the user's consent, and `models.suggest_downloads = false`
([configuration](10-configuration.md#implemented-settings)) turns it off.

**When.** The daemon watches each session's root agent when a turn starts (with the user's
message) and after each model call (with its prompt size), and checks, in order
(`imperium_local::suggest::detect`):

1. Nothing is suggested unless local models are wanted: `routing.prefer = "local"`, or the
   session's own model is local (`llamacpp:` or `vllm:`).
2. No local model is installed: a general model (`need = "local"`).
3. The message names an image file (a word ending in `.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`,
   or `.bmp`) and no local model has a vision projector: a vision model (`vision`).
4. The prompt is longer than every local model's context length (from its GGUF header): a
   model that reads that much (`context`).
5. `routing.prefer = "local"` but routing an ordinary task (default tier, no tags) picks a cloud
   model, because no local row in the model table can run: a general model (`local`).

Agents can also ask with the `suggest_local_model` tool (`task`, `needs` from `chat`, `code`,
`vision`, `tools`, `context`, and `context_tokens` for `context`); the most specific need named
is used (vision, code, tools, context, then chat). The tool only proposes: it never asks for
permission, and the user still decides. Tool-calling and code needs come only from the tool,
since the model table records no capabilities yet.

At most one suggestion is made per need per session, whether it was accepted, dismissed, or
nothing fitted (a failed Hub listing also counts, so the Hub is not asked on every turn).

**What.** Candidates are the `[[models.suggestions]]` rows whose `capabilities` cover the need
(`chat` for `local` and `context`, which also needs `context` at least the prompt size). Each
candidate's file listing is read from the Hub (the same `/api/models/<repo>/tree/main` listing
as `models.hf_files`; no other network use), and every GGUF quantisation is sized against this
machine as `models.hf_files` does (`gpu`, `partial`, `cpu`, `none`), counting a vision
candidate's projector (the F16 one when there are several). The best is chosen by fit (GPU,
then partial offload, then CPU; never one that does not fit), then size class (larger first),
then the candidate's place in the list, then quantisation preference (`Q4_K_M`, `Q5_K_M`,
`Q6_K`, `Q8_0`, `Q4_K_S`, `IQ4_XS`, ... then others), then smaller size.

**Asking.** The session gets a `model_suggestion` event (`id`, `need`, `repo`, `quant`,
`model` it will be as `llamacpp:<id>`, `size_bytes`, `fit`, `reason`, `mmproj`). Clients answer
with `session.suggestion_respond` (`session_id`, `suggestion_id`, `action`):

- `download` starts the download as `models.download` would (same job, progress
  notifications, and rescan when done) and returns the job. A `model_suggestion_update` follows
  with `state = "downloading"` and the `job_id`, then `ready` with the `model` once the folders
  were rescanned (switch with `session.control` `set_model`), or `failed` / `cancelled` with a
  `message`.
- `dismiss` (not now): `dismissed`; that need is not suggested again in the session.
- `never` (don't ask again): turns `models.suggest_downloads` off at once and writes `false` to
  the user config file, so it stays off after restarts; `disabled`. This is a global switch,
  not a per-need dismissal list; turn it on again in Settings.

The suggestion events are journalled with the session, but which suggestions wait for an answer
is kept in memory, so after a daemon restart an old suggestion can no longer be answered. The
desktop shows a card ([07](07-desktop-ui.md)); `imperium chat` prints the suggestion and reads
`/download`, `/dismiss`, or `/never`.

## Runtimes

**Implemented:** `imperium runtime install [--pack <id>]` (and `runtime.install`, *Install
llama.cpp* in the app) installs llama.cpp's own release build from
[ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp/releases) at the ref pinned in
`runtimes/llama-cpp.env`:

- **Which build.** For `runtime.llamacpp.backend = "auto"` the builds are tried best first: on
  an NVIDIA GPU CUDA 13 (driver 580 or newer), then CUDA 12 (driver 525 or newer, x64), then
  Vulkan, then the CPU build; Metal on Apple Silicon; the CPU build elsewhere. A configured
  backend (or `--pack windows-x64-vulkan`) installs only that build. Builds are named
  `<os>-<arch>-<backend>` and matched to the release's assets, such as
  `llama-<ref>-bin-win-cuda-13.5-x64.zip`; a CUDA build also gets its CUDA runtime libraries
  (`cudart-llama-bin-win-cuda-13.5-x64.zip`), unpacked beside `llama-server`.
- **Download and verification.** The release is read from the GitHub API, and each asset is
  downloaded through its API URL with `Accept: application/octet-stream` (the
  `github.com/.../releases/download` links have returned 504 for hours at a time). Every asset
  is checked against the SHA-256 `digest` GitHub publishes for it; an asset without one, or
  with a different hash, is refused and nothing is replaced.
- **Fallback.** After unpacking, the build is checked as in [runtime readiness](#runtime-readiness).
  A build that does not start is removed and the next one tried; one that starts but does not
  see the GPU is kept only if no later build does better. A failed download stops the install.
- **Where.** `<data>/runtimes/llama.cpp/<ref>/imperium-runtime-llama.cpp-<build>/bin/`, with an
  `imperium-runtime.json` recording the assets and digests. The first installed build of the
  list above is used.

**Which `llama-server` is used**, in order: `runtime.llamacpp.custom_path`; an installed build
(of the pinned release, else of the newest earlier release still installed, until the pinned
one is); otherwise, while `runtime.llamacpp.find_existing` is on (the default), a
`llama-server` already on this machine that starts and whose `--help` lists every flag Imperium
passes (`--kv-unified-per-slot`, `--no-cache-idle-slots`, ...). The daemon looks for one in the
background when it starts and whenever the server is changed; `runtime.status` reports it as
source `detected` and the app says so on the runtime card. Installing a build replaces it.

- **Where it looks**, one folder level deep at most (no disk is scanned): `PATH`; Unsloth's
  `~/.unsloth/llama.cpp` and a `~/llama.cpp` checkout (`build/bin/Release`, `build/bin`, `bin`);
  LM Studio's bundled backends (`~/.lmstudio/extensions/backends/llama.cpp-*/`, which ship a
  `llama-server`); on Windows winget (`%LOCALAPPDATA%\Microsoft\WinGet\Links` and
  `Packages\ggml.llamacpp*`), scoop (`~/scoop/apps/llama*/current`, its shims, and the global
  scoop), Chocolatey's `bin`, and `%LOCALAPPDATA%\llama.cpp`; on macOS `/opt/homebrew/bin`,
  `/usr/local/bin`, and `~/.local/bin`; on Linux `/usr/local/bin`, `/usr/bin`, Linuxbrew,
  `~/.local/bin`, and `/opt/llama.cpp`. Folders with many builds (LM Studio's) are read newest
  first, eight at most.
- **Unverified downloads are listed, never run.** A `llama.cpp*` checkout or unpacked release
  (`llama-b<N>-bin-*`) directly in the home or Downloads folder is only offered as a candidate
  (the runtime card's "Use detected…" list, `imperium runtime candidates`): it is not started
  with `--version`, never chosen by itself, and never preferred over an installed build. Using
  one (the card's "Use this", `imperium runtime use <path>`, or
  `imperium config set runtime.llamacpp.custom_path <path>`) sets `runtime.llamacpp.custom_path`,
  and only then is it run.
- **Which one.** Of those that start and take the flags: one that sees the GPU through the
  backend that suits this machine (CUDA on an NVIDIA GPU, Metal on Apple Silicon), then one that
  sees another GPU, then one that could not say, then a CPU-only one; among equals the newest
  build (the number in `--version`), then the first found.

**Installed when first needed.** With `runtime.llamacpp.auto_install` on (the default) and no
custom path, the daemon installs llama.cpp's build for this machine by itself (as
`runtime.install` would, with the same progress notifications, verification, and fallback)
the first time a runtime is needed: when a llama.cpp model is about to load, or when the Local
models panel opens (`runtime.status`); never when the app starts. It does so when no usable
`llama-server` is installed or found (the load waits for the install and then goes ahead), and
in the background when the one found runs on the CPU only although this machine has a GPU a
build supports (the GPU build is used from the next load; with `backend = "cpu"` it is not).
An automatic install is tried once per daemon run; after a failure the load fails with *No
llama.cpp runtime yet — open Local models → Runtime to install one or choose an existing
llama-server (or run `imperium runtime install`)* and the reason, and *Install official build*
tries again. With `auto_install` off, a missing runtime fails the load with that message, and
the session model selector warns before a llama.cpp model is chosen.

### llama.cpp updates

llama.cpp has no updater of its own, so llama.cpp updates with Imperium:

- **Imperium's build.** Builds live under the release ref Imperium pins, so when an update
  pins a newer release, the earlier build stays in use and the pinned one is installed (as
  above) the next time a local model is needed. Models already loaded keep the earlier build;
  new loads use the new one. The new build must start and list every flag Imperium passes, or
  it is removed and the earlier build stays (a rollback). One earlier release is kept until a
  model has loaded with the new build; then it is deleted (left for later while a model still
  holds its files).
- **Package-manager installs** (winget `ggml.llamacpp`, scoop, Chocolatey, Homebrew): when the
  build number in `llama-server --version` is older than the pinned release's (or the server
  lacks a flag Imperium passes), `runtime.status` offers an update (`update`, action
  `package_manager`, with the command), as set by `runtime.llamacpp.update_external`: `off`,
  `ask` (the default: an *Update* button), or `auto` (run by itself, in the background, at the
  first model load of a daemon run). `runtime.update_external` runs `winget upgrade --id
  ggml.llamacpp --exact`, `scoop update <app>`, `choco upgrade llama.cpp -y`, or `brew upgrade
  llama.cpp`, only while no model is loaded from that server, and reports the output.
- **Checkouts and unpacked folders** (`~/.unsloth/llama.cpp`, `~/llama.cpp`, LM Studio's
  backends, anything else not owned by a package manager) are never modified. When one is older
  than the pinned release or lacks a flag, `runtime.status` offers *Use Imperium's build
  instead* (action `use_managed`), which installs the official build; an installed build is
  preferred over a found one from then on.

The daemon starts one `llama-server` per loaded model on a free
loopback port with `--parallel` slots over one shared KV pool, waits for `/health`, logs its output to
`<data>/logs/llama-<id>.log`, and stops it when it has been idle for `idle_unload_s` or the
daemon exits. vLLM is later work.

### Runtime readiness

**Implemented:** `runtime.status` runs the `llama-server` in use with `--version` and
`--list-devices` (neither loads a model) and reads the result with the hardware probe: whether
the server runs, its version, the devices it sees (`CUDA0: NVIDIA GeForce RTX 5080 ...`), whether
it sees a GPU, a one-line summary, and, when something is wrong, the problem in words with what
to do:

- no file at the path: *No llama.cpp runtime yet*; install llama.cpp's build for this machine,
  or choose your own;
- it cannot start because a library is missing (Windows `STATUS_DLL_NOT_FOUND`, or "error while
  loading shared libraries"): for a CUDA build, the CUDA runtime is missing;
- it runs but lists no GPU on a machine with one: for a CUDA build (a `ggml-cuda` library beside
  it), *llama-server can't see your GPU — the CUDA runtime is missing or the NVIDIA driver is too
  old*; otherwise it is a CPU-only build and the GPU build is suggested;
- an old build without `--list-devices` runs, and whether it uses the GPU shows once a model
  loads.

It also lists `llama-server` executables already on the machine (on `PATH`, and in
`~/.unsloth/llama.cpp` and `~/llama.cpp` build folders) so one can be picked with
`runtime.set_server`. Choosing a server or installing a build changes the executable for models
loaded from then on, without a restart (a build installed by the CLI while the daemon runs is
picked up at the next `runtime.status`, which the CLI asks for); a model already loaded keeps
its server until it unloads.

### When models load

**Implemented:** a local model is loaded only when something actually calls it. Choosing it
never does:

- **Selecting a model for a session** (the new-session composer, `session.create` with a model,
  the session's model picker → `session.control` `set_model`) only checks it without loading:
  the spec resolves, and a local model is among those the runtime found in the model folders
  (otherwise `-32004` at once). The switch is journalled and the answer is an estimate
  ([03](03-agent-engine.md#switching-models-and-compaction)) against the context the model's
  launch settings give a session. No server starts and no slot is leased.
- **The first message** sent to the session loads it: the turn's first step measures the
  conversation with the model's tokenizer and window (compacting it if it does not fit), then
  makes the call, through the supervisor as any call does. A model that cannot load (the
  runtime missing, not enough memory, the server failing to start) fails that turn with the
  reason; the session keeps working, and another model can be picked.
- **Listings and status never load:** `models.list`, `models.local`, `models.config`, the
  session footer's model select, its context meter (which shows the breakdowns turns already
  produced), onboarding's model lists, `runtime.status`, and the resource monitor only read
  what the supervisor knows.
- **Loading on purpose:** `models.test` (the *Test* button in onboarding and the local models
  panel) loads the model to send it one prompt, and `session.control` `compact` asks the
  session's model for a summary at once.

**Implemented (stage 2c):** one loaded local model serves many sessions at once. The daemon has
one supervisor, loaded models are keyed by id, and every session that asks for `llamacpp:<id>`
reuses the same `llama-server`. What changed is how long a slot is held:

- **A lease per model call.** An agent takes a slot immediately before each streamed request and
  releases it when the request ends, whether it succeeded or failed. Tool calls, `await_tasks`,
  waiting for the user, and usage-limit pauses hold no slot, so any number of roots asleep in
  `await_tasks` leave the slots to the sessions that are working, even on a model with a single
  slot. The model is loaded on the first call, and stays loaded within `idle_unload_s` of the last
  call that used it. Hosted endpoints are unaffected.
- **A token reservation.** A llama.cpp lease also reserves tokens from the model's KV pool, which
  is budgeted in tokens. The agent estimates the call's prompt: after its first call, the last
  call's exact prompt and completion tokens plus the messages added since (estimated with a 1.25
  margin and 8 tokens of chat-template overhead each); on its first call, every message and the
  tool specs the same way, plus 64 tokens for the template's frame. The supervisor reserves
  `min(prompt + max_output_tokens, context)` tokens (the whole `context` when `max_output_tokens`
  is 0, when the prompt is unknown, or on a retry) before the call, and the lease tells the agent
  to send the rest as `max_tokens`, so the server cannot generate past the reservation. When the
  prompt estimate already fills the context there is no room to cap, and the request is sent with
  no `max_tokens`, so the server's own over-context error surfaces. A reply that stops with
  `finish_reason = "length"` (or fails with "Context size has been exceeded.") is retried once,
  without a cap and with the whole context reserved, after a `Notice`; only the retry's reply is
  kept in the conversation. A call that was sent uncapped is not retried, since the retry would be
  the same request. A prompt larger than its estimate is logged as a warning, so how often the
  pool is overcommitted is visible. vLLM manages its own paged KV cache, so a vLLM lease reserves no
  tokens and carries no `max_tokens`.
- **A queue.** Calls that find every slot busy, or too few tokens left in the pool, wait
  first-come, first-served, for at most `max_wait_s` (stage 2d; default 600 s), after which the
  call fails. A call is never rejected for lack of slots or pool: it queues in Imperium, per
  model. `models.local`,
  `imperium models local`, and the desktop's Local models panel show each loaded model's slots in
  use and `queued` (calls waiting), and each `model_call_completed` event carries `queue_wait_ms`,
  the time that call waited for a slot or for room to load its model, not counting the load, a
  swap-out, or a slot restore (`duration_ms` and the time to first token start after it; absent
  for hosted models).
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
  its turn, and marked waiting while it is in `await_tasks`, while a permission request waits for
  your answer, and while a usage-limit pause runs. A loaded model is *free* when none of the
  agents that took a slot on it since it was loaded is mid-turn (all are waiting or done), and
  *claimed* otherwise. A model claimed by an agent of another session than the one asking is
  treated like one in use: **a load for one session never evicts, restarts or interrupts a model
  another session is using**, and neither does the idle timeout or a settings change; the load
  waits (up to `max_wait_s`) and goes ahead once that model's agents finish their turns or sleep.
  A claim exists from the agent's first call on the loaded model, so a load may swap a model out
  between a turn's start and its first call (or after a reload and before the returning agent's
  first call): nothing is in flight then, and the agent's first call reloads it with its saved
  slot. A model claimed only by the asking session's own agents is waited for and may be swapped
  out as described below. An orchestrator asleep in `await_tasks` may be swapped out for any
  session's load, as before: two orchestrators each awaiting a subagent that needs the memory the
  other's model holds would otherwise wait for each other. The wait for a blocking model is
  bounded by the remaining turns of the agents already in its batch (each at most
  `agents.max_steps` calls, each call bounded by its reservation), because newcomers do not join
  while a load waits.
- **When.** A model that fits beside the loaded models loads beside them. Free models are
  swapped out, least recently used first and as few as needed, only when the newcomer does not
  fit beside them, or gets more than `swap_gain` times the GPU memory once they are gone than it
  would beside them. The default, 2.0, keeps a subagent model from running mostly from RAM next
  to a sleeping orchestrator while not swapping for a small gain; 1.0 swaps whenever the
  newcomer would get more GPU memory alone, so subagents run fully on the GPU (as G14 measures);
  values below 1.0 are rejected. If
  only a claimed model is in the way, it waits for that model's agent to start waiting or finish
  its turn (an orchestrator that spawns a subagent usually starts waiting moments later), for up
  to `min_residency_s`. After that it loads beside the claimed model if it fits at all, and swaps
  the claimed model out only when nothing else fits and that model has been loaded at least
  `min_residency_s`.
- **Bounded waits.** A call whose model is in the way of models in use waits for them and then
  loads, including a model swapped out between one agent's calls, instead of failing. A call
  that needs a swap while the `max_swaps_per_min` limit is spent waits for the minute to move on
  (an orchestrator coming back to its own model keeps its turn). The waits are bounded: a model
  too big for the whole budget fails at once; with `max_swaps_per_min = 0` (swapping off), a
  model that does not fit beside the loaded ones fails at once with the same not-enough-memory
  error; and any call, whether waiting for room or for a slot, fails after `max_wait_s` (default
  600 s) with an error naming the setting.
- **Swap out with state.** Before a model is unloaded, each slot holding the prompt of an agent
  that can still come back (mid-turn, or waiting for its subagents) is saved with `POST /slots/{id}?action=save` into `<data>/swap`, which `llama-server` is started
  with as `--slot-save-path`. Files are keyed by the agent and by the model: its id, the file's
  path, size, and modification time (not a hash of the contents, which would read gigabytes on
  every load), the context, the pool size, and the K and V cache types, so a stale file is never restored. When
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
  process has exited before the next model starts. A launch that fails quickly (the process
  exits or does not start) is tried once more; one that ran out the 600 s load timeout is not.
  At most `max_swaps_per_min` swaps happen per minute; beyond that, calls that need a swap wait
  (see bounded waits), and the router treats such models as unavailable so new tasks go to the
  next suitable row.
- **The prompt cache yields.** When a model is swapped out only because another model's
  `--cache-ram` took the RAM it needs, the newcomer's cache is sized to leave room for it to come
  back without its cache, so after one restart both stay loaded with smaller caches instead of
  taking turns.
- **Visibility.** Every client receives `model.residency` for every move with its reason (see
  [the residency controller](#residency-controller-implemented)), and `model.swapped_out` and
  `model.swapped_in` notifications
  (`ModelSwap`: `model_id`, `seconds`, `bytes`, `slots`, and a `reason`: `room`, `idle`, `config`,
  `grow`, `promote` or `request` (`models.unload`) out, `swap` or `restore` in). Out counts the slot bytes written and the time to save and stop; in
  counts the memory taken and the time to start, and `slots` is how many saved slots wait for
  their agents.

- **Background work ranks below yours (stage 2g).** Sessions a schedule or a project agent started
  ([14](14-projects-and-automation.md#quiet-periods)) make their local calls as `background`; yours
  are `interactive`. A background call never takes a place in a model's slot queue ahead of yours:
  it waits outside the queue until no call of yours is queued for that model, and background calls
  among themselves keep arrival order. Your call for a model that is not loaded loads it before a
  background call that arrived earlier, whatever the model. A background call never swaps out a
  model one of your calls used within `load.interactive_hold_min` (default 5 minutes) or holds now,
  and it makes no progress at all while the daemon's load view reads `saturated`. Waiting for these
  reasons does not count against `max_wait_s`, which keeps bounding every other wait. A model only
  background calls used is evicted like any other.

Not done in stage 2d: per-session ceilings on slots and on `--cache-ram` (llama.cpp manages its
prompt cache itself); shrinking the `--cache-ram` of a model that is in use or claimed, which
needs a restart of its server (only free models give their cache up, by being swapped out); the
longest wait per session; swap events in the desktop's Models panel; and the measured run on a
16 GB GPU.

**One KV pool, slots, and no global cap.** Each llama.cpp model is started with
`--ctx-size <pool_tokens> --kv-unified --parallel <slots> --no-cache-idle-slots`, plus
`--kv-unified-per-slot <context>` when `context < pool_tokens`. `context` is what one session may
use; `pool_tokens` is the KV cache all slots share, and the memory planner counts the KV cache
once, for the pool (`kv_bytes_per_token × pool_tokens`), whatever the slot count, so slots no
longer multiply memory. `--no-cache-idle-slots` keeps an idle slot's cells until the pool runs
short, so the swap-out save still finds the slots of agents asleep in `await_tasks`; under pool
pressure llama-server purges idle slots anyway, and an agent whose slot was purged reads its
prompt again on return (the pinned-slot switch above is a separate path and still works). There
is **no global cap** on sessions using local models: the per-model `slots` (default the larger of
16 and `runtime.llamacpp.parallel`; also written `max_slots`) is how many calls one server decodes at once, not an admission
limit, and calls beyond it queue in Imperium, first come, first served. Every setting can be set
per model, in
`[runtime.llamacpp.models."<id>"]` or from the desktop's model settings dialog
([10](10-configuration.md#layers)): context, pool, slots, K and V cache types, flash attention, a
cap on GPU layers, the wait cap, `max_output_tokens`, and extra arguments. Changing the settings
of a loaded model restarts it once it is free (swap reason `config`); a newcomer waits for the new
shape while the model's batch carries on. Changing settings never waits for a load or an unload:
the model is marked stale and a background task restarts it when no call lease, queued call or
claim of any session remains. `max_wait_s` and `max_output_tokens` only shape waits and
reservations, so they apply to a loaded model without a restart. A load starts its server with the
settings in effect when it was planned; if they change while it loads, the new server serves the
call that loaded it and then restarts the same way. A `config` restart saves only the slots its
new server can restore (same context, cache types and, when fixed, pool); the others are read
again.

**Implemented: the most context each agent can use, reserved as it is used.** llama.cpp reserves
the whole KV cache (`--ctx-size`) when the server starts and cannot grow it in place. Reserved but
empty cells cost VRAM capacity, not memory bandwidth (attention reads only the filled cells), so
what Imperium keeps small is the reservation, while each agent may use as much context as the
model and the machine allow (`imperium-local/src/context.rs`):

- **The ceiling** (`context`, what one agent may use and what compaction is planned against) is
  the least of what the model was trained for (its header's `context_length`), the configured
  limit, and the largest pool that fits on an empty machine beside the weights. While every layer
  fits on the GPU, only pools that keep them all there count, so context never pushes layers off
  the GPU; a model already split between GPU and RAM may use what fits in both. The fit is
  estimated with the planner's KV size per token at the model's cache types, against the budget
  measured when the daemon started. `runtime.llamacpp.context` and the per-model `context` are
  `"auto"` (the default: no limit of their own) or a number of tokens. A model whose header does
  not say what it was trained for gets an `auto` ceiling of 32,768 tokens. The ceiling never
  drops below a working context of 8,192 tokens (or the configured context, if smaller).
- **The pool** starts at 16,384 tokens (with a fixed context, `context × parallel` if that is
  smaller; never more than fits), or at what the call that loads the model needs: its prompt estimate plus
  `max_output_tokens`. One agent may use all of it, up to the ceiling: `--kv-unified-per-slot` is
  only passed when the ceiling is below the pool. When a call's prompt and reply no longer fit,
  the model is marked to grow: no new call takes a slot on it, and as soon as the calls running on
  it are done it restarts (swap reason `grow`) with the pool doubled until the call fits, rounded
  up to 1,024 tokens and capped at the ceiling. Unlike a `config` restart this does not wait for
  agents to finish their turns, since the agent that needs the room is mid-turn itself: the slots
  of agents that can come back are saved and restored into the larger pool (the slot key leaves
  out a pool that grows), and a slot that cannot be restored is read again. A model that is
  unloaded (idle, swapped out for room, or `models.unload`) forgets its grown pool and starts
  small again next time, so the memory goes back once the sessions that needed it are done. A
  fixed `pool_tokens` never grows and caps the ceiling.
- **Weights on the GPU first (fix: keep weights on GPU by shrinking the KV pool instead of
  offloading layers).** On a discrete GPU the KV memory gives way before the layers do: when the
  weights and the runtime overhead fit in the VRAM free at start, the ceiling and the pool are cut
  to the tokens left beside them, never below 8,192 tokens (or the context, if smaller). This also
  applies to a pool fixed with `pool_tokens`: one that does not fit is cut to what fits (logged,
  `pool_clamped` in `models.local` and `models.config`, a warning in the model settings dialog),
  and `--ctx-size` and `--kv-unified-per-slot` use the cut pool, rather than offloading most
  layers to the CPU as before (a pinned `8 × 40960` pool of a 4B model on a card with 8 GB free
  used to start with 5 of 36 layers on the GPU; it now starts with all 36 and about 133k
  tokens). Only when the weights and that minimum do not fit are layers split between GPU and
  RAM as before; `max_gpu_layers` and a CPU-lane pin are respected (the pool then fits what
  those placements leave). The Local models panel shows the layers on the GPU (*36/36 layers on
  GPU*) and says when the rest run on the CPU.
- **Slots are not reservations.** With `--kv-unified` every sequence takes cells from the one
  pool as it needs them, so `--parallel` itself costs almost nothing (a little per-slot state).
  Models start with a high slot ceiling (`slots`, also written `max_slots`, default the larger of
  16 and `parallel`), the per-slot cap (`--kv-unified-per-slot`) is the context ceiling, and the
  pool is never sized as slots × context: it follows demand. The most it may grow to
  (`pool_max`) is what fits beside the weights, at most every slot at the full context.
- **Growing with the work.** Besides one call that does not fit, a call that fits but finds the
  pool filled by the others' reservations (the high-water mark: it would have to wait) grows the
  pool, by doubling, towards `pool_max`, the same way: between calls, slots saved and restored.
  So the pool follows how many agents work at once. It shrinks back when the model is unloaded
  (after `idle_unload_s`, or swapped out for room), which is the hysteresis: it grows at once and
  shrinks only when nothing has used it for a while. A full pool that cannot grow makes new calls
  wait their turn; llama-server itself purges idle slots' cells when the pool runs short.
- **Several models in parallel.** A model loaded beside others gets its weights on the GPU first
  and the KV pool from the VRAM left (the arbiter places it with the memory the loaded models
  hold already deducted): when its pool would push layers off the GPU, the pool is cut to fit,
  down to a working context, rather than offloading layers, and the model is marked squeezed (its
  agents' ceiling is then its pool, and it does not grow until next loaded). Only when its
  weights and a working pool do not fit beside the loaded models do the usual choices apply:
  swapping a free model out, waiting, or partial offload as a last resort. The arbiter, use
  claims and KV pool code are the same; nothing runs beside them.
- **Why a call waits.** While a model call waits for a local model the agent emits
  `local_model_waiting` (`agent_id`, `model`, `reason`) each time the reason changes: `loading`,
  `growing_pool`, `kv_room`, `slot`, `other_model` (another model's calls come first, or it
  must unload to make room), `making_room` (models in the way finish before they give way, so
  the call does not run on the CPU), `saving_context` (the contexts on a model swapped out for
  it are saved), or `restoring_context` (its own context, saved while it waited, comes back).
  The CLI prints it; the desktop does not show it yet.
- **What the lists show.** `models.local` rows carry the loader's own estimate for each model
  under its settings (the same placer and pool sizing, beside the models loaded now, against the
  budget now, which the residency controller refreshes): layers on the GPU (`gpu_layers` of `layers`), `vram_bytes`,
  `ram_bytes`, the context ceiling and the starting pool; a loaded model reports its live
  values instead. They are recomputed on every listing, so a saved setting shows at once.
- **Why not a llama.cpp fork.** llama.cpp allocates the KV cache when it creates the context,
  and the unified pool already hands out cells per sequence, so dynamic slots need no engine
  change; only growing the cache in place would, which means maintaining a fork against a fast
  moving upstream. Restarting between calls with the slots saved and restored gets the same
  result at the cost of a server start per doubling; an in-place resize remains a possible later
  option.
- **Memory savers.** The K and V cache types can be set per model (`cache_type_k`,
  `cache_type_v`; `q8_0` about halves the KV cache of `f16`, and a quantised V cache needs flash
  attention on or `auto`). The ceiling is computed at those types, so `q8_0` roughly doubles the
  context that fits.

A growth restart reads the prompts of agents whose slots could not be restored again, and costs a
server start; doubling keeps restarts to a few per session. Not measured on hardware yet: the
restore of a slot into a larger pool, and the compute buffers llama.cpp allocates for large
contexts (the planner still counts a flat runtime overhead).

**Implemented: KV residency and the GPU rule.** What sits on the GPU follows who is working, at
the grain of one agent's slot as well as one model:

- **The GPU rule.** No agent runs on the CPU alone while no other agent is generating, so every
  agent gets a turn on the GPU. A call that would run on the CPU, or split across GPU and RAM,
  beside the loaded models while none of them has a call running makes everything not
  generating give way: idle models, models of agents mid-turn between calls, and models of
  agents waiting. Their slots are saved first. `min_residency_s` and the swaps per minute do
  not hold this up; `max_swaps_per_min = 0` still does, and so does a model the user used
  lately when the newcomer is background work. CPU or partial placement is allowed in two
  cases: while another agent generates on the GPU, or when the model cannot fit an empty GPU
  (it then gets the most layers it can). A newcomer also never takes fewer GPU layers than it
  would get once idle models gave way (the `swap_gain` comparison now only breaks ties
  between placements with the same layers on the GPU).
- **Checked at every call.** When a call starts on a loaded model that runs partly or wholly on
  the CPU while nothing generates anywhere, and it would get more layers on the GPU once the
  models not generating gave way, the model is not used as it is. A warning is logged and it
  restarts between calls (swap reason `promote`, wait reason `making_room`) to be placed again.
  So an agent that ran on the CPU beside a generating one moves to the GPU at its next call
  once that agent is done.
- **Waiting agents give up their cells.** While an agent waits (for its subagents in
  `await_tasks`, an approval, an answer) and another agent takes a slot on the same model, the
  waiting agent's slot is saved to the slot files and its cells are freed
  (`POST /slots/{id}?action=erase`), so its subagents use the GPU pool. Its next call restores
  the prompt before it is sent (`restoring_context`). A session the user stops releases its
  agents' slots at once (`LocalModels::suspend` at the safe point where each halts). When an
  agent goes on, after Resume or after a steering message wakes the root, the engine
  (`ensure_resident`) checks `LocalModels::suspended` and calls `LocalModels::resume`, which
  restores the context ahead of the next call with a near-empty interactive call (`resume`). That loads the model if it was swapped out and
  puts the agent ahead of idle subagents. A waiting agent was already no claim on its
  model, so a subagent on another model that does not fit beside it swaps it out (slots saved)
  rather than running on the CPU, and it comes back with its slots restored when it resumes.
- **The critic.** A CPU-lane model (`placement = "cpu"`) runs on the CPU beside the agents
  generating on the GPU. When an agent of its own session is waiting on it, nothing generates,
  and no other session is working, it is placed on the GPU for that load. The waiting root's
  model gives way (slots saved) and comes back with its context restored when the root
  resumes. When another session is working it stays on the CPU lane.
- **Order.** Calls running now are never interrupted; an agent that resumes comes back on its
  next call (restoring its context, loading its model if it was swapped out); subagents and
  other newcomers take what the idle and waiting leave. `min_residency_s` and the swaps per
  minute still damp swapping between agents that are all working.

Not done yet: a resuming root does not jump the queue of a model whose slots are all busy; a
waiting agent's slot is only suspended when a free slot permit lets it be taken (with every
slot busy, llama-server may reuse it and its prompt then comes from the prompt cache or is read
again).

### Residency controller (implemented)

**Implemented (roadmap item 16):** `imperium-core` owns every decision about which local models
and KV caches are in memory; the `imperium-local` supervisor carries them out and decides
nothing on its own. The controller (`crates/imperium-core/src/residency.rs`) is a plan → actions
loop over one vocabulary (`imperium-local::residency`): actions `load`, `unload`, `save_slot`,
`restore_slot`, `resize_pool` and `set_budget`, each with a reason.

- **The plan loop.** Whenever the supervisor signals a change (a slot freed, an agent waiting or
  going on, settings changed) and at least every 15 s, the controller reads the loaded models
  and plans: a model whose settings changed restarts once no slot, queued call or mid-turn agent
  of any session holds it (`config`), and a model nobody has used for `idle_unload_s` unloads
  (`idle`). The supervisor executes each action (`Supervisor::execute`) and refuses one that would
  move a model or slot in use, or an idle unload of a model used again since it was planned. The
  supervisor no longer runs an idle-unload timer or a settings-restart task of its own.
- **Calls ask it.** A call whose model is not loaded asks the installed `ResidencyPolicy` what to
  swap out. The controller installs `CorePolicy`: the arbiter's rules above (never a model in
  use, the GPU rule and the critic exception, `min_residency_s`, the swaps per minute,
  `swap_gain`), with every eviction logged and reported with why it gave way. Waiting agents
  yield their KV cells to an agent on the same model through the policy too
  (`ResidencyPolicy::yield_slots`). What a call in flight needs it still does at once, as
  before, because the call is waiting for it: load its model, restore its saved context, and
  restart a pool that must grow or a model the GPU rule moves to the GPU, the restart now made
  by the first call waiting for the model once its running calls are done instead of by a
  background task.
- **Resume.** An agent that goes on after waiting (subagents done, a steering message waking
  the root, Resume after a stop) calls `LocalModels::resume`, which loads its model if it was
  swapped out and restores its context ahead of its next call, as the user's work, reported as
  `resume`. A stopped session's slots are saved as `stopped`.
- **A save comes before the restore that reads it.** A waiting agent's slot is saved (`yield`,
  `stopped`) with its save marked in flight; the agent's next call on that model waits for the
  save to finish (*Saving context*) and then restores what it wrote, so it never restores an
  older save over context it built since. A yield planned while the agent waited is dropped if
  the agent has gone on before the save starts.
- **Titles never load a model.** A session title asks for a slot with
  `LocalModels::acquire_loaded`: the supervisor hands out a slot only on a model loaded as it
  is, and answers `AcquireError::NotLoaded` (never loading, restarting, growing or moving the
  model) when it was swapped out between the title's check and its call. The title then falls
  back to the first message's first words.
- **A crashed server is recovered.** The supervisor asks each server whether its process has
  exited (`Server::exited`, a non-blocking `try_wait` on the child). A call that finds its
  model's server gone takes no slot on it and waits; the controller (on that signal or its next
  look) unloads it with reason `crashed` (`detail` says how it ended), whoever held it, since
  nothing can be saved from a dead server; the waiting call, or the next one, loads it again. A
  model that crashes again within 10 minutes stays down before it is reloaded:
  0 s after the first crash, then 5 s, doubling to at most 5 minutes
  (`ResidencyPolicy::crash_backoff`), so a server that dies as it loads does not spin. Calls
  holding a slot when the server died fail as before.
- **The budget follows the machine.** Every 30 s the controller reads the free VRAM (and, while
  no model is loaded, the available RAM) the way the start did, `[hardware]` overrides included,
  and moves the budget when it changed by 512 MiB or more: VRAM is what is free plus what the
  loaded models hold, so memory another program takes (a training run, a game) or gives back is
  planned around. RAM is not read while a model is loaded, since weights are memory-mapped and
  prompt caches fill lazily; a reading with no GPU where the start found one keeps the VRAM
  budget. Context ceilings stay sized against the memory measured at start (a pool that does
  not fit the live budget is cut, as beside other models), so a budget change never makes a
  loaded model stale.
- **Every move says why.** The supervisor reports each move (`Supervisor::moves`) and the daemon
  sends it to every client as `model.residency` (`ModelResidency`: `action`, `model_id`,
  `holder`, `reason`, `ok`, `detail`, `seconds`, `bytes`). Reasons: `call`, `resume`, `room`,
  `more_gpu`, `swap_gain`, `gpu_rule`, `critic`, `aged`, `idle`, `config`, `grow`, `promote`,
  `request`, `yield`, `stopped`, `budget`, `crashed`. An eviction's `detail` names the model it made room
  for, a load's its placement. `model.swapped_out` keeps its coarser reasons (every eviction for
  a load is `room`).

**Review (before the controller).** Every path that moved local-model memory, who decided, and
on what signal:

| Path | Decided by | Signal | Reported |
| ---- | ---------- | ------ | -------- |
| Load for a call, and its evictions | supervisor (`plan` → arbiter) | a call for a model not loaded | `model.swapped_in/out`, every eviction as `room` |
| Idle unload | supervisor's own 15 s task | `idle_unload_s` | `idle` |
| Settings restart | supervisor's own task, spawned by a settings change or a load that raced one | `models.set_config`, rescan, CPU-lane pins | `config` |
| Pool growth | supervisor (`context.rs` via `Loaded::growth`), restarted by the same task | a call that does not fit, or a full pool | `grow` (missing from the protocol docs) |
| GPU-rule promotion | supervisor (`promotes`), same task | a call on a model running on the CPU while nothing generates | `promote` (missing from the protocol docs), a warning |
| Waiting agent's slot save and erase | supervisor, a task per waiting agent spawned from `acquire` | another agent took a slot on the model | a log line only |
| Stopped session's slots | engine (`hold_while_stopped` → `LocalModels::suspend`) | the user stops the session | a log line only |
| Resume restore and reload | engine (`ensure_resident`, a near-empty call) | steering, subagents done, Resume | indistinguishable from a call |
| `models.unload` | daemon RPC → supervisor | the user | `request` |
| Background calls giving way | supervisor, with the load view and `interactive_hold_min` core hands it | the quiet-machine rule | `local_model_waiting` only |
| Titles | core (`title_model`: only a model `Loaded` with a free slot) | first exchange | none |
| Prompt cache size | supervisor (`cache_room`) | each load | a log line |
| Saved-slot disk cache | `slot_files` (`swap_cache_mb`, oldest of the busiest session first) | each save | none |
| Budget | measured once at start | none | none |

Findings: decisions were split across the supervisor's two background tasks, its call path, the
engine and the RPC handler; nothing could see why a model moved beyond `room`/`idle`; slot saves
and restores, resumes and the budget were not reported at all; the budget never followed other
programs on the GPU; and `model.swapped_out`'s documented reasons left out `grow` and `promote`.
Races found: (1) a waiting agent's slot save runs in a detached task and can finish after the
agent resumed and took another slot, so its next call restores the older context (llama.cpp
then re-reads from where the prompts diverge: wasted work, not a wrong answer); (2) titles check
that a model is `Loaded` and then call it, so a swap-out in between makes the title call load
the model it meant not to; (3) there is no supervisor restart of a crashed `llama-server`: a
server that dies stays listed as loaded until it is unloaded, and its calls fail. The controller
removes the split and the missing reasons. The three races are fixed since: a call waits for its
own slot's save (race 1), titles use a call that never loads (race 2), and a crashed server is
unloaded as `crashed` and reloaded with a backoff (race 3), as described above. The desktop's
wait line words `local_model_waiting` and `model.residency` ([07](07-desktop-ui.md#waiting-status-line-implemented)),
and `imperium chat` prints each `model.residency` move as a dim line.

Not done yet: reading free VRAM through NVML instead of `nvidia-smi`, and other vendors;
saved-slot cache evictions and prompt-cache sizing as reported moves; noticing a crash between
calls sooner than the controller's 15 s look; and measuring all of it on a 16 GB GPU (swap and
restore times, the budget refresh beside another GPU process).

`models.config` returns, for every local model and every model-table row, what `models.toml`
saves (`saved`), what is in effect (`effective`, from the daemon's settings snapshot), the memory
plan and placement, and `reload_pending` when a loaded server was started with a different
shape. `models.set_config` replaces each named model's saved settings whole (all-null removes
them), refuses unknown ids, settings a vLLM model cannot use and invalid values with
`INVALID_PARAMS` (`local model <id>: <reason>` or `model table row <id>: <reason>`), and writes
nothing then; with `dry_run` it only returns the would-be result, which the desktop dialog uses for
live memory estimates. Saves are serialised, and so are the settings a rescan after a download
hands the supervisor, so a save is never undone by an older snapshot. A custom `llama-server` (`runtime.llamacpp.custom_path`)
must be llama.cpp b12180 or later: it needs `--kv-unified`, `--kv-unified-per-slot` and
`--no-cache-idle-slots`. This is not probed; an older binary exits at once, its log names the
unknown argument, and the usual quick-failure error surfaces that.

**Implemented (stage 2f): the CPU lane.** Some work needs accuracy, not speed: critiquing a plan,
reviewing a design, checking a proof. On a machine with plenty of system RAM, a larger model can
live entirely in RAM and run on the CPU beside the GPU models, at no API cost and with no
VRAM planned for it. A model-table row opts in with `placement = "cpu"`:

```toml
[[models.catalog]]
id = "critic"
spec = "llamacpp:Qwen3-30B-A3B-Q4_K_M"   # a llama.cpp model; other specs are refused
location = "local"                        # required for placement = "cpu"
tier = "strong"
tags = ["critique", "review"]
placement = "cpu"                         # auto (default) | cpu
# parallel = 1                            # slots; default 1 for a CPU-pinned row
# context = 32768                         # most tokens per agent; default runtime.llamacpp.context
# max_wait_s = 3600                       # longest a call waits for a slot or for room; default
#                                         # 3600, or runtime.llamacpp.max_wait_s if longer
```

- **Pinned, not planned.** A pinned model is served by its own `llama-server` started with
  `--n-gpu-layers 0 --device none`, even when VRAM is free. `-ngl 0` alone would still
  initialise the GPU backend (compute buffers in VRAM) and run large batched operations there;
  `--device none` (llama.cpp b12180, `common/arg.cpp`) gives the server no GPU device at all.
  A model with an `mmproj` projector also gets `--no-mmproj-offload`, so the projector stays
  off the GPU without relying on it following `--device`. What is guaranteed is exactly that:
  Imperium plans zero VRAM for the model, passes those flags, and never counts it against the
  VRAM budget. The planner places it with
  `place_on_cpu`: weights, the KV pool (as it starts or has grown, unless the model's settings
  fix it), and the runtime overhead all count against RAM, so loading it leaves the GPU models' placements as they were. Nothing here has
  been measured on hardware (a driver may still hold a little memory for the process). The
  existing "partial" placement is different: it splits a model that does not fit the GPU, and
  still prefers the GPU. `placement = "cpu"` is validated at startup: it needs
  `location = "local"` and a `llamacpp:<id>` spec (a cloud row, another endpoint, or a vLLM
  folder is refused with the row's id); `parallel`, `context`, and `max_wait_s` are now per-model
  settings (`slots`, `context`, `max_wait_s` under `[runtime.llamacpp.models."<id>"]`), still
  accepted on the row, only valid with `placement = "cpu"` there, and at least 1 (a row and a
  model table that set the same key to different values are refused, naming both); and rows naming the same model must agree on `placement` and
  on those three, because the model is served once.
- **One RAM budget.** Everything that lives in RAM is counted together against free RAM less
  `runtime.llamacpp.ram_headroom_mb`: the CPU lane's weights and KV, every server's
  `--cache-ram` (the lane's included, sized from what is left after its weights as for any
  model), and any GPU model's layers that spill into RAM. Saved slots (stage 2d) live on disk
  and are not RAM. A pinned row that does not fit the RAM budget is refused at load with the
  usual "not enough memory" error and reported unavailable to routing, which falls to the next
  choice; it never spills onto the GPU. `models.local` lists it with `placement` `cpu`,
  `cpu_lane: true`, and its own slot count.
- **Outside GPU swaps.** Evicting a pinned model frees RAM and no VRAM, so it is never chosen
  as the victim of a swap made for GPU memory, however long it has been idle. The arbiter puts
  it last among candidates: it is evicted only when a newcomer cannot be placed unless the RAM
  it holds is freed, and then by the usual rules (nothing in use is evicted; one an agent is
  mid-turn on is waited for). It unloads after `idle_unload_s` like any model. Known cost: a
  newcomer that needs only RAM (a CPU-lane model, or a GPU model's RAM share) evicts idle GPU
  models before the lane, even when freeing the lane alone would have been enough; the order is
  kept simple on purpose, and the evicted models come back by the usual swap.
- **Measured speed.** After each model call the agent loop reports the tokens generated and how
  long generating them took, from the first generated token to the end. A thinking model's
  reasoning (`reasoning_content`, or `reasoning` from some services) counts as generated: it
  is in `completion_tokens` and starts the window, though it is not shown as reply text, and it
  sets the event's time to first token. A call whose window is under 100 ms (a buffered
  response or a reply of a few tokens) or that streamed nothing (a tool call alone) is not
  counted, since its time is mostly reading the prompt; its event's `tokens_per_sec` still
  shows tokens over the whole call. The supervisor keeps a running tokens-per-second per model,
  weighted by tokens so a short reply does not outweigh a long one, and `models.local` lists it
  as `tokens_per_s` (`imperium models local` and the desktop panel show it). It is kept after
  the model unloads and reset when the daemon restarts. `llama-server`'s own `timings` object
  is not read; the call's measured time is used.
- **Not in the ranking.** A CPU-pinned row is left out of the ranking by tier, so a task asking
  for `strong` work never lands on the slow lane by accident. It is chosen when an agent names
  it, or when a routing rule lists it in `use`. A rule matches one tag, so send each tag of
  the work you want on the lane to the row:

  ```toml
  [[routing.rules]]
  when_tag = "critique"
  use = ["critic"]

  [[routing.rules]]
  when_tag = "review"
  use = ["critic"]

  [[routing.rules]]
  when_tag = "verify"
  use = ["critic"]

  [[routing.rules]]
  when_tag = "proof-check"
  use = ["critic"]
  ```

  If the lane is unavailable (RAM short), the rule falls through to the ranking and, failing
  that, the session's model.

The four rules above are now built in, as [routing extras](#routing-extras-implemented): write
your own only to change where those tags go. Not done: giving CPU tasks a longer
`max_wall_s` and never sending a task with a short deadline to the lane. There is no task
deadline in the task contract on this branch; `LocalModels::tokens_per_s` is the measurement
that scheduling will use. The lane serves one call at a time unless `parallel` is raised, so
CPU tasks queue behind each other; the row's `max_wait_s` (default an hour, or the global
`runtime.llamacpp.max_wait_s` if longer) bounds that wait, so a call queued behind a
multi-minute task does not fail at the global 600 s.

*Choosing a model for the lane.* Prefer a mixture-of-experts model with few active parameters
over a dense one of the same quality: it reads only the active experts per token, which is what
limits generation on a CPU. A Qwen3-30B-A3B-class model (Apache-2.0; about 3B active
parameters of 30B) at `Q4_K_M` is the suggestion. These are estimates, to be replaced by a
measurement on the development machine before this becomes a default: about 18 GB of weights
plus a few GB of KV cache at 8-32K context, so a 64 GB machine holds it beside a 16 GB GPU's
models with room for the OS; tens of tokens per second on a desktop CPU, against about 4 for a
dense 32B model. A prompt of tens of thousands of tokens still takes minutes to read on the
CPU, which suits critiques that can wait. Use only models whose licence you have checked
(Apache-2.0 and MIT are the ones Imperium will suggest).

**Implemented (phase 7):** Hugging Face model folders (a `config.json` with safetensors files)
in `models.folders` are discovered as `vllm:<folder name>`, their shape read from `config.json`
(including `text_config` for multimodal models). They are planned GPU-only (vLLM does not
offload to system memory) and served by a supervised `vllm serve <folder>
--served-model-name <id>` on a free loopback port, with `--max-model-len` (the per-session
context), `--max-num-seqs` (the slots; no token pool), and a `--gpu-memory-utilization` sized to the model's
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

- **llama.cpp:** llama.cpp's own release builds per backend (CPU, CUDA 12/13, Vulkan, ROCm,
  Metal) are downloaded on demand and verified by SHA-256 ([runtimes](#runtimes)). Users can
  point at their own `llama-server` build instead, and one already on the machine is used
  while none is installed.
- **vLLM:** installed into a managed `uv` environment on Linux/WSL2, or run from the official
  Docker image, or used on a remote host. Imperium never bundles vLLM.
- Each loaded model is a supervised process with a port, health check, idle timeout, and
  memory reservation. Multiple slots (`--parallel`) over one KV pool (`--kv-unified`) let one
  loaded model serve several agents concurrently; an agent holds a slot and a token reservation
  only while a model call runs.

## Hardware awareness

**Implemented (phase 4):** at start the daemon probes system memory and cores (`sysinfo`) and
NVIDIA GPUs (`nvidia-smi`); Apple Silicon is planned as unified memory. `[hardware] vram_mb`
and `ram_mb` override the probe for GPUs it cannot read, or to plan as if memory were smaller.
The budget is the free VRAM and the available RAM less `runtime.llamacpp.ram_headroom_mb`,
measured at start and again every 30 s by the [residency
controller](#residency-controller-implemented) (VRAM always, RAM while no model is loaded);
loaded models reserve what the planner estimated (the runtime overhead is a fixed 512 MiB).
AMD/Intel/Metal probes and calibration against measured usage are later work. `backend = "auto"` picks `cuda13` for an NVIDIA driver of the
580 series or newer, `cuda12` for older drivers, `metal` on Apple Silicon, and `cpu`
otherwise; set `vulkan` or `rocm` explicitly.

## Active models and resource monitor

**Implemented:** `system.resources` returns a snapshot of what the machine and the sessions
are using, and `system.resources_subscribe {on}` sends the same snapshot as
`system.resources_changed` about every two seconds to the connections that asked. The daemon
samples only while at least one connection is subscribed (closing the connection
unsubscribes it), so an idle daemon reads nothing. A snapshot holds:

- **Machine:** CPU total and per core, RAM and swap used and total (`sysinfo`), and per NVIDIA
  GPU its utilisation, VRAM used and total, and temperature, read through `nvidia-smi` (NVML's
  counters). Without an NVIDIA driver the GPU list is empty; other vendors are later work.
- **Processes:** `imperium-core` itself and each model server (`llama-server`, `vllm`) with
  its pid, CPU share of the whole machine, RAM, and VRAM when the driver reports it per
  process (it does not on Windows under WDDM).
- **Local models:** every model the supervisor holds, in use or idle (cached until it
  unloads): runtime, placement (`gpu`, `partial` with its GPU layers, or `cpu`), whether it is
  pinned to the CPU lane, the context one session may use, its KV pool and the tokens running
  calls reserve from it, planned VRAM and RAM, slots in use and calls queued, whether an agent
  is mid-turn on it (a claim), the live sessions on it (their model, or calls on it in the last
  15 minutes), and while idle how long until `idle_unload_s` unloads it. A model running with
  a [draft model](#speculative-decoding-implemented) names it (its memory is in the planned
  VRAM and RAM); one whose configured draft is not used carries the notice saying why.
  `models.unload {model}` (or `imperium models unload <id>`) unloads an idle model now, saving
  its slots as a swap-out does (reason `request`); a model with a slot in use, a call queued,
  or an agent mid-turn on it is refused, as the [idle unload](#runtimes) spares it.
- **Endpoints:** each hosted `<endpoint>:<model>` that a live session uses or called in the
  last 15 minutes, with the endpoint's host, the sessions, requests and tokens in that window
  (from `model_call_completed` events, so subagents on catalogue models count), the latest
  call's latency, and failed turns with the latest error.

`imperium resources` prints a snapshot and `imperium resources --watch` keeps printing them;
the desktop's [Resources panel](07-desktop-ui.md#resources-panel-implemented) shows them live.

The daemon maintains a live **resource map**:

- GPUs: vendor, VRAM total/free (NVML, ROCm SMI, DXGI, Metal), compute backend support.
- System RAM, unified memory (Apple Silicon), CPU cores.
- Reservations held by loaded models and pending loads.

**Memory planner** for a candidate local model:

```text
required = weights + kv_bytes_per_token × pool_tokens + runtime_overhead
fits     = required ≤ free_vram (full offload)  or  partial offload within RAM headroom
```

In the implementation, the KV cache is one pool of `pool_tokens` shared by every slot (it
starts small and grows on demand, see above), so llama.cpp is started with `--ctx-size pool_tokens
--kv-unified`; the key and value caches may have different types, each counted at its size.
`max_gpu_layers` caps the layers offloaded, with the memory split recomputed for the capped
count. When a model does not fit on the GPU, as many whole layers as
fit (after the overhead) are offloaded and the rest must fit in the RAM budget; otherwise the
model does not fit.

KV bytes per token come from GGUF metadata (layers × KV heads × head dim × 2 × cache-type
bytes). Estimates are calibrated against measured usage after each load and stored per model.
A [draft model](#speculative-decoding-implemented) adds its own weights, KV cache and buffers
to `required`.

### Speculative decoding (implemented)

A model can name a smaller **draft model** that proposes a few tokens at a time while the model
checks them in one pass: the output is the one the model alone would give, produced faster when
the draft guesses well. It is a per-model setting (`runtime.llamacpp.models."<id>"` in
[10](10-configuration.md), *Draft model* in the model settings dialog), off unless set:

- `draft_model` is the id of another installed GGUF model (one found in `models.folders`, so it
  can be downloaded like any other); `draft_max`, `draft_min` and `draft_gpu_layers` are the
  optional parameters. llama.cpp models only; a vLLM model is refused a draft.
- The flags come from the pinned llama.cpp release (b12180, `tools/server/README.md` and
  `docs/speculative.md` of that tag): `--spec-draft-model <path> --spec-type draft-simple`, plus
  `--spec-draft-n-max`, `--spec-draft-n-min` and `--spec-draft-ngl` when the matching setting is
  set. Unset parameters leave llama.cpp's defaults (3 and 0 tokens, and `auto` for the GPU
  layers, which Imperium plans as the whole draft on the GPU). `--spec-type` defaults to `none`,
  so the draft model alone would not draft. The draft flags, by pattern (`--spec-draft-*`, `--draft*`, the long forms ending in `-draft`,
  and the short ones such as `-md`, `-ngld`, `-hfd`, `-ctkd`, `-devd`, `-td`), are Imperium's:
  they may not appear in `extra_args`, per model or global, nor `--spec-type` while a draft is
  set. Other flags Imperium sets are refused in a model's `extra_args` only, as before. llama.cpp's own `LLAMA_ARG_*` environment variables
  for the draft (`LLAMA_ARG_SPEC_DRAFT_*` by prefix, and the others) are removed from every llama-server's environment (`LLAMA_ARG_SPEC_TYPE` too
  while a draft is set), so the environment cannot name a draft that is not in the memory plan.
- **Memory.** The draft is part of the model's footprint everywhere the planner counts it: its
  weights, a KV cache as large as the model's pool at `f16` (llama.cpp sizes its context from
  the model's), and 128 MiB of buffers. It sits on the GPU unless
  `draft_gpu_layers` is `0` or the model is pinned to the CPU lane (then in RAM), so it comes off
  the room the model's own layers share; with a `draft_gpu_layers` below the draft's layer count the
  share of its weights and KV cache for the other layers is counted in RAM. The model's placement, `required_bytes`, the memory
  `models.local`, `models.config` and the resource monitor report, and the residency
  controller's reservations all include it; the context and the KV pool are sized with it.
- **Checks, each load.** The draft is used only if it is an installed GGUF model other than the
  model itself, smaller than it, with the same tokenizer family and a vocabulary within 128
  entries of the model's (read from the GGUF headers; a header without the fields is not held
  against it), and if it fits on an empty machine beside the model without pushing layers of a
  model that would fit the GPU alone off it.
- **Fallback, never a refusal.** When a check fails the model loads without the draft and
  `models.config` (`draft_notice`), the resource monitor (`draft_notice` on the loaded model),
  `imperium resources`, the model settings dialog and the load's entry in the residency log say
  why, for example *Draft model x is not used: it is not an installed model (check
  models.folders). The model runs without it.* A configured draft that is missing at config time
  is accepted: it may be downloaded later.
- **When llama-server itself cannot use the draft.** Two cases differ. If the process fails to
  start with the draft, the start is tried once more with it (the failure may be transient),
  then once without it; only if that start works is the draft blamed (the notice names the
  server's error) and left out of later loads until the model's settings or the installed
  models change. A start that times out is not retried and blames nothing, and a model that
  fails without the draft too blames nothing. But a draft whose vocabulary does not match the
  model's does not stop llama-server (b12180, `server-context.cpp`): it logs *failed to
  initialize speculative decoding context* and serves without speculation, as it does for a
  model whose cache cannot be rolled back (recurrent or hybrid architectures), for which it does
  not even try. So after every start with a draft Imperium asks the server (`GET /slots`, the
  `speculative` field of each slot); if it is off, the notice says the server runs without
  speculative decoding, the draft is left out of later loads like a refused one, and the memory
  reserved for the draft is released from the model's placement.
- Changing a loaded model's draft settings restarts it once nothing is using it, like any
  launch setting. A saved slot (swap-out) also holds the draft's state in llama.cpp, so the slot
  key includes the configured draft model: slots only restore into a server with the same draft
  setting. When a draft was configured but left out by a fallback, slots saved from that server
  still carry the configured draft's key.

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
alias = "Claude Sonnet"               # optional display name; the id stays what rules use
context_window = 200000               # optional; for context breakdowns (local models report theirs)

[routing]
prefer = "local"                      # cheapest (default) | local | cloud

[[routing.rules]]                     # first matching rule wins
when_tag = "code"                     # optional; also when_tier = "strong"
location = "local"                    # optional restriction
use = ["qwen-local"]                  # optional ids, tried in order
```

A subagent's model is chosen as follows:

1. A model the agent named (`spawn_agent`'s `model`, by id or spec), if it is in the table, or
   a local model found in the model folders that the table does not name and the user did not
   hide (by its spec, `llamacpp:<id>`; see [model awareness](#model-awareness-and-model-cards-implemented)).
   Such a local model is reached by name only, never by the ranking below.
2. The first rule whose `when_tier` (exact) and `when_tag` match the task: its `use` ids are
   tried in order (tier is not checked for an explicit choice; the location restriction is),
   then its `location` restricts the remaining choice, and its optional `tier` raises the
   task's minimum tier to at least that one. If none of your rules matched, the first matching
   [built-in task-class rule](#task-classes) applies instead, but its location and tier are
   preferences rather than restrictions.
3. The suitable rows (at or above the requested tier, default `standard`, and in the requested
   or rule location) ranked by `prefer`, then by input plus output price, with unpriced rows
   counted as free. Rows pinned to the [CPU lane](#runtimes) (`placement = "cpu"`) are left
   out: they are for work that can wait, chosen by name or by a rule.
4. The session's own model.

Agents pass `tier`, `location`, `tags`, `model`, `single_call`, and `effort` ([reasoning
effort](#reasoning-effort)) to `spawn_agent`. Ids must be unique and
rules may only name ids in the table; the daemon refuses to start otherwise. Rows whose
endpoint is unknown or whose key is missing are skipped with a warning. `models.list` returns
the table, and the desktop app shows it in a searchable, sortable Models panel.

### Reasoning effort

**Implemented.** Every session has a reasoning effort, `low`, `medium`, `high` or `max`: it
starts at `models.effort` (default `medium`), `session.control` `set_effort` changes it (from
every agent's next model call, during a turn too), and it is kept in the session's meta file,
so a resumed session keeps it. `spawn_agent` takes an optional `effort` for one subagent; other
subagents use the session's. The effort is sent only to models that can reason, decided in this
order:

1. the model table row's `reasoning = true | false`;
2. its tags: `reasoning` or `thinking` say yes, `no-reasoning` or `no-thinking` say no;
3. for a local model, `runtime.llamacpp.models."<id>".reasoning`, else its chat template (the
   GGUF's `tokenizer.chat_template`, or a Hugging Face folder's `chat_template.jinja` or
   `tokenizer_config.json`): a thinking switch (`enable_thinking`), think tags, or a
   `reasoning_effort` variable mean it can;
4. the model's name: known reasoning families (OpenAI o-series, GPT-5 except `-chat`,
   gpt-oss, Claude 3.7 and 4 onwards, DeepSeek R1, QwQ and Qwen 3 except `-Instruct`, Gemini
   2.5 and 3, Grok 3 mini and 4, Magistral, GLM 4.5 onwards, ...).

`models.list` and `models.local` rows carry `reasoning`, and session summaries carry `effort` and
whether the session's model can reason. Each endpoint gets the effort in its own form, chosen by
its host (every endpoint speaks Chat Completions):

| Backend | What is sent |
|---|---|
| OpenAI and other OpenAI-compatible services | `reasoning_effort`: `low`, `medium`, `high` (`max` is sent as `high`, the highest level every server accepts) |
| llama.cpp, vLLM, Ollama (local servers) | the same `reasoning_effort`; llama.cpp hands it to the chat template, so templates without levels only see that reasoning is on. llama.cpp has no per-request thinking budget (only the server's `--reasoning-budget`), so none is sent |
| OpenRouter (`openrouter.ai`) | `reasoning: {"effort": ...}`, `max` included |
| Anthropic (`api.anthropic.com`, its OpenAI-compatible endpoint, which ignores `reasoning_effort`) | `thinking: {"type": "enabled", "budget_tokens": N}` with 1024, 4096, 12288 or 24000 tokens; `max_tokens` is set to the budget plus 8000 when the call sets none, and a smaller cap shrinks the budget (no thinking below 1024). Claude 5 models think adaptively whatever the budget |
| DeepSeek (`deepseek.com`), Mistral (`mistral.ai`) | nothing: no effort control is known |

Summaries for compaction and session titles are sent no effort.

### Task classes

**Implemented (stage 2e):** menial work defaults to small local models and hard work to cloud
frontier models, from the tags an agent puts on `spawn_agent`. These built-in rules come after
your `[[routing.rules]]` and apply only when none of yours matched, so a rule of yours for the
same tag (or a catch-all) replaces them:

| Tags (checked in this order) | Location | Tier |
| --- | --- | --- |
| `architecture`, `math`, `proof`, `algorithm`, `design` | `cloud` | at least `frontier` |
| `refactor`, `tests`, `ui`, `format`, `docs` | `local` | the task's own (not raised) |

A task with tags from both rows counts as hard. A built-in rule's location and tier floor are
preferences, not restrictions: the router first looks for a row in that location at the floor,
then (for a hard task with no cloud `frontier` row) for a `frontier` row in any location, and
when there is none (no local row, memory is short, or no `frontier` row at all) it ranks the
rows of every location at the task's own tier by the usual steps. Each of these passes leaves
out CPU-lane rows, as the ranking does. An explicit `location` on the
call, and a `location` or `tier` in a rule of yours, remain requirements. Set
`routing.builtin_rules = false` to turn the built-ins off.

A task for a frontier model can be sent with `single_call: true`: the subagent gets the brief
and no tools, makes exactly one model call, and its reply is the report. The brief must carry
everything needed (see the [dispatch contract](03-agent-engine.md#dispatch-contract)).

**Implemented (phase 4):** rows with spec `llamacpp:<id>` (location must be `local`) are
placed by the memory planner. A local row is considered only while it can run: loaded, fitting
beside the loaded models, or fitting once loaded models nobody has a slot on are swapped out
(stage 2d; not while the swap limit is reached). In each ranking step a loaded model beats one
that would have to be loaded, which beats one that needs a swap, and nothing in use is evicted,
so when memory is short the choice falls to the next suitable row, typically hosted. A row that
needs a swap carries the expected seconds (the model's last measured load, or its size at
2 GiB/s), for stage 2e's comparison of the swap with a hosted model's price per task class; until
then a local row that can swap wins on price like any local row.
A session's own model can also be `llamacpp:<id>`. Queue-delay handling, capability filters,
escalation and the built-in CPU-lane rules are [routing extras](#routing-extras-implemented);
the cost scoring with load latency below is later work.

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

### Routing extras (implemented)

Four additions to the choice above. Each applies in the router's decision, which the unit tests
exercise with fake models and fake load views; none touches budgets, permission modes or the
safety floor, since the chosen model is checked against its budget and runs under the session's
permissions as before.

- **Queue-delay handling.** A local runtime reports how long a call made now is expected to
  wait for a slot (`LocalModels::queue_wait_s`: calls queued ahead and the busy slots, times the
  model's recent seconds per call; 30 s per call before any is measured; 0 when a slot is free
  or the model is not loaded). A local model whose wait is over `routing.max_queue_wait_s`
  (default `120`; `0` turns it off) is skipped for the next eligible route, hosted if the call
  and rules allow it, and the task's notice says which model was skipped and the wait. Rows
  reached only by a rule or a name (the CPU lane) are never skipped for their queue; their wait
  is bounded by the row's `max_wait_s`.
- **Capability filters.** A task needs *vision* when it carries the tag `vision`, *long
  context* with the tag `long-context`, and *tools* unless it is `single_call`; a rule can add
  to these with `require = ["vision", "tools", "long_context"]` for the tasks it matches. A
  model declares them on its table row: the tag `vision` for vision, the tag `no-tools` for a
  model that cannot call tools (tools are assumed otherwise), and a context window (the row's
  `context_window`, or a local model's planned context) of at least
  `routing.long_context_tokens` (default `100000`; an unknown window does not qualify). A model
  without a needed capability is skipped, even when named, and the notice gives the reason; when
  none qualifies the session's model carries on.
- **Escalation.** A subagent that fails, or whose report begins `CONFIDENCE: LOW` (the subagent
  prompt asks for it when the task is beyond the model), is run again with its report appended
  to the brief on a stronger route, up to `routing.escalation_attempts` times (default `1`;
  `0` turns it off). The route comes from the first matching rule's `escalate = ["id", ...]`
  (one id per attempt, in order), else the router runs again from the tier above the failed
  model's, never choosing the failed model; at the top tier there is nowhere to go and the
  failure is reported. A model whose budget blocks is not tried and one whose budget asks is
  approved at its first call, as for any subagent. Writer subagents (`access: "write"`) are not
  retried: their edits stay in their worktree. A notice marks each retry; the task keeps its id.
- **Built-in CPU-lane rules.** Tasks tagged `critique`, `review`, `verify` or `proof-check`
  prefer a row pinned to the [CPU lane](#runtimes) that can take them now (at or above the
  task's tier, within its location, and not backed up past the queue limit), choosing the
  highest tier, then the most ready. With no such row the usual ranking decides, and when a lane
  row exists the notice says why it was not used. They apply only when none of your
  `[[routing.rules]]` and none of the [task-class rules](#task-classes) matched, so a rule of
  yours for the same tag replaces them, and `routing.builtin_cpu_lane = false` (or
  `routing.builtin_rules = false`) turns them off. A rule can also set `cpu_lane = true` to
  ask the same for its own tags.

Notices appear where the task starts, in the same place as other session notices (the desktop
app's session view, `imperium chat`); there is no protocol change.

## Model awareness and model cards (implemented)

The root agent picks a model for delegated work with the `list_models` tool. It lists the
models the session may delegate to: the model table's enabled rows, and every local model the
table does not name (by spec, `llamacpp:<id>` or `vllm:<id>`, which `spawn_agent` accepts by
name). Models the user hid are left out: the desktop app keeps `models.hidden` in step with
its hidden marks ([07](07-desktop-ui.md)), and a hidden local model outside the table is not
offered by name either. For each model it gives:

- the id to pass as `spawn_agent`'s `model`, the display name, local or hosted with its spec,
  the tier and tags of a table row, and the context it reads per request (the table's
  `context_window`, else the local model's planned context, else what it was trained for);
- whether it reads images (a `vision` tag or a vision projector beside a local model) and
  whether it reasons (as for [reasoning effort](#reasoning-effort): the row's setting or tags,
  a local model's chat template, else its name);
- for a hosted model, its input and output price per million tokens;
- for a local model, whether it fits in memory now, from the planner's estimate (nothing is
  loaded): `loaded`; `fits_now` beside the loaded models; `fits_after_unloading_idle`, when it
  fits once loaded models nobody is using (no call on them or queued, no agent mid-turn) are
  unloaded; or `does_not_fit` while the models in use stay loaded or at all. The placement it
  would get (`gpu`, `partial`, `cpu`) follows, and a table row whose file is gone says so;
- its measured speed and its **card**.

A **model card** says what the model is good at (coding, writing, research, and so on). It
joins, in order:

1. the user's card: `card` on a `[[models.catalog]]` row (or `models.overrides."<id>".card`)
   and `runtime.llamacpp.models."<id>".card` for a local model, edited in the model dialog;
2. what the model's metadata says (`imperium_local::card`): the GGUF header's name, size label,
   fine-tune, architecture, quantisation, trained context, `general.tags`, languages, licence and
   description; a vision projector beside it; and the Hugging Face model card (`README.md` with
   front matter) in its folder when one came with it: pipeline, tags, languages, base model,
   licence and its first paragraph. Strengths are guessed from the words of its name, tags and
   pipeline (`coder` suggests coding, `VL` or a projector reading images, `R1` or `thinking`
   reasoning, `instruct` chat);
3. Imperium's measurements since the daemon started: calls, mean tokens per second and time to
   the first token from every session's `model_call_completed`, and a local model's measured
   generation speed.

With a `query` (a task description), the models whose cards best match come first, with what
matched; the rest follow. The search uses the retrieval layer
([17](17-memory.md#model-cards-implemented)). `location` limits the list to local or hosted
models and `limit` caps it (50 at most).

The root agent is told, while it has the tool, to call it with a query before delegating work
that suits a particular kind of model, to prefer a model whose card fits and that is loaded or
fits now, to take one that fits only after unloading idle models when it is clearly better,
never one that does not fit, and to pass the id as `spawn_agent`'s `model` (the same rule is in
`schemas/prompts/emperor-root.md`). Cards are data, not instructions.

`models.list_models` says who gets the tool: `root` (default), `all` (subagents too) or `off`.

```toml
[models]
hidden = ["llamacpp:old-model"]       # kept by the desktop app; list_models leaves these out
list_models = "root"                  # root | all | off

[[models.catalog]]
id = "coder"
spec = "llamacpp:Qwen3-Coder-30B-A3B-Instruct-Q4_K_M"
location = "local"
tier = "standard"
card = "Good at: coding, refactoring, tests. Weak at long documents."

[runtime.llamacpp.models."gemma-3-12b-it-Q4_K_M"]
card = "Good at: reading screenshots and diagrams; short answers."
```

Agents write cards with `set_model_card`, recorded and undoable
([06](06-extensibility.md#agents-manage-the-app-config-implemented)). Not yet: measurements are
kept in memory only (they start again with the daemon).

## OpenAI-compatible local API (implemented)

Other tools (editors, chat front ends, scripts) can use Imperium's local models through an
OpenAI-style HTTP API. It is a listener of its own in `imperium-core`, **off by default**, and
speaks only what those tools need: `GET /v1/models` (also `GET /v1/models/{id}`) and
`POST /v1/chat/completions`, streamed (server-sent events) and not. Completions, embeddings,
and the other endpoints are not served, and no CORS headers are sent, so browser pages on other
origins cannot call it.

```toml
[api.openai_compatible]
enabled = false           # on: listen as below; off: stop at once
port = 0                  # 0 until chosen: the first enable picks a free port (from 11500) and saves it
bind = "127.0.0.1"        # loopback only, unless allow_lan is set
allow_lan = false         # risky: lets bind be an address other computers can reach
max_concurrent = 4        # most calls in flight at once; one more gets a 429
expose = []               # specs of hosted models to serve; local models need no entry
```

Settings → API, `imperium api enable|disable|status|token [rotate]`, and `api.get` / `api.set` /
`api.token` change these and apply them at once ([07](07-desktop-ui.md#api-settings-implemented),
[02](02-architecture.md#protocol)); the keys also work through `config.set`.

- **Where it listens.** `bind` must be a loopback address (`127.0.0.1`, `::1`, or `localhost`).
  Any other address is refused, the server does not start, and `api.get` reports why, unless
  `allow_lan = true` is also set. That serves plain HTTP to the network: anyone who learns the
  token can use the models (and the hosted ones exposed), so the setting is documented as
  risky and logged at start. The port is chosen once and kept, so tools need configuring once;
  if it is taken later the server reports the error instead of moving. Turning `allow_lan`
  off through `api.set` or Settings → API puts `bind` back on `127.0.0.1` when it was an
  address beyond this computer, and leaves a loopback address you chose (such as `::1`).
- **Authentication.** Every request, unknown routes included, needs
  `Authorization: Bearer <token>`; otherwise the answer is `401` with `invalid_api_key`. The
  token is separate from the daemon's: 32 random bytes made on first use, kept in the secret
  store as `api.openai-token` (the keychain), compared in constant time, never written to the
  config, the data folder, or the logs, and shown or rotated only through `api.token`
  (Settings → API, `imperium api token`). Rotating ends every tool's access at once.
- **Models.** The ids are Imperium specs (`llamacpp:qwen3-4b`, `vllm:<folder>`), with the
  alias as the display `name` in the listing. Every local model the runtime found is served,
  except those it cannot run here and those hidden in `models.hidden`. A **hosted** model (any
  row of the model table reached over an endpoint) is served only when its spec is in `expose`,
  because each call spends the user's key and money; `api.set` accepts only specs of hosted
  rows, and a request for one that is not exposed gets `404 model_not_found` that says so.
  The endpoint's key never leaves the daemon.
- **Calls.** A chat completion takes a slot on its model exactly as a session's call does
  ([residency controller](#residency-controller-implemented)): the model is loaded if needed,
  the call queues first-come, first-served while every slot is busy and while room is made, up
  to `runtime.llamacpp.max_wait_s`, the move is reported through `model.residency`, and the
  slot is released when the response ends or the client disconnects (which also stops the
  generation). API calls rank as interactive. The response begins when the slot is held, so a
  client with a short timeout should allow for a model load. Nothing bypasses the controller.
  At most `max_concurrent` calls are in flight (running, or queued behind a model); one more
  is refused at once with `429` `rate_limit_exceeded`, and a call keeps its place until its
  answer, a streamed one included, has ended. Timeouts keep a stuck call from holding a slot:
  a local model's server gets that model's `max_wait_s` (its model-table row's or settings',
  else `runtime.llamacpp.max_wait_s`, so a slow CPU-lane row is given longer) to begin its
  answer, or to finish it for a call that is not streamed, and a hosted endpoint gets
  `runtime.llamacpp.max_wait_s`; a streamed answer may pause 5 minutes between chunks; and a
  client that leaves a chunk unread for a minute is given up on, which ends the generation.
  A client that hangs up, or a server that holds the request and never answers, does not
  escape the meter: the call is recorded by its estimate (the prompt, and what reply there
  was) when it is dropped. Only a call the server refused or could not be reached for counts
  nothing.
  The reply of a local model is capped by the slot's token reservation (`max_tokens` is
  clamped to it, which `runtime.llamacpp.max_output_tokens` sets; raise it, or set 0 for no
  cap, to allow longer replies).
- **Request fields.** `messages`, `temperature`, `top_p`, `max_tokens` (or
  `max_completion_tokens`), `stop`, `seed`, the penalties, `logit_bias`, `response_format`,
  `logprobs`, `stream`, `stream_options.include_usage`, and `reasoning_effort` (mapped like a
  session's [effort](#reasoning-effort), and only for models that reason) are relayed;
  `n` above 1 is refused, and every other field, including llama.cpp's own (`id_slot`,
  `grammar`, ...), is dropped, because the runtime owns them. `tools` and `tool_choice` are
  passed through only to a backend that takes them: llama.cpp (always started with `--jinja`)
  and hosted endpoints do; a vLLM model does when `--enable-auto-tool-choice` is among
  `runtime.vllm.extra_args`; otherwise the request fails with `400`. Responses are the server's
  own, with `model` rewritten to the id the client used and llama.cpp's `timings` removed.
  A streamed response includes a usage chunk only if the client asked for one.
- **Budgets.** Each call is recorded in the usage ledger with the source `api` (tokens from the
  server's usage report, else an estimate; cost from the endpoint's prices), so it counts
  toward global, endpoint, and model budgets and toward a budget of scope `source` with target
  `api` ([budgets](10-configuration.md#budgets)) that caps API use alone. A budget that is
  used up and blocks or asks refuses the call before it reaches the model, with
  `429` and `{"error": {"type": "insufficient_quota", "code": "budget_exceeded", ...}}`;
  nobody can be asked through the API, so an asking budget refuses as a blocking one does.
  A call is admitted by what it is expected to use (its prompt estimate plus its `max_tokens`,
  or 1,024 tokens, priced at the endpoint's prices), and that counts against the budgets for
  calls that arrive meanwhile until the call is recorded with what it really used, so calls
  that arrive together cannot all pass a budget with room for one. The check is still "used
  up", not "would be used up": a call that fits the remaining room is admitted, and a later
  one is refused. Hosted replies are not capped by Imperium: the model's `max_tokens` is the
  client's own (none if it sends none), so one call can use more than the budget has left;
  the budgets stop the calls after it, not that call.
- **Errors** use OpenAI's shape, `{"error": {"message", "type", "param", "code"}}`: `400` for a
  bad request or one the model's server rejects (such as a prompt that does not fit), `401`,
  `404` for an unknown or unexposed model or URL, `429` for budgets (or the endpoint's own rate
  limit), `502` when the model's endpoint fails, and `503` when a local model cannot be loaded.

## Local model benchmark (implemented)

How fast a local model is depends on the machine, the quantisation, and the runtime build, so
Imperium can measure it. `models.bench` (Settings → Models, `imperium models bench [<id>]`) runs a
small fixed prompt set against one installed local model, or against each model that can run
here, and reports and saves the numbers. **It never runs unasked**: no schedule, startup, or
agent starts it, and there is no setting that does (so there is nothing to add to
[10](10-configuration.md); the results file is below). Speculative decoding with a draft model
is a separate, later item ([12](12-roadmap.md)); the benchmark is how its gain will be judged.

- **Through the normal path.** The benchmark is a caller of the supervisor like a session: it
  takes a slot with `acquire_with` as a **background** call
  ([residency controller](#residency-controller-implemented)), so the model is loaded, queued,
  and swapped by the same rules and reported through `model.residency`, the call waits while
  the [load view](14-projects-and-automation.md#quiet-periods) reports the machine
  **saturated** and never evicts a model the user's own calls used recently (the
  `load.interactive_hold_min` hold), and it is bounded by the model's `max_wait_s`. It does not
  wait for a fully quiet machine: a busy one is allowed, so the machine's load is stored with
  the result. There is no second launcher. The stored `machine` is the worse of the load
  levels (`quiet`, `busy`, `saturated`) read when the model's turn began, once it held its
  slot, and after its last prompt, so a number taken while the machine was busy can be told
  apart. While it waits, the run's step says why (loading, a busy slot, another model in the
  way, memory being freed, or a saturated machine).
- **Cold start.** By default a model that is loaded and has sat idle for longer than
  `load.interactive_hold_min` is unloaded first (through the residency controller, which also
  refuses while anything uses it), so its load time is measured; the unload is skipped while
  the machine is saturated. A model used more recently, or in use, is left alone and its load
  time is left out (`--warm` and the *Keep a loaded model loaded* switch skip the unload on
  purpose).
- **The prompt set.** Three prompts, fixed so results from different days compare: `short`
  (a one-line question, up to 96 tokens of reply), `medium` (a few hundred tokens of prose to
  summarise) and `long` (about five times that); the long one is left out for a model whose
  context is too small for it. Each is sent once, streamed, with `temperature` 0, a fixed
  `seed`, the prompt cache off (so every prompt is really processed) and thinking off for
  llama.cpp; replies are capped by the slot's reservation like any call's.
- **What is measured**, all from the arrival times of the real stream, not from the server's
  own `timings`:

| Metric | How |
| ------ | --- |
| Load time (`load_s`) | the seconds of the runtime's own `load` move made for the call; absent when the model was already loaded when the call began |
| Time to first token (`ttft_ms`) | request sent to the first text (a thinking model's reasoning counts); the result is the median of the prompts, each prompt has its own |
| Prompt speed (`prompt_tok_s`) | the server's reported prompt tokens over the time to the first token, summed over the prompts (a lower bound on the true prefill rate, since the time includes the first token); absent for a prompt whose server sent no usage report, which is also left out of the sum, since its size would only be an estimate |
| Generation speed (`gen_tok_s`) | reply tokens after the first over the time from the first text to the last, summed over the prompts; without a usage report the number of streamed chunks stands in for the tokens |
| Peak memory (`peak_ram_bytes`, `peak_vram_bytes`) | the highest resident memory, and GPU memory when `nvidia-smi` reports it for the process, of the model's server process, read every second from before the model loads until the prompts end (each reading of `nvidia-smi` is bounded by a timeout); absent when the server is not a process Imperium can read (a stand-in in tests, an unsupported GPU), never estimated |

- **Results are saved** to `<data>/local-benchmarks.json` (the newest 200), one entry per model
  and run: the model, run id, date (UTC), the **hardware fingerprint** (a short hash of the
  GPUs and their memory, system memory, cores and the runtime build: for llama.cpp the
  release Imperium pins and the pack in use, for vLLM vLLM alone, so a different GPU, memory
  or build counts as another machine, while free memory does not), the same in words, the
  runtime, how it was placed (`gpu`, `partial`, `cpu`), the machine's load level, every metric
  above and each prompt's own numbers. A model that fails to load or answer, or whose prompts
  all fail or do not fit its context, is reported in the run with its reason and not saved;
  the run goes on to the next model. A file that cannot be parsed is renamed to
  `local-benchmarks.json.bad` and a new one started; one that cannot be read is left alone and
  the result is not saved.
- **Cancelling.** `models.bench_cancel` (Settings → *Cancel*, Ctrl+C in the CLI) stops the
  run's task: the request in flight is dropped, which releases the slot and ends the generation
  at once. The run's state changes first, under the lock that saving takes, so a result that
  finishes at that moment is not saved afterwards. Models measured before keep their saved
  results; the one being measured has none. The model stays loaded until it idles out like any
  other. The CLI handles Ctrl+C from before it sends the request and cancels whatever
  benchmark is running.
- **Protocol.** `models.bench` returns the run (`running`) at once; `models.bench_progress`
  notifications (a `BenchRun` with its step, models done and results so far) follow every
  step, and the last one has the state `done`, `cancelled` or `failed`: `failed` when no model
  could be measured or the run's task panicked; `done` with a message when only some failed.
  `imperium models bench` exits non-zero unless the run is `done`. Only one run may run at a
  time, and the last 20 runs are kept in memory. `models.bench_results` returns the saved
  results newest first (optionally for one model), this machine's fingerprints (for llama.cpp
  and for vLLM models), and the runs since the daemon started. A paired phone's connection may
  not call the benchmark methods and is not sent `models.bench_progress` (it names models and
  carries error text).

## Cost accounting

Every call records prompt, cached, and completion tokens, latency, time-to-first-token,
tokens/sec, and cost. The ledger aggregates per call, agent, task, session, model, and day,
and is exposed through `telemetry.*` ([06](06-extensibility.md#telemetry-api)).
