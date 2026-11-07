# 02 — Architecture

## Process model

```text
 ┌──────────────┐   ┌──────────────┐   ┌──────────────────┐
 │ Desktop app  │   │  imperium    │   │ Scripts / mods / │
 │ (Tauri+React)│   │  (CLI/TUI)   │   │ other clients    │
 └──────┬───────┘   └──────┬───────┘   └────────┬─────────┘
        │  JSON-RPC 2.0 over WebSocket (local) or SSH-forwarded socket
        └──────────────────┼────────────────────┘
                    ┌──────┴───────┐
                    │  imperiumd   │  sessions · agent engine · scheduler · telemetry
                    └──┬───┬───┬───┘
        ┌──────────────┘   │   └───────────────┐
 ┌──────┴──────┐  ┌────────┴───────┐  ┌────────┴────────┐
 │ Model       │  │ Tool hosts     │  │ Sidecars        │
 │ runtimes    │  │ (fs, exec, web,│  │ (media: Python, │
 │ llama-server│  │  browser, MCP) │  │  ComfyUI)       │
 │ vLLM, HTTP  │  └────────────────┘  └─────────────────┘
 └─────────────┘
```

- **`imperiumd`** is the only process that holds state. It owns sessions, the agent engine,
  the model scheduler, tool execution, policy resolution, configuration, and telemetry.
- **Clients** are stateless views. The desktop app embeds or launches a local daemon; the CLI
  connects to a running daemon or starts an ephemeral one (`imperium run --ephemeral`).
- **Remote hosts** run their own `imperiumd`. The desktop connects through an SSH tunnel to the
  remote daemon's Unix socket, so remote sessions use the remote machine's files, models, and
  GPUs with no special-case code paths (see [isolation](05-tools-and-isolation.md#ssh-hosts)).
- **Model runtimes and sidecars** are child processes supervised by the daemon (restart policy,
  health checks, log capture, memory accounting).

## Protocol

- JSON-RPC 2.0 requests and responses, plus server-pushed **event streams** (subscriptions).
- Transport: WebSocket on `127.0.0.1` with a per-launch bearer token (written to a user-only
  file), or a Unix domain socket / Windows named pipe. Never bound to a public interface by
  default.
- Schema defined in Rust (`imperium-protocol`) and exported to TypeScript with `ts-rs` /
  JSON Schema, published as `@imperium/protocol`. CI fails if the generated types drift.
- Versioned: `protocol.version` in the handshake; additive changes bump minor, breaking
  changes bump major and keep the previous major for one release.

Core method groups: `session.*`, `agent.*`, `task.*`, `model.*`, `runtime.*`, `tool.*`,
`mcp.*`, `plugin.*`, `policy.*`, `config.*`, `secret.*` (write-only), `telemetry.*`, `fs.*`.

## Rust workspace

| Crate                 | Responsibility                                                         |
| --------------------- | ---------------------------------------------------------------------- |
| `imperium-protocol`   | Wire types, method/event definitions, TS export                        |
| `imperium-core`       | Sessions, agent loop, delegation, task graph, event bus                |
| `imperium-models`     | Endpoint clients, model registry, router, cost accounting              |
| `imperium-runtime`    | llama.cpp / vLLM process supervision, hardware probing, memory planner |
| `imperium-tools`      | Built-in tools: fs, exec, web search, fetch, browser (CDP)             |
| `imperium-mcp`        | MCP client (stdio, streamable HTTP) and server bridge                  |
| `imperium-sandbox`    | Isolation tiers: policy guard, container driver, SSH transport         |
| `imperium-extensions` | Plugin, skill, and hook loading; manifest compatibility layers         |
| `imperium-policy`     | Policy pack resolution, layering, and prompt assembly                  |
| `imperium-config`     | Layered config, schema validation, keychain access                     |
| `imperium-telemetry`  | Metrics, traces, cost ledger, OpenTelemetry export                     |
| `imperium-automation` | Projects' persistent agents and schedules                              |
| `imperium-remote`     | Device pairing, encrypted remote transport, relay client               |
| `imperium-pets`       | Pet engine, pack loading, WASM behaviour host                          |
| `imperiumd`           | Daemon binary                                                          |
| `imperium-cli`        | `imperium` binary: CLI and TUI client                                  |

Key dependencies (to confirm during phase 0): `tokio`, `axum` / `tokio-tungstenite`,
`serde`, `reqwest`, `rmcp` (official Rust MCP SDK), `chromiumoxide`, `keyring`, `sysinfo`,
`nvml-wrapper`, `russh`, `gguf` metadata parsing, `tracing` + `opentelemetry`.

## Repository layout

```text
Cargo.toml              Rust workspace
crates/                 Rust crates listed above
apps/desktop/           Tauri 2 shell (src-tauri/) and React app (src/)
apps/mobile/            Phone app (phase 11)
pets/                   Bundled default pet packs
packages/protocol/      Generated TypeScript protocol types
packages/mod-sdk/       UI mod SDK (@imperium/mod-sdk)
packages/ui/            Shared React components and design tokens
sidecars/media/         Python media sidecar (uv project)
runtimes/               Pinned runtime versions (llama.cpp ref, vLLM version)
policy/                 Bundled default policy pack (pinned snapshot)
scripts/                Check, build, and CI helper scripts
docs/                   These documents
```

JavaScript uses a pnpm workspace (`package.json`, `pnpm-workspace.yaml`, `pnpm-lock.yaml` at
the root). Python uses `uv` with a committed `uv.lock`.

## Data locations

Resolved with platform conventions (`directories` crate):

| Kind      | Linux                         | Windows                       | macOS                                     |
| --------- | ----------------------------- | ----------------------------- | ----------------------------------------- |
| Config    | `~/.config/imperium`          | `%APPDATA%\Imperium`          | `~/Library/Application Support/Imperium` |
| Data      | `~/.local/share/imperium`     | `%LOCALAPPDATA%\Imperium`     | `~/Library/Application Support/Imperium`  |
| Cache     | `~/.cache/imperium`           | `%LOCALAPPDATA%\Imperium\cache` | `~/Library/Caches/Imperium`             |

Data holds session transcripts (SQLite + append-only JSONL), the cost ledger, installed
plugins, runtime packs, and policy snapshots. `IMPERIUM_HOME` overrides all three.

## Concurrency and failure

- One Tokio runtime; each agent turn, tool call, and runtime is a task with a cancellation
  token. Cancelling a parent cancels its subtree unless a child is marked `detached`.
- Runtime crashes surface as events; in-flight requests on that runtime fail with a retryable
  error and the router may re-route them.
- Session state is journaled before each model call, so a daemon restart resumes sessions at
  the last completed step.
