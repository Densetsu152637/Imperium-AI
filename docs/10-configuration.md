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

`imperium config explain <key>` shows the effective value and its source. The desktop settings
UI edits the same files; every setting has a schema entry (published as JSON Schema for editor
completion).

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

Headless Linux without a Secret Service falls back to an encrypted file unlocked by a
passphrase. Secrets are never written to logs, transcripts, telemetry, or exported sessions,
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
max_concurrent = 8

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
