# 10 — Configuration

## Layers

Later layers override earlier ones, key by key:

1. Built-in defaults
2. User config: `<config>/config.toml`
   (2a. App settings: `<config>/models.toml`, written by the desktop app; see below)
3. Host config (SSH host's own `config.toml`)
4. Project config: `<project>/.imperium/config.toml` (shareable) and
   `<project>/.imperium/config.local.toml` (git-ignored)
5. Session overrides
6. Environment variables (`IMPERIUM_*`) and CLI flags

Implemented so far (phase 0): defaults, user config, project config, app settings, and
environment variables. Host config, session overrides, and CLI flags arrive with the features that need
them.

- **Environment variables** use `IMPERIUM_<SECTION>__<KEY>` with a double underscore between
  levels, for example `IMPERIUM_DAEMON__PORT=8080` sets `daemon.port`. Values are read as TOML
  (`8080`, `true`, `["a"]`) and otherwise as plain strings. Only nested keys can be set this
  way; `IMPERIUM_HOME` is not configuration (it relocates all data directories).
- **The app layer** (`<config>/models.toml`, shown as `app settings <path>` by `config explain`)
  holds the model settings saved from the desktop's model settings dialog
  (`runtime.llamacpp.models."<id>"` and `models.overrides."<id>"`). It is loaded after the user
  file and project files and before environment variables, so what is saved in the app wins over
  `config.toml`, key by key; your hand-written `config.toml` is never rewritten, and the same
  keys written there work too and show as effective when the app saved nothing. The app
  refuses invalid values when saving and writes nothing; when the daemon or the CLI loads, an
  entry of the file that does not validate is dropped with a warning (logged by the daemon, listed
  by the loader), and a file that is not valid TOML is ignored with a warning, so a bad
  `models.toml` never stops the daemon; saving from the app refuses such a file (naming it and
  the parse error) rather than overwrite it. A save applies only what `models.toml` changes:
  edits made to `config.toml` while the daemon runs take effect when it restarts.
- **Unknown keys are ignored**, so a config written for a newer version still loads; a known
  key with the wrong type is an error naming the problem.
- `imperium config explain <key> [--project <dir>]` shows the effective value and the layer
  that set it (for example `set by environment variable IMPERIUM_DAEMON__PORT`).

### Editing settings from the app

The desktop app's Settings page reads and changes settings through the daemon:

- `config.describe` lists every user-settable scalar and list setting (one table in
  `imperium-config`, `registry.rs`) with its `key`, `section`, `label`, `description`, `kind`
  (`bool`, `integer`, `number`, `string`, `path`, `path_list`, `string_list`, `enum` with
  `options`, or `secret_ref`), `default`, effective `value`, the `source` layer that set it
  (`default`, `user`, `project`, or `env`), and `restart`. A `secret_ref` shows the secret's
  name only, never its value.
- `config.set { key, value }` writes the value into the **user** config file (created when
  missing), keeping its comments, formatting, and unknown keys; a `null` value removes the key so
  the default applies. The whole configuration is then loaded and validated, and an invalid
  change is undone and reported as an error. Project files are never written. When an
  environment variable sets the key, the file is still written but the result says
  `overridden: true`, since the variable keeps winning.
- The daemon reads its configuration once at start, so `restart: true` (nearly every setting)
  means the change applies after a daemon restart, which the app offers. Settings with
  `restart: false` (`models.suggest_downloads`, `models.default`, `models.folders`, and
  `runtime.llamacpp.custom_path`) apply at once; a changed `models.folders` is scanned again
  at once. The desktop app's model setup and `models.add_folder`, `models.use`,
  `models.add_endpoint`, and `runtime.set_server` write these keys the same way
  ([setting up models](04-models-and-routing.md#setting-up-models-from-the-app)).

Structured lists and maps (`endpoints`, `models.catalog`, `routing.rules`, `mcp.servers`,
`projects`, `schedules`, `hooks`, `github.watch`, `media.speech`, `media.transcription`) are not
listed; edit them in the file.

### Implemented settings

| Key           | Default | Meaning                                                        |
| ------------- | ------- | -------------------------------------------------------------- |
| `daemon.port` | `0`     | Port of the local WebSocket listener; `0` picks a free port at each start |
| `agents.max_steps` | `50` | Model calls an agent may make in one turn before it stops |
| `resume.mode` | `wait_for_reset` | When a provider's usage runs out: `wait_for_reset`, `at_time`, `retry_every`, or `stop` (each session can override it) |
| `resume.at_time`, `retry_every_minutes`, `max_wait_hours` | none, `5`, `12` | Local `HH:MM` for `at_time`; retry interval; longest wait in one turn |
| `agents.resume_days` | `7` | Sessions active within this many days continue after the daemon restarts |
| `sessions.auto_archive_on_pr_close` | `true` | Archive a session once the pull request it watches is merged or closed ([archiving sessions](14-projects-and-automation.md#archiving-sessions)) |
| `sessions.agent_state_keep` | `true` | Keep each agent's state (a subagent's conversation, plan, and usage) in `<data>/sessions/<id>.agents/`, so finished subagents can be resumed with a follow-up, also after a restart; deleted with the session ([agent state](03-agent-engine.md#agent-state)) |
| `sessions.auto_archive_after_days` | `0` | Archive an idle session after this many days without activity; `0` never does. Working, waiting, pinned, and agent sessions are never archived |
| `models.catalog` | none | The model table: `[[models.catalog]]` rows with `id`, `spec`, `location` (`local` or `cloud`), `tier` (`basic`, `standard`, `strong`, `frontier`), optional `tags`, and optional `context_window` (tokens per request, for [context breakdowns](03-agent-engine.md#context-management); llama.cpp models report their own), optional `placement` (`auto` by default, or `cpu` to pin a `location = "local"`, `llamacpp:<id>` row to the [CPU lane](04-models-and-routing.md#runtimes): served with no GPU layers, in system RAM only, never swapped out for the GPU; a cloud row or another spec is refused at startup), and, only valid with `placement = "cpu"`, optional `parallel` (slots, default `1`), `context` (tokens per slot, default `runtime.llamacpp.context`) and `max_wait_s` (longest a call waits for a slot or for room on that row, default `3600` or `runtime.llamacpp.max_wait_s` if longer); a CPU-pinned model is started with `--n-gpu-layers 0 --device none` (plus `--no-mmproj-offload` with a projector), and rows naming the same model must agree on these; see [routing](04-models-and-routing.md#routing) |
| `routing.prefer` | `cheapest` | Tie-breaker among suitable models: `cheapest`, `local`, or `cloud` |
| `routing.rules` | none | `[[routing.rules]]` delegation rules with optional `when_tier`, `when_tag`, `location`, `tier` (raises the task's minimum tier), and `use` (model table ids in order) |
| `routing.builtin_rules` | `true` | Whether the built-in [task-class rules](04-models-and-routing.md#task-classes) (hard tags prefer cloud `frontier` rows, menial tags prefer local rows at the task's own tier) apply when none of `routing.rules` matched |
| `models.default` | none | Model for new sessions, as `<endpoint>:<model>` (split at the first `:`, so `ollama:qwen3:8b` works); applies without a restart; *Use* in the Local models panel and `imperium models use` set it |
| `endpoints.<id>.base_url` | — | OpenAI-compatible base URL, ending before `/chat/completions` |
| `endpoints.<id>.api_key` | none | `{ secret = "<name>" }`, read from the keychain when a session starts. *Connect an API endpoint* in the app (`models.add_endpoint`) writes `base_url` and this reference, storing the key as `<id>-api-key` |
| `endpoints.<id>.pricing."<model>"` | none | `{ input_usd_per_mtok, output_usd_per_mtok, cached_input_usd_per_mtok? }`; models without prices report no cost |
| `endpoints.<id>.kind` | `openai` | The API the endpoint speaks, choosing its model client: `openai` (OpenAI-compatible Chat Completions; an endpoint on this machine is also tried for llama.cpp's tokenizer) or `llamacpp` (the same, always using llama.cpp's tokenizer for [context breakdowns](03-agent-engine.md#context-management)) |
| `models.folders` | none | Folders scanned for GGUF models (`llamacpp:<id>`), including LM Studio and Hugging Face cache layouts, and Hugging Face folders (`vllm:<id>`); the `llamacpp` and `vllm` endpoint ids are reserved. Applies and is scanned at once; *Add folder* in the app and `imperium models folders add` append to it |
| `models.suggest_downloads` | `true` | Sessions may offer to download a suitable local model from Hugging Face when the best available one is poor for the task ([model download suggestions](04-models-and-routing.md#model-download-suggestions)); applies without a restart, and a suggestion's "Don't ask again" sets it to `false` in the user file |
| `models.suggestions` | six well-known GGUF repositories (Qwen3 4B and 14B, Qwen2.5-Coder 7B, Qwen3-Coder 30B-A3B, Gemma 3 4B and 12B) | `[[models.suggestions]]` rows suggestions choose from: `repo`, `capabilities` (`chat`, `code`, `tools`, `vision`), `size` (`small`, `medium`, `large`; default `small`), `context` (tokens), and `mmproj` (download the vision projector too); setting any row replaces the built-in list |
| `models.download_dir` | first of `models.folders`, else `<data>/models` | Where [Hugging Face downloads](04-models-and-routing.md#downloading-from-hugging-face) go, as `<owner>/<repo>/` below it; must be absolute; scanned with `models.folders` |
| `runtime.llamacpp.backend` | `auto` | llama.cpp build to install and use: `auto` (the best that runs here: CUDA 13, CUDA 12, Vulkan, CPU; Metal on Apple Silicon), `cpu`, `vulkan`, `cuda12`, `cuda13`, `rocm`, or `metal` ([runtimes](04-models-and-routing.md#runtimes)) |
| `runtime.llamacpp.custom_path` | none | Your own `llama-server` executable, used instead of an installed build; applies to models loaded from then on, without a restart (*Choose llama-server…* in the app, `imperium runtime use`) |
| `runtime.llamacpp.find_existing` | `true` | With no build installed and no `custom_path`, use a `llama-server` already on this machine (on `PATH`, or in `~/.unsloth/llama.cpp` or `~/llama.cpp`) that starts and supports the flags Imperium passes, preferring one that sees the GPU |
| `runtime.llamacpp.context` | `8192` | Tokens one session may use on a local model (`--kv-unified-per-slot` when below the pool) |
| `runtime.llamacpp.parallel` | `2` | Deprecated: only sizes the default KV pool, `pool_tokens = context × parallel`. It no longer caps anything: sessions beyond a model's slots queue |
| `runtime.llamacpp.slots` | the larger of `8` and `parallel` | Default `--parallel` of each model: calls one server decodes at once, `1`-`256`. A CPU-pinned model defaults to `1` |
| `runtime.llamacpp.pool_tokens` | `context × parallel` | Default tokens of KV cache all slots of a model share (`--ctx-size` with `--kv-unified`); at least `context`. The planner counts the KV cache once, for the pool |
| `runtime.llamacpp.cache_type` | `f16` | Default KV cache type of keys and values: `f32`, `f16`, `bf16`, `q8_0`, `q4_0`, `q4_1`, `iq4_nl`, `q5_0`, or `q5_1` |
| `runtime.llamacpp.flash_attn` | `auto` | `--flash-attn`: `auto`, `on`, or `off`; a quantised V cache type needs `on` or `auto` |
| `runtime.llamacpp.max_output_tokens` | `8192` | Most tokens one local reply may generate before it is retried once without a cap; `0` = uncapped (the call reserves the whole context). See [runtimes](04-models-and-routing.md#runtimes) |
| `runtime.llamacpp.models."<id>".*` | none | Settings of one local model, overriding the defaults above: `context`, `pool_tokens` (at least `context`), `slots`, `cache_type_k`, `cache_type_v`, `flash_attn`, `max_gpu_layers` (cap on the layers placed on the GPU), `max_wait_s`, `max_output_tokens`, and `extra_args` (added after `runtime.llamacpp.extra_args`; may not contain a flag Imperium sets, such as `--ctx-size`, with or without `=value`). A CPU-lane row's `parallel`, `context` and `max_wait_s` count too; a row and this table setting different values are refused. Changing a loaded model's settings restarts it once nothing is using it; `max_wait_s` and `max_output_tokens` apply without a restart. Model ids may contain dots: write them as quoted keys |
| `models.overrides."<id>".*` | none | What the app changes on a model-table row: `enabled`, `tier`, `tags`, `context_window`, and both `input_usd_per_mtok` and `output_usd_per_mtok` (the endpoint price for that model). An id that names no row is ignored. `enabled` is also accepted on `[[models.catalog]]` rows; `false` leaves the row out of routing |
| `runtime.llamacpp.idle_unload_s` | `900` | Seconds an unused model stays loaded |
| `runtime.llamacpp.ram_headroom_mb` | `2048` | System memory kept free when placing models; the one RAM budget is free RAM less this, shared by GPU models' RAM share, every server's `--cache-ram`, and models pinned to the CPU lane (their weights and KV) |
| `runtime.llamacpp.cache_ram_mb` | `8192` | Host memory (MiB) for llama.cpp's prompt cache (`--cache-ram`), which restores the prompt of an agent that lost its slot to another instead of reading it again; capped to the RAM left in the budget after the model and the models that stay loaded beside it (and room for a model swapped out only because of a cache), counted as RAM held by the model, `0` turns it off |
| `runtime.llamacpp.swap_cache_mb` | `8192` | Disk (MiB) under `<data>/swap` for the slots of models [swapped out](04-models-and-routing.md#runtimes) to make room for another, restored when they come back; only slots of agents that can come back (mid-turn or waiting for subagents) are saved, and past the limit the files of agents that cannot go first, then the oldest of the session holding the most; `0` turns saving off |
| `runtime.llamacpp.min_residency_s` | `30` | How long a model needing room waits for an agent mid-turn on a loaded model to finish or start waiting, and how long that model stays loaded before it may be swapped out; a model nobody is mid-turn on is swapped out at once |
| `runtime.llamacpp.max_swaps_per_min` | `6` | Swaps allowed per minute; beyond it, a call for a model that does not fit beside the loaded ones waits for the minute to move on (within `max_wait_s`), and routing treats such models as unavailable; `0` turns swapping off, and such a call fails at once |
| `runtime.llamacpp.swap_gain` | `2.0` | Idle models are swapped out for a model that fits beside them only when it gets more than this many times the GPU memory alone; `1.0` swaps whenever it gets more, so subagents run fully on the GPU; must be at least `1.0` |
| `runtime.llamacpp.max_wait_s` | `600` | Longest a call waits for a slot on a local model or for room to load it (behind models in use, or for the swap limit's minute) before it fails with an error naming this setting |
| `runtime.llamacpp.extra_args` | none | Extra `llama-server` arguments, for example `["--flash-attn", "on"]` |
| `hardware.vram_mb`, `hardware.ram_mb` | probed | Override the hardware probe |
| `runtime.vllm.command` | `vllm` | The `vllm` executable for Hugging Face model folders (`vllm:<id>`; Linux and WSL2) |
| `runtime.vllm.max_gpu_memory_utilization`, `extra_args` | `0.9`, none | Cap on vLLM's share of GPU memory; extra `vllm serve` arguments |
| `mcp.servers.<name>` | none | An MCP server: `command`, `args`, `env` for stdio, or `url`, `headers`, `bearer_token` for HTTP; `allow` (tools that run without asking), `enabled`, `timeout_s` (default 60); see [MCP](05-tools-and-isolation.md#mcp) |
| `mcp.trusted_projects` | none | Project folders whose `.mcp.json` servers may start |
| `extensions.plugin_dirs` | none | More plugin folders (Claude Code layout), besides `<config>/plugins` |
| `memory.enabled` | `true` | Global and project memory ([17](17-memory.md)) |
| `memory.write.global`, `.project`, `.project_private` | `ask`, `ask`, `auto` | Whether agents may save memories in each scope: `auto`, `ask`, or `off` |
| `memory.recall` | `true` | Put the best-matching memories before each message |
| `memory.max_index_tokens`, `memory.max_recall_tokens` | `1500`, `1500` | Budgets for the index in agent instructions and for recalled memories |
| `workflow.enabled` | `true` | Serve workflow packs and project instruction files to agents ([18](18-workflow-contract.md)) |
| `workflow.packs` | none | Workflow folders, such as a clone of Agent-Custom-Workflow |
| `workflow.default_pack` | `true` | With no `packs`, use the bundled Agent-Custom-Workflow snapshot |
| `workflow.enforce` | `warn` | `required-before-action` refuses writes and commands until required context is loaded; `warn` and `off` do not |
| `<config>/mods/<id>/` | none | UI mods (folders with `mod.json`), shown as panels; see [UI mods](06-extensibility.md#ui-mods) |
| `schedules` | none | `[[schedules]]` with `id`, `trigger` (`cron`, `every`, or `at`), `action` (`project` and one of `prompt`, `command`, or `agent` (a message for the project agent), optional `model`, `timeout_s`), `missed`, `overlap`, `notify`, `enabled`, `prefer_quiet` (default `false`: hold a due run until the machine is quiet, for at most `max_defer`, default `2h`, then run anyway) and `conditions = { when_quiet = true }` (wait for a quiet machine however long it takes); see [schedules](14-projects-and-automation.md#schedules) |
| `web.enabled` | `true` | Offer `web_fetch` (and `web_search` with a provider) to agents |
| `web.search` | `none` | `searxng` (with `web.searxng_url`), `brave`, or `tavily` (with `web.api_key = { secret = "<name>" }`) |
| `web.allow_domains` | none | Domains `web_fetch` reads without asking (subdomains included) |
| `web.browser`, `web.browser_path`, `web.headless` | `true`, found automatically, `true` | Browser tools over CDP (Chrome, Chromium, or Edge) with their own profile |
| `media.image` | none | `backend = "openai"` with `endpoint` and `model`, or `backend = "comfyui"` with `comfyui_url` and `workflow`; see [media](09-media-generation.md) |
| `media.speech`, `media.transcription` | none | `endpoint` (an `[endpoints]` entry) and `model`; speech also takes `voice` |
| `phone.bind` | off | Address to serve paired phones on, such as a Tailscale address with a port; see [mobile](15-mobile-and-remote.md) |
| `pets.enabled`, `pets.pet` | `true`, first found | The pet and which one ([16](16-pets.md)) |
| `pets.folders`, `sleep_after_minutes`, `fidget` | none, `5`, `true` | More folders of Pixel Pets v3 packages; when it sleeps; idle fidgets |
| `isolation.tier` | `policy` | Where agents' commands run: `policy` (tier 0), `os` (bubblewrap or Seatbelt), or `container` ([tiers](05-tools-and-isolation.md#isolation-tiers)) |
| `isolation.container_runtime`, `container_image`, `network` | `docker`, `ubuntu:24.04`, `true` | Container tier settings; `network = false` cuts sandboxed and container commands off the network |
| `permissions.grants`, `permissions.rules` | none | Path grants outside environments (`{ path, mode }`, mode `read` or `read_write`) and allow / ask / deny rules (`{ tool, pattern, action }`) for every environment; edited in Settings → Permissions ([permissions](05-tools-and-isolation.md#permissions)) |
| `projects` | none | `[[projects]]` with `path`, `agent` (run a project agent), `agent_model`, `max_sessions` (2), `max_sessions_per_day` (20), `prefer_quiet` (default `true`: the agent's sessions get their brief only when the machine is quiet, for at most `max_defer`, default `2h`) |
| `load.sample_s`, `load.settle_s` | `5`, `60` | How often (seconds) the daemon looks at the machine for the [load view](14-projects-and-automation.md#quiet-periods), and how long it must look calmer before the level drops (so a pause between two of your messages is not quiet) |
| `load.busy_cpu_pct`, `load.saturated_cpu_pct` | `70`, `95` | Overall CPU use (percent) at which the machine counts as busy, and as saturated; not counted while a background session is working |
| `load.busy_free_ram_mb`, `load.saturated_free_ram_mb` | `1024`, `512` | Available memory (MiB) below which the machine counts as busy, and as saturated; keep the busy value below `runtime.llamacpp.ram_headroom_mb` (2048), which model placement leaves free, or a machine with models loaded would never read quiet |
| `load.saturated_swaps_per_min` | `3` | Local models swapped out in the last minute at which the machine counts as saturated; `0` ignores swaps |
| `load.interactive_hold_min` | `5` | Minutes after one of your sessions last used a local model during which background work may not swap that model out |
| `github.watch` | none | `[[github.watch]]` with `repo` (`owner/name`) and `project` (a local clone whose `origin` is that repository) |
| `github.autofix.enabled` | `false` | Fix the user's pull requests whose checks fail ([auto-fix](14-projects-and-automation.md#github-auto-fix)) |
| `github.autofix.push` | `ask` | `ask` queues an approval before each push; `auto` pushes without asking |
| `github.autofix.max_attempts_per_pr`, `poll_s`, `model` | `3`, `120`, default model | Stop after this many failing commits per pull request; polling interval; model for fix sessions |
| `hooks` | none | `[[hooks]]` entries with `event`, optional `matcher`, `command`, and `timeout_s`; see [hooks](06-extensibility.md#hooks) |

Configuration is read when the daemon starts; restart it after editing.

## Secrets

API keys, OAuth tokens, and pairing keys are stored in the OS keychain (Windows Credential
Manager, macOS Keychain, Secret Service on Linux) through the `keyring` crate. Config files
reference secrets by name only:

```toml
[endpoints.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key = { secret = "openrouter" }   # resolved from the keychain
```

A Hugging Face token for gated or private models is read from `HF_TOKEN`, or else from the
secret named `huggingface`.

Secret names are 1–64 characters of lowercase letters, digits, `.`, `_`, and `-`, starting
with a letter or digit. Today `imperium secret set <name>` (value from standard input, or a
prompt without echo) and `imperium secret delete <name>` write the keychain directly; the
`secret.*` protocol methods arrive with the desktop settings UI.

Headless Linux without a Secret Service will fall back to an encrypted file unlocked by a
passphrase (not yet implemented; today such systems report a keychain error). Secrets are never written to logs, transcripts, telemetry, or exported sessions,
and the protocol exposes `secret.set` / `secret.delete` but no read.

## Example

```toml
[ui]
scale = 1.25
font_scale = { prose = 1.1, code = 1.0 }
density = "compact"
max_prose_width_ch = 90

[models]
folders = ["D:/Models", "E:/LLM/gguf", "ssh://lab/models"]
default_root = "openrouter:anthropic/claude-sonnet"

[endpoints.local-vllm]
kind = "openai"
base_url = "http://192.168.1.20:8000/v1"

[runtime.llamacpp]
backend = "auto"          # auto | cpu | cuda12 | cuda13 | vulkan | rocm | metal
custom_path = ""
find_existing = true      # use a llama-server already on this machine until one is installed
idle_unload_s = 900
context = 8192            # tokens one session may use
slots = 8                 # calls one server decodes at once; no cap on sessions
pool_tokens = 65536       # KV cache all slots share

# What the desktop's model settings dialog writes to <config>/models.toml:
[runtime.llamacpp.models."Qwen3-8B-Q4_K_M"]
context = 32768
slots = 4
cache_type_k = "q8_0"
cache_type_v = "q8_0"

[models.overrides.coder]
enabled = false

[routing]
prefer_local = true
max_session_cost_usd = 5.0

[agents]
max_depth = 3
max_concurrent = 8        # replaces local-paths/plan.md
manager_pool = "excluded" # replaces local-paths/pool-managers.md

[isolation]
default_tier = "policy"   # policy | os | container | remote
exec_from_hosted_models = "container"

[worktrees]                # full options: 14-projects-and-automation.md#worktrees
mode = "per-session"
location = "data"

[policy]
pack = "builtin:agent-custom-workflow"
personality = "luxion"

[telemetry]
otlp_endpoint = ""
prometheus = false
```

## Other recommended options

- Per-endpoint rate limits, timeouts, retry policy, and monthly spend caps with alerts.
- Model aliases (`fast`, `smart`, `vision`) usable in routing rules and prompts.
- Proxy settings and custom CA certificates.
- Transcript retention period and automatic redaction rules.
- Import/export of the whole configuration (secrets excluded) for moving between machines.
