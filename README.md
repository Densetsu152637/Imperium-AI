# Imperium-AI

A moddable desktop workbench for open-weight models. Imperium runs agents against
local runtimes (llama.cpp, vLLM) and any hosted endpoint, delegates work across many
models in parallel, and exposes everything — tokens, throughput, cost, hardware — to a
React interface that users can restyle, extend, or replace.

> **Status:** under active development on the `experimental` branch (about 150 merged pull
> requests). The core, CLI, and desktop app run today; installers ship on the experimental
> release channel. Features still being built are listed under [In progress](#in-progress).
> The full design lives in [`docs/`](docs/README.md).

## What makes it different

- **An explicit agentic workflow instead of an ever-growing context.** Agents follow a
  versioned [workflow contract](docs/18-workflow-contract.md): for each message or delegated
  brief, a deterministic resolver builds a *context manifest* of the instruction files that
  apply, and the agent loads each one with a visible tool call (optionally enforced before any
  write or command). Work is split into subagents that start with a fresh context and a precise
  brief, large inputs are chunked into artefacts the model pages through, and parents sleep at
  zero token cost until a subagent reports. The result is that long sessions stay sharp rather
  than degrading under context bloat. When compaction is unavoidable, the system prompt, loaded
  workflow files, and skills are carried over verbatim; only conversation is summarised.
- **Hybrid local and cloud agents.** A root agent on a hosted frontier model can delegate to
  subagents on local llama.cpp or vLLM models (or the other way round). The router sends each
  task to the cheapest model that meets its requirements and fits the machine's free memory,
  with queue-delay handling, capability filters, and escalation to a stronger model when needed.
- **Persistent agents.** Each environment can run several long-lived agents that survive
  restarts, keep their own memory (`Memory.md`), run on schedules, and can look for work on their
  own (agent initiative). This area is still being fleshed out.
- **Local models without the hassle.** Imperium finds or installs the right llama.cpp build for
  your GPU (NVIDIA, AMD, Intel, Apple Silicon), downloads GGUF models from Hugging Face, shares
  runtime slots and KV cache across agents, gives each agent the most context that fits, and
  supports speculative decoding and a local benchmark. It can also serve your local models as an
  OpenAI-compatible API.
- **Everything is visible.** Tokens, throughput, cost, CPU/GPU/RAM/VRAM, and an exact per-model
  context breakdown are shown live, with budgets and alerts per environment, endpoint, and model.

## Implemented today

- **Core:** a Rust daemon (`imperium-core`) serving token-protected JSON-RPC over a local
  WebSocket or socket; the `imperium` CLI and the Tauri + React desktop app are both clients.
  Sessions survive restarts, can be stopped and resumed, branched from any message, and compared
  across models.
- **Models:** any OpenAI-compatible endpoint (OpenRouter, LM Studio, Ollama, ...), local
  llama.cpp and vLLM, keys in the OS keychain, per-session reasoning effort, mid-session model
  hot-swap.
- **Agents and tools:** planning and delegation, worktrees for parallel writer subagents, file,
  glob/grep, shell and background-command tools, web search and fetch with citations, a CDP
  browser the user can take over, live preview, image, speech, and transcription tools, and
  `ask_user` questions.
- **Safety:** allow/ask/deny permission rules, path grants, per-agent permission profiles,
  OS-sandbox, container, WSL, and SSH isolation, git checkpoints with rewind, a review pane with
  per-hunk accept/reject, and undoable, journalled config changes made by agents.
- **Ecosystem:** MCP servers, Agent Skills (`SKILL.md`), Claude Code-style plugins, hooks and
  slash commands, workflow packs (Agent-Custom-Workflow is bundled as the default), and a UI mod
  SDK with sandboxed panels.
- **Automation:** daemon-run schedules, event subscriptions, and GitHub "Fix CI" with an approval
  queue.
- **Desktop:** tiling, detachable panels and saved layouts, command palette, session search,
  attachments and push-to-talk, native notifications, self-update on main and experimental
  channels, and the Nursery: moddable pixel pets that act out what agents are doing.
- **Phone:** pair a phone to follow, steer, and approve sessions through a scoped web app.
- **Platforms:** installers for Windows, macOS, and Linux built in CI.

## In progress

- Training the default root agent ("Emperor") towards the working style of frontier agents.
- Richer persistent-agent behaviour and project automation.
- Hook and mod extras, policy-pack updates, and an Imperium MCP server.
- Session sharing and sync (designed, not built), more web and browser tooling, and desktop gaps
  such as rebindable shortcuts.

See the [roadmap](docs/12-roadmap.md) for the full list.

## Install

Download the installer for your platform from the
[releases page](https://github.com/Densetsu152637/Imperium-AI/releases): `…_x64-setup.exe` on
Windows, the `.dmg` on macOS, or the `.AppImage` or `.deb` on Linux. It installs the desktop app
together with the `imperium-core` daemon and the `imperium` CLI; the app starts the daemon when it
opens and keeps itself up to date (Settings → Updates chooses the main or experimental channel).

## Development

```bash
cargo build
cargo run --bin imperium-core      # leave running; Ctrl+C stops it
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

To have Imperium load GGUF models itself, list your model folders, install llama.cpp's build
for your hardware (or let Imperium use a `llama-server` already on the machine), and use a
model as `llamacpp:<id>`:

```toml
[models]
folders = ["D:/Models"]
```

```bash
cargo run --bin imperium -- runtime install
cargo run --bin imperium -- models local
```

Or download one from Hugging Face (into the first model folder, or `models.download_dir`):

```bash
cargo run --bin imperium -- models files Qwen/Qwen3-8B-GGUF
cargo run --bin imperium -- models pull Qwen/Qwen3-8B-GGUF --quant Q4_K_M
```

The desktop app needs the daemon running, then:

```bash
pnpm --filter @imperium/desktop tauri dev
```

## Documentation

Start at [docs/README.md](docs/README.md).

## Licence

[MIT](LICENSE)
