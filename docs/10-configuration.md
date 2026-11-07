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

## Secrets

API keys, OAuth tokens, and pairing keys are stored in the OS keychain (Windows Credential
Manager, macOS Keychain, Secret Service on Linux) through the `keyring` crate. Config files
reference secrets by name only:

```toml
[endpoints.openrouter]
kind = "openai"
base_url = "https://openrouter.ai/api/v1"
api_key = { secret = "openrouter" }   # resolved from the keychain
```

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
