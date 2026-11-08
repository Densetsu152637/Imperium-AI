# 10 — Configuration

## Layers

Later layers override earlier ones, key by key:

1. Built-in defaults
2. User config: `<config>/config.toml`
3. Host config (SSH host's own `config.toml`)
4. Project config: `<project>/.imperium/config.toml` (shareable) and
   `<project>/.imperium/config.local.toml` (git-ignored)
5. Session overrides
6. Environment variables (`IMPERIUM_*`) and CLI flags

Implemented so far (phase 0): defaults, user config, project config, and environment
variables. Host config, session overrides, and CLI flags arrive with the features that need
them.

- **Environment variables** use `IMPERIUM_<SECTION>__<KEY>` with a double underscore between
  levels, for example `IMPERIUM_DAEMON__PORT=8080` sets `daemon.port`. Values are read as TOML
  (`8080`, `true`, `["a"]`) and otherwise as plain strings. Only nested keys can be set this
  way; `IMPERIUM_HOME` is not configuration (it relocates all data directories).
- **Unknown keys are ignored**, so a config written for a newer version still loads; a known
  key with the wrong type is an error naming the problem.
- `imperium config explain <key> [--project <dir>]` shows the effective value and the layer
  that set it (for example `set by environment variable IMPERIUM_DAEMON__PORT`).

The desktop settings UI will edit the same files; every setting will have a schema entry
(published as JSON Schema for editor completion).

### Implemented settings

| Key           | Default | Meaning                                                        |
| ------------- | ------- | -------------------------------------------------------------- |
| `daemon.port` | `0`     | Port of the local WebSocket listener; `0` picks a free port at each start |
| `agents.max_steps` | `50` | Model calls an agent may make in one turn before it stops |
| `resume.mode` | `wait_for_reset` | When a provider's usage runs out: `wait_for_reset`, `at_time`, `retry_every`, or `stop` (each session can override it) |
| `resume.at_time`, `retry_every_minutes`, `max_wait_hours` | none, `5`, `12` | Local `HH:MM` for `at_time`; retry interval; longest wait in one turn |
| `agents.resume_days` | `7` | Sessions active within this many days continue after the daemon restarts |
| `models.catalog` | none | The model table: `[[models.catalog]]` rows with `id`, `spec`, `location` (`local` or `cloud`), `tier` (`basic`, `standard`, `strong`, `frontier`), optional `tags`, and optional `context_window` (tokens per request, for [context breakdowns](03-agent-engine.md#context-management); llama.cpp models report their own); see [routing](04-models-and-routing.md#routing) |
| `routing.prefer` | `cheapest` | Tie-breaker among suitable models: `cheapest`, `local`, or `cloud` |
| `routing.rules` | none | `[[routing.rules]]` delegation rules with optional `when_tier`, `when_tag`, `location`, and `use` (model table ids in order) |
| `models.default` | none | Model for new sessions, as `<endpoint>:<model>` (split at the first `:`, so `ollama:qwen3:8b` works) |
| `endpoints.<id>.base_url` | — | OpenAI-compatible base URL, ending before `/chat/completions` |
| `endpoints.<id>.api_key` | none | `{ secret = "<name>" }`, read from the keychain when a session starts |
| `endpoints.<id>.pricing."<model>"` | none | `{ input_usd_per_mtok, output_usd_per_mtok, cached_input_usd_per_mtok? }`; models without prices report no cost |
| `endpoints.<id>.kind` | `openai` | The API the endpoint speaks, choosing its model client: `openai` (OpenAI-compatible Chat Completions; an endpoint on this machine is also tried for llama.cpp's tokenizer) or `llamacpp` (the same, always using llama.cpp's tokenizer for [context breakdowns](03-agent-engine.md#context-management)) |
| `models.folders` | none | Folders scanned for GGUF models (`llamacpp:<id>`) and Hugging Face folders (`vllm:<id>`); the `llamacpp` and `vllm` endpoint ids are reserved |
| `models.download_dir` | first of `models.folders`, else `<data>/models` | Where [Hugging Face downloads](04-models-and-routing.md#downloading-from-hugging-face) go, as `<owner>/<repo>/` below it; must be absolute; scanned with `models.folders` |
| `runtime.llamacpp.backend` | `auto` | Runtime pack: `auto`, `cpu`, `vulkan`, `cuda12`, `cuda13`, `rocm`, or `metal` |
| `runtime.llamacpp.custom_path` | none | Your own `llama-server` executable, used instead of a pack |
| `runtime.llamacpp.context` | `8192` | Context per slot each loaded model is started with |
| `runtime.llamacpp.parallel` | `2` | Slots per loaded model (agents served at once) |
| `runtime.llamacpp.cache_type` | `f16` | KV cache type: `f16`, `q8_0`, or `q4_0` |
| `runtime.llamacpp.idle_unload_s` | `900` | Seconds an unused model stays loaded |
| `runtime.llamacpp.ram_headroom_mb` | `2048` | System memory kept free when placing models |
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
| `schedules` | none | `[[schedules]]` with `id`, `trigger` (`cron`, `every`, or `at`), `action` (`project` and one of `prompt`, `command`, or `agent` (a message for the project agent), optional `model`, `timeout_s`), `missed`, `overlap`, `notify`, `enabled`; see [schedules](14-projects-and-automation.md#schedules) |
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
| `projects` | none | `[[projects]]` with `path`, `agent` (run a project agent), `agent_model`, `max_sessions` (2), `max_sessions_per_day` (20) |
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
backend = "auto"          # auto | cpu | cuda | vulkan | rocm | metal | custom
custom_path = ""
idle_unload_s = 900

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
