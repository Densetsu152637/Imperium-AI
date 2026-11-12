# 10 — Configuration

## Layers

Later layers override earlier ones, key by key:

1. Built-in defaults
2. User config: `<config>/config.toml`
   (2a. App settings: `<config>/models.toml`, written by the desktop app; see below)
3. Host config (SSH host's own `config.toml`)
4. Project config: `<project>/.imperium/config.toml` (shareable) and
   `<project>/.imperium/config.local.toml` (git-ignored). **Narrow-only, not a plain layer:** the
   files belong to the repository, so they are never merged as settings. They are read through an
   allow-list and checked against layers 1 to 3 so they can only make things stricter (see the
   environment layer below)
5. Session overrides
6. Environment variables (`IMPERIUM_*`) and CLI flags

Implemented so far (phase 0): defaults, user config, app settings, and environment variables. A session's settings add an **environment layer** after these: the
environment's own `.imperium/config.toml` (first folder) checked against the user's settings so it
can only narrow them, never run a program, and start servers or plugins only for a folder in
`mcp.trusted_projects` ([14](14-projects-and-automation.md#environment-configuration-implemented)).
`imperium env config <env>` shows what it changes. Host config, session overrides, and CLI flags arrive with the features that need
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
  that set it (for example `set by environment variable IMPERIUM_DAEMON__PORT`). With
  `--project` it also lists what the project's own `.imperium` files apply and refuse as
  narrowing; they are never shown as the layer that set a value. `imperium workflow` ignores the
  project's files for the same reason (a repository cannot name workflow packs to trust).
- `imperium config set <key> <value>...` and `imperium config unset <key>` change a setting from
  the same table the app uses (below), such as `imperium config set policy.personality luxion`.
  With a daemon running they go through `config.set`, so settings it applies at once take
  effect; otherwise they write the user file directly.

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
  `restart: false` (`models.suggest_downloads`, `models.default`, `models.effort`, `models.folders`,
  `runtime.llamacpp.custom_path`, `policy.personality`, and `workspaces.root`, which moves the working folders first) apply at once; a changed `models.folders` is scanned again
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
| `daemon.socket` | `false` | Also serve the daemon on a local socket only your user can open (a Unix domain socket under `<data>/ipc/`, or a Windows named pipe), with the same protocol and token, so an SSH forward can reach it ([02](02-architecture.md#protocol)). Needs a daemon restart |
| `api.openai_compatible.enabled` | `false` | Serve the local models (and the hosted ones in `expose`) to other tools as an OpenAI-compatible API ([04](04-models-and-routing.md#openai-compatible-local-api-implemented)); starts and stops at once, also through `imperium api enable` and `api.set` |
| `api.openai_compatible.port` | `0` | Port of the API server. `0` means none is chosen yet: the first enable picks a free one (from 11500 up), saves it here, and keeps it |
| `api.openai_compatible.bind` | `127.0.0.1` | Address the API server listens on. A loopback address (`127.0.0.1`, `::1`, `localhost`) unless `allow_lan` is set; any other is refused and the server does not start |
| `api.openai_compatible.allow_lan` | `false` | Lets `bind` be an address other computers can reach. Risky: the API is plain HTTP, so anyone on the network who learns the API token can use your models and the hosted ones exposed |
| `api.openai_compatible.max_concurrent` | `4` | Most [API](04-models-and-routing.md#openai-compatible-local-api-implemented) calls in flight at once (running, or waiting for a model); one more is refused with a `429` `rate_limit_exceeded` error. Applies at once |
| `api.openai_compatible.expose` | none | Specs (`<endpoint>:<model>`) of hosted model-table rows the API also serves, spending your keys and money; nothing hosted is served unless listed. Local models need no entry |
| `agents.max_steps` | `50` | Model calls an agent may make in one turn before it stops |
| `resume.mode` | `wait_for_reset` | When a provider's usage runs out: `wait_for_reset`, `at_time`, `retry_every`, or `stop` (each session can override it) |
| `resume.at_time`, `retry_every_minutes`, `max_wait_hours` | none, `5`, `12` | Local `HH:MM` for `at_time`; retry interval; longest wait in one turn |
| `agents.resume_days` | `7` | Sessions active within this many days continue after the daemon restarts |
| `context.auto_compact_at` | `0.85` | Compact an agent's conversation mid-turn when the next model call would read this share of its model's context window; `0` never does, and anything from 0 up to 0.5 means 0.5. Needs a known window; the system prompt and loaded workflow context are never compacted ([mid-turn compaction](03-agent-engine.md#context-management)) |
| `sessions.auto_archive_on_pr_close` | `true` | Archive a session once the pull request it watches is merged or closed ([archiving sessions](14-projects-and-automation.md#archiving-sessions)) |
| `sessions.agent_state_keep` | `true` | Keep each agent's state (a subagent's conversation, plan, and usage) in `<data>/sessions/<id>.agents/`, so finished subagents can be resumed with a follow-up, also after a restart; deleted with the session ([agent state](03-agent-engine.md#agent-state)) |
| `sessions.auto_archive_after_days` | `0` | Archive an idle session after this many days without activity; `0` never does. Working, waiting, pinned, and agent sessions are never archived |
| `sessions.checkpoints` | `true` | Before each root turn that changes files in a git repository, record the session's files as a commit on a private ref (`refs/imperium/checkpoints/<session>/<n>`), without touching the index, HEAD, or branches, so `session.rewind` can put the files and the conversation back ([checkpoints](14-projects-and-automation.md#checkpoints-and-rewinding)) |
| `sessions.attachment_max_mib` | `10` | The largest file that can be attached to a message, in MiB, checked when it is attached and again when the message is sent. Uploads from the app (pasted files, browser drops) travel over the daemon connection, which carries about 12 MiB of file at most ([attachments](07-desktop-ui.md#attachments-and-voice-input-implemented)) |
| `workspaces.root` | `<data>/workspaces` | The folder that holds a working folder for every environment: its `Memory.md` and one scratch folder per session, deleted when the session is archived ([environment working folders](14-projects-and-automation.md#environment-working-folders-implemented)). Must be absolute. Changing it (Settings → *Working folders*, `imperium config set workspaces.root`, `imperium workspaces move`) moves the existing folders there first and refuses a folder that is not empty or is inside the current one; applies at once |
| `models.catalog` | none | The model table: `[[models.catalog]]` rows with `id`, `spec`, `location` (`local` or `cloud`), `tier` (`basic`, `standard`, `strong`, `frontier`), optional `alias` (a [display name](#model-aliases)), optional `tags` (`reasoning` or `thinking` mark a model that can reason, `no-reasoning` or `no-thinking` one that cannot), optional `reasoning` (`true` or `false`: whether the model can reason, so sessions send it their [reasoning effort](04-models-and-routing.md#reasoning-effort); over the tags, a local model's chat template and the model's name), and optional `context_window` (tokens per request, for [context breakdowns](03-agent-engine.md#context-management); llama.cpp models report their own), optional `placement` (`auto` by default, or `cpu` to pin a `location = "local"`, `llamacpp:<id>` row to the [CPU lane](04-models-and-routing.md#runtimes): served with no GPU layers, in system RAM only, never swapped out for the GPU; a cloud row or another spec is refused at startup), and, only valid with `placement = "cpu"`, optional `parallel` (slots, default `1`), `context` (tokens per slot, default `runtime.llamacpp.context`) and `max_wait_s` (longest a call waits for a slot or for room on that row, default `3600` or `runtime.llamacpp.max_wait_s` if longer); a CPU-pinned model is started with `--n-gpu-layers 0 --device none` (plus `--no-mmproj-offload` with a projector), and rows naming the same model must agree on these; see [routing](04-models-and-routing.md#routing) |
| `routing.prefer` | `cheapest` | Tie-breaker among suitable models: `cheapest`, `local`, or `cloud` |
| `routing.rules` | none | `[[routing.rules]]` delegation rules with optional `when_tier`, `when_tag`, `location`, `tier` (raises the task's minimum tier), and `use` (model table ids in order) |
| `routing.builtin_rules` | `true` | Whether the built-in [task-class rules](04-models-and-routing.md#task-classes) (hard tags prefer cloud `frontier` rows, menial tags prefer local rows at the task's own tier) apply when none of `routing.rules` matched |
| `models.default` | none | Model for new sessions, as `<endpoint>:<model>` (split at the first `:`, so `ollama:qwen3:8b` works); applies without a restart; *Use* in the Local models panel and `imperium models use` set it |
| `models.effort` | `medium` | [Reasoning effort](04-models-and-routing.md#reasoning-effort) new sessions start with: `low`, `medium`, `high` or `max`; each session changes its own (`session.control` `set_effort`, `/effort` in the CLI, the selector beside the model in the app). Sent only to models that can reason; applies without a restart, to sessions created from then on |
| `endpoints.<id>.base_url` | — | OpenAI-compatible base URL, ending before `/chat/completions` |
| `endpoints.<id>.api_key` | none | `{ secret = "<name>" }`, read from the keychain when a session starts. *Connect an API endpoint* in the app (`models.add_endpoint`) writes `base_url` and this reference, storing the key as `<id>-api-key` |
| `endpoints.<id>.pricing."<model>"` | none | `{ input_usd_per_mtok, output_usd_per_mtok, cached_input_usd_per_mtok? }`; models without prices report no cost |
| `endpoints.<id>.kind` | `openai` | The API the endpoint speaks, choosing its model client: `openai` (OpenAI-compatible Chat Completions; an endpoint on this machine is also tried for llama.cpp's tokenizer) or `llamacpp` (the same, always using llama.cpp's tokenizer for [context breakdowns](03-agent-engine.md#context-management)) |
| `models.folders` | none | Folders scanned for GGUF models (`llamacpp:<id>`), including LM Studio and Hugging Face cache layouts, and Hugging Face folders (`vllm:<id>`); the `llamacpp` and `vllm` endpoint ids are reserved. Applies and is scanned at once; *Add folder* in the app and `imperium models folders add` append to it |
| `models.suggest_downloads` | `true` | Sessions may offer to download a suitable local model from Hugging Face when the best available one is poor for the task ([model download suggestions](04-models-and-routing.md#model-download-suggestions)); applies without a restart, and a suggestion's "Don't ask again" sets it to `false` in the user file |
| `models.hidden` | none | Specs of the models the user hid (`llamacpp:<id>` for a local model); the desktop app keeps it in step with its hidden marks. Agents' `list_models` leaves them out, and a hidden local model outside the model table is not offered for delegation by name ([model awareness](04-models-and-routing.md#model-awareness-and-model-cards-implemented)) |
| `models.list_models` | `root` | Which agents of new sessions get `list_models`: `root`, `all` (subagents too), or `off`; applies without a restart |
| `models.suggestions` | six well-known GGUF repositories (Qwen3 4B and 14B, Qwen2.5-Coder 7B, Qwen3-Coder 30B-A3B, Gemma 3 4B and 12B) | `[[models.suggestions]]` rows suggestions choose from: `repo`, `capabilities` (`chat`, `code`, `tools`, `vision`), `size` (`small`, `medium`, `large`; default `small`), `context` (tokens), and `mmproj` (download the vision projector too); setting any row replaces the built-in list |
| `models.download_dir` | first of `models.folders`, else `<data>/models` | Where [Hugging Face downloads](04-models-and-routing.md#downloading-from-hugging-face) go, as `<owner>/<repo>/` below it; must be absolute; scanned with `models.folders` |
| `runtime.llamacpp.backend` | `auto` | llama.cpp build to install and use: `auto` (the best that runs here: CUDA 13, CUDA 12, Vulkan, CPU; Metal on Apple Silicon), `cpu`, `vulkan`, `cuda12`, `cuda13`, `rocm`, or `metal` ([runtimes](04-models-and-routing.md#runtimes)) |
| `runtime.llamacpp.custom_path` | none | Your own `llama-server` executable, used instead of an installed build; applies to models loaded from then on, without a restart (*Choose llama-server…* in the app, `imperium runtime use`) |
| `runtime.llamacpp.context` | `"auto"` | Most tokens one agent may use on a local model: `"auto"` (what the model was trained for, capped to what fits in memory beside its weights with every layer on the GPU when the weights fit) or a number of tokens. Compaction is planned against it. See [runtimes](04-models-and-routing.md#runtimes) |
| `runtime.llamacpp.parallel` | `2` | Deprecated: with a fixed `context`, the starting KV pool is `context × parallel` (at most 16384). It caps nothing: sessions beyond a model's slots queue |
| `runtime.llamacpp.slots` (or `max_slots`) | the larger of `16` and `parallel` | Default `--parallel` of each model: most calls one server decodes at once, `1`-`256`. Slots share one KV pool and reserve none of their own, so the ceiling is high and the pool never grows with it. A CPU-pinned model defaults to `1` |
| `runtime.llamacpp.pool_tokens` | none | Fixes the tokens of KV cache all slots of a model share (`--ctx-size` with `--kv-unified`), at least `context`; it never grows, and is cut to what fits beside the weights on the GPU when it does not. Unset, the pool starts at 16384 tokens (or what the first call needs) and grows on demand up to the context. The planner counts the KV cache once, for the pool |
| `runtime.llamacpp.find_existing` | `true` | With no build installed and no `custom_path`, use a `llama-server` already on this machine (on `PATH`, package managers, LM Studio, fixed llama.cpp checkout folders; [where](04-models-and-routing.md#runtimes)) that starts and supports the flags Imperium passes, preferring one that sees the GPU. Unpacked downloads in the home or Downloads folder are only listed as candidates, never run until adopted with `custom_path` |
| `runtime.llamacpp.auto_install` | `true` | With no `custom_path` and no usable `llama-server` (or only a CPU build on a machine with a GPU, or Imperium's build of an earlier release), download and verify llama.cpp's official build for this machine the first time a local model is needed ([runtimes](04-models-and-routing.md#runtimes)) |
| `runtime.llamacpp.update_external` | `ask` | A `llama-server` installed with winget, scoop, Chocolatey, or Homebrew that is older than the pinned release: `off`, `ask` (offer *Update*), or `auto` (run the package manager's upgrade while no model is loaded from it) ([updates](04-models-and-routing.md#llamacpp-updates)) |
| `runtime.llamacpp.cache_type` | `f16` | Default KV cache type of keys and values: `f32`, `f16`, `bf16`, `q8_0`, `q4_0`, `q4_1`, `iq4_nl`, `q5_0`, or `q5_1` |
| `runtime.llamacpp.flash_attn` | `auto` | `--flash-attn`: `auto`, `on`, or `off`; a quantised V cache type needs `on` or `auto` |
| `runtime.llamacpp.max_output_tokens` | `8192` | Most tokens one local reply may generate before it is retried once without a cap; `0` = uncapped (the call reserves the whole context). See [runtimes](04-models-and-routing.md#runtimes) |
| `runtime.llamacpp.models."<id>".*` | none | Settings of one local model, overriding the defaults above: `alias` (a [display name](#model-aliases); never restarts the model), `context` (`"auto"` or tokens), `pool_tokens` (fixes the pool; at least `context`), `slots`, `cache_type_k`, `cache_type_v`, `flash_attn`, `max_gpu_layers` (cap on the layers placed on the GPU), `max_wait_s`, `max_output_tokens`, `reasoning` (`true` or `false`, over what the chat template says about [reasoning](04-models-and-routing.md#reasoning-effort); never restarts the model), the [speculative decoding](04-models-and-routing.md#speculative-decoding-implemented) keys `draft_model` (the id of an installed, smaller GGUF model with the same tokenizer family; unset = no draft), `draft_max` (tokens the draft proposes per step, at least 1) and `draft_min` (at least 0, at most the maximum in effect: `draft_max`, or llama.cpp's default of 3 when unset); unset keeps llama.cpp's defaults, and `draft_gpu_layers` (`0` keeps the draft in RAM, a smaller number than its layers splits it; unset is llama.cpp's `auto`, planned as all on the GPU), where the draft keys other than `draft_model` need `draft_model`, a model may not be its own draft, and a draft that is missing, incompatible, or does not fit leaves the model running without it, with a notice, and `extra_args` (added after `runtime.llamacpp.extra_args`; may not contain a flag Imperium sets, such as `--ctx-size` or the draft model flags, with or without `=value`; the global `runtime.llamacpp.extra_args` are checked for the draft flags only, so older global flags such as `--flash-attn on` keep working). **Breaking, draft flags only:** an `extra_args` that passes `-md`, `--model-draft`, `--draft`, `--draft-max`, `--draft-min`, `-ngld` or another draft flag (any `--spec-draft-*`, `--draft*`, `--*-draft`, `-hfd`, `-ctkd`, `-devd`, `-td` and the like), in a model's or the global `extra_args`, is now refused when the settings load; configure the draft with `draft_model` and its keys instead. A CPU-lane row's `parallel`, `context` and `max_wait_s` count too; a row and this table setting different values are refused. Changing a loaded model's settings restarts it once nothing is using it; `max_wait_s` and `max_output_tokens` apply without a restart. Model ids may contain dots: write them as quoted keys |
| `models.overrides."<id>".*` | none | What the app changes on a model-table row: `alias` (a [display name](#model-aliases), over the row's own), `enabled`, `tier`, `tags`, `context_window`, and both `input_usd_per_mtok` and `output_usd_per_mtok` (the endpoint price for that model). An id that names no row is ignored. `enabled` is also accepted on `[[models.catalog]]` rows; `false` leaves the row out of routing |
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
| `extensions.lazy` | `true` | Load skills and plugins on first use: agents start with only each one's name and description ([06](06-extensibility.md#lazy-skills-and-plugins-implemented)). `false` puts every skill's instructions in the prompt and starts every plugin's servers with the session |
| `extensions.eager` | none | Skills and plugins, by name, loaded up front even when `extensions.lazy` is on |
| `memory.enabled` | `true` | Global and project memory ([17](17-memory.md)) |
| `memory.write.global`, `.project`, `.project_private` | `ask`, `ask`, `auto` | Whether agents may save memories in each scope: `auto`, `ask`, or `off` |
| `memory.write.environment` | `ask` | Whether agents may write to an environment's `Memory.md`, which every later session there reads as part of its instructions, so a write shows the whole entry for approval by default ([17](17-memory.md#environment-memory-implemented)) |
| `memory.recall` | `true` | Put the best-matching memories before each message |
| `memory.max_index_tokens`, `memory.max_recall_tokens` | `1500`, `1500` | Budgets for the index in agent instructions and for recalled memories |
| `memory.session_search` | `true` | Index sessions in the background for `session.search`, and offer agents `search_sessions` ([17](17-memory.md#session-search-implemented)) |
| `memory.session_search_scope` | `environment` | Which sessions an agent's `search_sessions` reaches: `environment` (its own environment, or outside any, its folder) or `all` |
| `memory.embedding_model` | empty | Model table id or `<endpoint>:<model>` of an embedding model on an OpenAI-compatible endpoint, for search by meaning; empty searches by keyword only |
| `workflow.enabled` | `true` | Serve workflow packs and project instruction files to agents ([18](18-workflow-contract.md)) |
| `workflow.packs` | none | Workflow folders, such as a clone of Agent-Custom-Workflow |
| `workflow.default_pack` | `true` | With no `packs`, use the bundled Agent-Custom-Workflow snapshot |
| `policy.personality` | `marcus_aurelius` | The root agent's personality, by id: `<config>/personalities/<id>.md` wins over a workflow pack's `personalities/<id>.md`, then the bundled one. `none` (or empty) turns it off; an id that is not found falls back to `marcus_aurelius` with a notice in the session. Applies to new sessions without a restart ([personalities](08-agent-policy.md#personalities)) |
| `workflow.enforce` | `warn` | `required-before-action` refuses writes and commands until required context is loaded; `warn` and `off` do not |
| `<config>/mods/<id>/` | none | UI mods (folders with `mod.json`), shown as panels; see [UI mods](06-extensibility.md#ui-mods) |
| `schedules` | none | `[[schedules]]` with `id`, `trigger` (`cron`, `every`, or `at`), `action` (`project` and one of `prompt`, `command`, or `agent` (a message for the project agent), optional `model`, `timeout_s`), `missed`, `overlap`, `notify`, `enabled`, `prefer_quiet` (default `false`: hold a due run until the machine is quiet, for at most `max_defer`, default `2h`, then run anyway) and `conditions = { when_quiet = true }` (wait for a quiet machine however long it takes); see [schedules](14-projects-and-automation.md#schedules) |
| `web.enabled` | `true` | Offer `web_fetch` (and `web_search` with a provider) to agents |
| `web.search` | `none` | `searxng` (with `web.searxng_url`), `brave`, or `tavily` (with `web.api_key = { secret = "<name>" }`) |
| `web.allow_domains` | none | Domains `web_fetch` reads without asking (subdomains included) |
| `web.browser`, `web.browser_path`, `web.headless` | `true`, found automatically, `true` | Browser tools over CDP (Chrome, Chromium, or Edge) with their own profile |
| `media.image` | none | `backend = "openai"` with `endpoint` and `model`, or `backend = "comfyui"` with `comfyui_url` and `workflow`; see [media](09-media-generation.md) |
| `media.speech`, `media.transcription` | none | `endpoint` (an `[endpoints]` entry) and `model`; speech also takes `voice`. `media.transcription` also serves push-to-talk in the app ([voice input](07-desktop-ui.md#attachments-and-voice-input-implemented)) |
| `phone.bind` | off | Address to serve paired phones on, such as a Tailscale address with a port; see [mobile](15-mobile-and-remote.md) |
| `pets.enabled`, `pets.pet` | `true`, first found | The pet and which one ([16](16-pets.md)) |
| `pets.folders`, `sleep_after_minutes`, `fidget` | none, `5`, `true` | More folders of Pixel Pets v3 packages; when it sleeps; idle fidgets |
| `isolation.tier` | `policy` | Where agents' commands run: `policy` (tier 0), `os` (bubblewrap or Seatbelt), or `container` ([tiers](05-tools-and-isolation.md#isolation-tiers)) |
| `isolation.container_runtime`, `container_image`, `network` | `docker`, `ubuntu:24.04`, `true` | Container tier settings; `network = false` cuts sandboxed and container commands off the network |
| `permissions.mode` | `default` | The [permission mode](05-tools-and-isolation.md#permission-modes-implemented) sessions people start begin in: `ask` (ask before anything not read-only, ignoring allow rules), `default` (the rules decide), `accept_edits` (file edits run without asking), `plan` (read and plan only), or `full` (everything runs except what deny rules or the safety floor stop; the floor is a best-effort guard against destructive commands and secret-file writes that applies everywhere, unattended sessions' pre-approved tools included, but is not a security boundary). Each session changes its own (`session.control` `set_permission_mode`, `/mode` in the CLI, the selector in the session footer); unattended sessions (schedules, project agents, auto-fix) start in `default`; ask rules ask every time in every mode. Applies without a restart, to sessions created from then on |
| `permissions.grants`, `permissions.rules` | none | Path grants outside environments (`{ path, mode }`, mode `read` or `read_write`) and allow / ask / deny rules (`{ tool, pattern, action }`) for every environment; edited in Settings → Permissions ([permissions](05-tools-and-isolation.md#permissions)) |
| `budgets.week_start`, `budgets.month_start_day`, `budgets.thresholds` | `monday`, `1`, `[50, 80, 100]` | When weekly budgets start (a day name) and monthly ones reset (1 to 28), at local midnight; the percentages that alert for budgets without their own ([budgets](#budgets)) |
| `budgets.limits` | none | Token and cost budgets (`{ id, name, scope, target, window, tokens, cost_usd, thresholds, action }`); edited in Settings → Budgets ([budgets](#budgets)) |
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

## Budgets

Budgets put limits on model usage over a window and alert as it fills up. The daemon records
every model call a session makes (subagents included) by local day, environment, and model spec
in `<data>/usage.json`, keeping 400 days, so windows and alerts survive restarts.

```toml
[budgets]
week_start = "monday"     # weekly windows start at local midnight on this day
month_start_day = 1       # monthly windows reset on this day of the month (1 to 28)
thresholds = [50, 80, 100]
limits = [
  { id = "all-month", scope = "global", window = "monthly", cost_usd = 50.0, action = "ask" },
  { id = "work-week", name = "Work", scope = "environment", target = "<environment id>", window = "weekly", tokens = 5000000, action = "block" },
  { id = "router-day", scope = "endpoint", target = "openrouter", window = "daily", cost_usd = 5.0, thresholds = [80, 100] },
  { id = "big-model", scope = "model", target = "openrouter:anthropic/claude-sonnet", window = "monthly", tokens = 20000000 },
]
```

- **Scope:** `global` counts every call; `environment` the calls of sessions in one environment
  (`target` is its id); `endpoint` the calls to one endpoint (`target` is its name, such as
  `openrouter` or `llamacpp`); `model` the calls to one model (`target` is its spec,
  `<endpoint>:<model>`); `source` the calls that come through an entry point rather than from
  a session (`target` is `api`, the [OpenAI-compatible API](04-models-and-routing.md#openai-compatible-local-api-implemented),
  whose calls also count toward `global`, `endpoint`, and `model` budgets). A used-up `source`
  budget that asks or blocks refuses API calls with a `429` error.
- **Window:** `daily`, `weekly`, or `monthly`, in the daemon machine's local time.
- **Limits:** `tokens` counts prompt plus completion tokens; `cost_usd` counts calls to models
  with prices ([pricing](04-models-and-routing.md)). Local models have no price, so they count
  tokens only, unless one is configured for them. With both limits, whichever is further along
  counts. Each budget needs at least one.
- **Alerts:** each threshold (a percentage, default 50, 80, 100) alerts once per window; a call
  that crosses several sends one alert for the highest. Alerts go to every client as
  `budgets.alert`; the desktop shows them as notifications (Settings → Notifications).
- **Action** when a budget is used up: `warn` (the default) only alerts; `ask` makes each new
  turn in the scope ask first (`session.send` fails with `-32008` until it is repeated with
  `confirm_budget`); `block` refuses new turns in the scope until the window resets. The check
  also runs before every model call, every compaction (automatic, manual, and on a model switch)
  and every subagent spawn, against the model that call uses, so a budget used up in the middle
  of a turn stops the next call there (the turn fails with the budget message), a subagent on a
  blocked endpoint or model is refused, and steering a running turn is gated at its next call.
  Compaction summaries are counted in the usage (they are paid calls); the short title calls are
  neither gated nor counted. A budget that asks pauses the agent with a permission request
  (an unattended session's waits in the approvals queue like any other), once per budget, per
  session, and window: parallel subagents raise one prompt, and with several used-up budgets that
  ask, each one is asked. The approval (or `confirm_budget`) is held in the daemon's memory, so a
  restart asks again, and it ends when the window rolls over. It is not skipped by allow rules,
  the `full` permission mode, or pre-approved tools, and declining fails the turn. A call already
  running finishes. Scheduled runs
  and project-agent briefs in a scope that asks or blocks do not start.

`budgets.get` returns the budgets and where each stands, `budgets.set` replaces them (applied at
once; budgets without an id get one), and `budgets.usage` returns usage by day with the top
environments, endpoints, and models for the Usage panel ([07](07-desktop-ui.md#budgets-and-usage-implemented)).

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
secret named `huggingface`. The [OpenAI-compatible API](04-models-and-routing.md#openai-compatible-local-api-implemented)'s
bearer token is the secret `api.openai-token`, made on first use by `api.token`; it is shown
and rotated only there (Settings → API, `imperium api token`) and is never written to the config
file, the data folder, or the logs.

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
auto_install = true       # install the official build the first time a local model needs one
update_external = "ask"   # off | ask | auto: package-manager llama.cpp updates
idle_unload_s = 900
context = "auto"          # most tokens one agent may use, or a number
slots = 16                # most calls one server decodes at once; no cap on sessions
# pool_tokens = 65536     # fixes the KV pool; unset, it grows on demand

# What the desktop's model settings dialog writes to <config>/models.toml:
[runtime.llamacpp.models."Qwen3-8B-Q4_K_M"]
alias = "Qwen 3 (8B)"
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
personality = "marcus_aurelius"   # the default; "none" turns personalities off

[telemetry]
otlp_endpoint = ""
prometheus = false
```

## Model aliases

Any model, local or in the model table, may have an `alias`: a display name, such as
`Qwen 3 (4B)` for `Qwen3-4B-Instruct-2507-Q4_K_M`, that clients show instead of its id. A local
model's alias is `runtime.llamacpp.models."<id>".alias`; a model-table row's is `alias` on its
`[[models.catalog]]` row, or `models.overrides."<id>".alias` (what the desktop's model dialog
writes), which wins. A table row with no alias of its own is shown by the alias of the local model
its spec names. The id is unchanged: config, `[[routing.rules]]`, `spawn_agent`, `--model` and
`models.default` still use ids and specs, and an alias is never accepted in their place. An
alias is trimmed, non-blank, without control characters and at most 80 characters; an invalid
one is refused naming the key. `models.list` and `models.local` report each model's `alias`
(`null` without one), and `imperium models local` prints it under the id.

## Model cards

Any model may have a `card`: what it is good at (coding, writing, research, ...) and notes,
which agents read through `list_models` when they choose a model to delegate to
([model cards](04-models-and-routing.md#model-awareness-and-model-cards-implemented)). It sits
where the alias does: `runtime.llamacpp.models."<id>".card` for a local model, `card` on a
`[[models.catalog]]` row, or `models.overrides."<id>".card` (what the model dialog writes),
which wins. A card may span lines; it must not be blank, have control characters other than line
breaks and tabs, or be longer than 4,000 characters. `models.config` reports each local model's
`metadata_card` (what its header and Hugging Face model card say), which agents read after it.

Agents write cards too, with `set_model_card`: the card goes to the app settings file
(`models.overrides."<id>".card` for a table row, `runtime.llamacpp.models."<id>".card` for a local
model), applies at once, and is recorded in the config change journal with what the file held
before, so Settings → *Changes by agents* or `imperium config undo <id>` puts it back
([06](06-extensibility.md#agents-manage-the-app-config-implemented)).

## Other recommended options

- Per-endpoint rate limits, timeouts, and retry policy (spend caps with alerts are
  [budgets](#budgets)).
- Model aliases (`fast`, `smart`, `vision`) usable in routing rules and prompts.
- Proxy settings and custom CA certificates.
- Transcript retention period and automatic redaction rules.
- Import/export of the whole configuration (secrets excluded) for moving between machines.
