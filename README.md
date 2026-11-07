# Imperium-AI

A moddable desktop workbench for open-weight models. Imperium runs agents against
local runtimes (llama.cpp, vLLM) and any hosted endpoint, delegates work across many
models in parallel, and exposes everything — tokens, throughput, cost, hardware — to a
React interface that users can restyle, extend, or replace.

> **Status:** planning. No application code exists yet; the design lives in [`docs/`](docs/README.md).

## Highlights (planned)

- **Rust core, React shell.** A headless daemon (`imperiumd`) owns sessions, agents, tools,
  and models. The CLI (`imperium`), the Tauri desktop app, and remote hosts are all clients of
  the same protocol.
- **Any model, anywhere.** OpenAI-compatible endpoints, local llama.cpp and vLLM, multiple
  model folders, keys stored in the OS keychain.
- **Reactive delegation.** Subagents run as tasks; parents sleep at zero token cost until a
  completion event arrives.
- **Cost- and hardware-aware routing.** Each subagent goes to the cheapest model that meets the
  task's requirements and fits the machine's free memory — otherwise it reuses a loaded model or
  falls back to a hosted endpoint.
- **Open ecosystem.** MCP servers, Agent Skills (`SKILL.md`), and Claude Code–style plugin and
  hook manifests.
- **Layered policy.** Ships with the [Agent-Custom-Workflow](https://github.com/Densetsu152637/Agent-Custom-Workflow)
  policy by default; override, fork, or disable it.
- **Isolation tiers.** Folder policy, container, or SSH remote — always visible in the UI.
- **Projects and automation.** Per-project persistent agents and cron/event schedules that run
  while the computer is on.
- **Phone app.** Pair a phone with each computer to follow, steer, and approve remote sessions.
- **Pets.** Moddable pixel companions that act out what the agents are doing.
- **Media generation.** Image, video, and audio models through ComfyUI and a Python sidecar.
- **Built for ultrawide.** Independent font and density scaling, tiling panes, readable line lengths.
- **Linux, Windows, and macOS** from the first release.

## Development

Phase 0 provides the Rust core (daemon, CLI, protocol, configuration) and the TypeScript
protocol package.

```bash
cargo build
cargo run --bin imperiumd          # leave running; Ctrl+C stops it
cargo run --bin imperium -- ping   # in another terminal
cargo run --bin imperium -- config explain daemon.port
bash scripts/check-rust.sh         # what CI runs: fmt, clippy, tests, generated-type drift
pnpm install && bash scripts/check-web.sh   # TypeScript packages
```

Set `IMPERIUM_HOME` to keep a development daemon's files apart from your own.

To chat with an agent, add an endpoint to your config file (see
[configuration](docs/10-configuration.md#implemented-settings)) and restart the daemon:

```toml
[models]
default = "openrouter:qwen/qwen3-coder"

[endpoints.openrouter]
base_url = "https://openrouter.ai/api/v1"
api_key = { secret = "openrouter" }   # store it with: imperium secret set openrouter
pricing."qwen/qwen3-coder" = { input_usd_per_mtok = 0.2, output_usd_per_mtok = 0.8 }
```

```bash
cargo run --bin imperium -- chat --project path/to/project
```

Local servers such as llama.cpp, LM Studio, or Ollama work the same way with their
OpenAI-compatible URL and no `api_key`.

To have Imperium load GGUF models itself, list your model folders, install the llama.cpp
runtime pack for your hardware, and use a model as `llamacpp:<id>`:

```toml
[models]
folders = ["D:/Models"]
```

```bash
cargo run --bin imperium -- runtime install
cargo run --bin imperium -- models local
```

The desktop app needs the daemon running, then:

```bash
pnpm --filter @imperium/desktop tauri dev
```

## Documentation

Start at [docs/README.md](docs/README.md).

## Licence

[MIT](LICENSE)
