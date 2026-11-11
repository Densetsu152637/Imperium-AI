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
                    │ imperium-core│  sessions · agent engine · scheduler · telemetry
                    └──┬───┬───┬───┘
        ┌──────────────┘   │   └───────────────┐
 ┌──────┴──────┐  ┌────────┴───────┐  ┌────────┴────────┐
 │ Model       │  │ Tool hosts     │  │ Sidecars        │
 │ runtimes    │  │ (fs, exec, web,│  │ (media: Python, │
 │ llama-server│  │  browser, MCP) │  │  ComfyUI)       │
 │ vLLM, HTTP  │  └────────────────┘  └─────────────────┘
 └─────────────┘
```

- **`imperium-core`** is the only process that holds state. It owns sessions, the agent engine,
  the model scheduler, tool execution, policy resolution, configuration, and telemetry.
- **Clients** are stateless views. The desktop app embeds or launches a local daemon; the CLI
  connects to a running daemon or starts an ephemeral one (`imperium run --ephemeral`).
- **Remote hosts** run their own `imperium-core`. The desktop connects through an SSH tunnel to the
  remote daemon's Unix socket, so remote sessions use the remote machine's files, models, and
  GPUs with no special-case code paths (see [isolation](05-tools-and-isolation.md#ssh-hosts)).
- **Model runtimes and sidecars** are child processes supervised by the daemon (restart policy,
  health checks, log capture, memory accounting).

## Protocol

- JSON-RPC 2.0 requests and responses, plus server-pushed **event streams** (subscriptions,
  planned).
- **Transport (implemented):** a WebSocket at `ws://127.0.0.1:<port>/rpc`, text frames only.
  The port is `daemon.port` (default `0`: a free port chosen at each start). The upgrade must
  carry `Authorization: Bearer <token>`; anything else gets HTTP 401. The token is 32 random
  bytes generated at every launch. It is never bound to a public interface. Browser-based
  clients (the desktop webview), which cannot set headers, instead offer the subprotocols
  `imperium.v0` and `imperium.token.<token>`; the daemon selects `imperium.v0`. Unix domain
  sockets and Windows named pipes are planned for SSH forwarding.
- **Discovery (implemented):** on start the daemon writes `<data>/daemon.json` (URL, token,
  PID, protocol version), readable only by the user (mode 0600 on Unix; per-user directory ACL
  on Windows), and removes it on shutdown. It holds a lock on `<data>/daemon.lock`, so only one
  daemon runs per user; a second start exits with "already running". A daemon killed without
  cleanup leaves a stale `daemon.json`, which the next start overwrites.
- **Handshake (implemented):** every connection's first call must be `system.handshake`
  (client name/version and protocol version). Other calls before it fail with `-32002`. The
  daemon refuses a different protocol major with `-32001`. `system.ping` returns uptime.
- **Sessions (implemented):** `session.create` (the caller is subscribed), `session.list`
  (each summary carries `created_at`, from when the session's journal was started),
  `session.subscribe` (returns the session's full event history so far, then streams new
  events with no gap or duplicate), `session.search` (searches running and archived sessions'
  content: the best match per session with an excerpt, highlights, and the history event to
  open at; see [17](17-memory.md#session-search-implemented)), `session.send`, and
  `session.respond`. Events arrive as
  `session.event` notifications. `session.send` takes an optional `mode` (`queue`, the
  default: run now, or after the current turn; `steer`: join the running turn) and `agent_id`
  (a running subagent), and `attachments` (files staged with `session.attach`, which references
  a file inside the session's folders or copies it into the session's attachments folder), and
  returns the message's `id`; `session.queue` lists, edits, or
  removes queued messages ([03](03-agent-engine.md#queueing-and-steering)). `voice.transcribe`
  turns recorded speech into text through `[media.transcription]`
  ([07](07-desktop-ui.md#attachments-and-voice-input-implemented)). `session.agents`
  lists a session's agents with their kept state and `session.agent_resume` gives a finished
  subagent a follow-up ([03](03-agent-engine.md#agent-state)). `session.control` changes a running session between turns:
  its usage-limit policy, what it watches, its model (without loading it; the next turn
  compacts the conversation to fit, [03](03-agent-engine.md#switching-models-and-compaction)),
  or compaction now, pins it (`set_pinned`) or renames it (`rename`; both kept across restarts
  and announced as `session.updated`); model switches and compaction are refused with `-32006` while a turn runs,
  and compaction with `-32007` when the conversation cannot fit.
- Schema defined in Rust (`imperium-protocol`) and exported to TypeScript with `ts-rs` into
  `packages/protocol/src/generated/` whenever `cargo test` runs. `scripts/check-rust.sh` (run
  by CI) fails if the checked-in files differ. The pnpm package `@imperium/protocol`
  (`packages/protocol`) re-exports them; its `src/index.ts` is generated by
  `scripts/generate-index.mjs` on every typecheck and is not checked in.
- Versioned: `ProtocolVersion { major, minor }` in the handshake (currently 0.14: 0.14 added `session.control` `set_permission_mode` and the `permission_mode` fields of session summaries and control results; 0.13 added `personalities.list`; 0.11 added the `model.residency` notification; 0.9 added `session.control` `set_effort`, the `effort` and `reasoning` fields of session summaries and control results, `reasoning` on model rows, and `effort` on `spawn_agent`; 0.8 added `session.attach`, `attachments` on `session.send`, queued messages, `turn_started`, and `steered`, and `voice.transcribe`; 0.7 added the `local_model_waiting` session event and the local model rows' context, layer and memory fields; 0.6 added the `budgets.*` methods and notifications, `confirm_budget` on `session.send`, and the `-32008` budget error; 0.5 added the `permissions.*` methods, environment permissions, and suggested rules on approvals); additive
  changes bump minor, breaking changes bump major and keep the previous major for one release.

Core method groups: `session.*`, `agent.*`, `task.*`, `model.*`, `runtime.*`, `tool.*`,
`mcp.*`, `plugin.*`, `policy.*`, `config.*`, `secret.*` (write-only), `telemetry.*`, `fs.*`.

## Rust workspace

| Crate                 | Responsibility                                                         |
| --------------------- | ---------------------------------------------------------------------- |
| `imperium-api`        | Shared types and traits: chat messages, tool specs, usage and pricing, the local-model and tool-provider interfaces, Markdown front matter (implemented) |
| `imperium-protocol`   | Wire types, method/event definitions, TS export                        |
| `imperium-engine`     | Agent engine library: sessions, agent loop, delegation, task graph, event bus |
| `imperium-models`     | Endpoint clients behind `ChatClient`, model registry, router, cost accounting |
| `imperium-local`      | GGUF discovery, hardware probe, memory planner, runtime packs, runtime supervision (llama.cpp, vLLM); carries out residency decisions, decides none on its own |
| `imperium-tools`      | Built-in tools: files, commands, isolation tiers (implemented)         |
| `imperium-web`        | Web search providers, page fetching, and a CDP browser (implemented)   |
| `imperium-mcp`        | MCP client (stdio, streamable HTTP; implemented) and server bridge (planned) |
| `imperium-sandbox`    | Isolation tiers: policy guard, container driver, SSH transport         |
| `imperium-extensions` | Plugin, skill, hook, and slash-command loading; Claude Code plugin compatibility (implemented) |
| `imperium-workflow`   | Workflow packs, context retrieval, load manifests, documentation pointers (implemented) |
| `imperium-policy`     | Policy pack layering and prompt assembly (planned)                      |
| `imperium-memory`     | Global and project memory: entries, index, search, redaction (implemented) |
| `imperium-config`     | Layered config, schema validation, secret store (keychain, in-memory)  |
| `imperium-telemetry`  | Metrics, traces, cost ledger, OpenTelemetry export                     |
| `imperium-automation` | Schedule triggers (cron, interval, one-shot), missed runs, run history (implemented); project agents (planned) |
| `imperium-github`     | Failing pull request checks through `gh`, auto-fix worktrees, commit and push (implemented) |
| `imperium-remote`     | Device pairing, encrypted remote transport, relay client               |
| `imperium-media`      | Image (OpenAI-compatible, ComfyUI), speech, and transcription tools (implemented) |
| `imperium-pets`       | Pixel Pets v3 packages and the behaviour arbiter (implemented); WASM behaviours (planned) |
| `imperium-core`       | The daemon (`imperium-core` binary, formerly `imperiumd`), also a library for tests |
| `imperium-cli`        | `imperium` binary: CLI and TUI client                                  |

`imperium-api` depends on no other Imperium crate, and crates that only share vocabulary
depend on it rather than on each other: `imperium-tools`, `imperium-mcp`, `imperium-web`, and
`imperium-media` take `ToolSpec` from it instead of `imperium-models`; `imperium-config` takes
`Pricing`; `imperium-memory` takes the front-matter parser instead of `imperium-extensions`;
and `imperium-local` implements its `LocalModels` trait without depending on `imperium-engine`.
Likewise `imperium-web`, `imperium-media`, and `imperium-mcp` implement its `ToolProvider` trait,
so `imperium-engine` does not depend on them (it reaches `imperium-mcp` only through
`imperium-extensions`, for plugins' server definitions); the daemon builds the providers.
`imperium-models`, `imperium-engine`, and `imperium-extensions` re-export the moved items at their
old paths.

Secrets go through the `SecretStore` trait in `imperium-config` (`load`, `store`, `delete`):
`KeychainSecrets` uses the OS keychain and `MemorySecrets` keeps them in memory for tests and
headless runs. The daemon builds one `Arc<dyn SecretStore>` at start and every key lookup
(endpoint and media keys, web search keys, MCP tokens, the Hugging Face token) reads through it;
environment variables such as `HF_TOKEN` still take precedence where they did before.

### Swappable implementations

Each replaceable subsystem is a trait with built-in implementations chosen by name, so a new
one is added without editing its callers:

| Seam | Trait | Built-ins | Chosen by |
| ---- | ----- | --------- | --------- |
| Secrets | `SecretStore` (`imperium-config`) | `KeychainSecrets`, `MemorySecrets` | The daemon at start |
| Local runtimes | `RuntimeBackend` (`imperium-local`) | `LlamaCpp`, `Vllm` | First backend that accepts the model format and is available; `Supervisor::with_backends` sets the list |
| Web search | `SearchProvider` (`imperium-web`) | `Searxng`, `Brave`, `Tavily` | `web.search` through `SearchProviders::register` / `build` |
| Image generation | `ImageGenerator` (`imperium-media`) | `OpenAiImages`, `ComfyUi` | `media.image.backend` through `ImageGenerators::register` / `build` |
| Local model leasing | `LocalModels` (`imperium-api`) | The `imperium-local` supervisor | The daemon at start |
| Local memory residency | `ResidencyPolicy` (`imperium-local::residency`) | `Standard` (the arbiter's rules), `CorePolicy` (`imperium-core::residency`) | The daemon's residency controller installs `CorePolicy` with `Supervisor::set_policy` |
| GitHub | `GitHub` (`imperium-github`) | REST client | The daemon at start |
| Model clients | `ChatClient` (`imperium-models`), with an optional `Tokenizer` | `OpenAiCompatible`, `LlamaCpp` | `endpoints.<id>.kind` (default `openai`) through `ChatClients::register` / `build`, given to each session as `SessionConfig::clients` |
| Agent tools beyond the agent's own | `ToolProvider` (`imperium-api`) | `WebTools` (`imperium-web`, with the browser), `MediaTools` (`imperium-media`), `McpTools` (`imperium-mcp`) | The daemon for each session, as `SessionConfig::tools` |

### Local memory residency

`imperium-core` owns every decision about which local models and KV caches are in memory
(roadmap item 16; [04](04-models-and-routing.md#residency-controller-implemented)). Its
`ResidencyController` (`crates/imperium-core/src/residency.rs`) runs one plan loop per local
runtime and is the policy calls consult; the `imperium-local` supervisor executes:

```text
            signals (slot freed, agent waiting, settings changed, every 15 s)
 supervisor ---------------------------------------------> ResidencyController (core)
     ^                                                        | plan(loaded, idle_unload_s)
     | execute(Action, Reason): Load, Unload, SaveSlot,       | refreshed_budget(probe)
     |   RestoreSlot, ResizePool, SetBudget                   | CorePolicy::decide (calls)
     +--------------------------------------------------------+
 every move --> Supervisor::moves --> model.residency (clients), model.swapped_out/in
```

The supervisor has no timer and spawns nothing that unloads; what it still does without being
told is what a call in flight needs (load its model, evict what the policy says, restore the
caller's saved context, restart a pool that must grow or a model the GPU rule moves once it
has drained), each reported with its reason.

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
examples/mods/          Example UI mods
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

| Kind      | Linux                         | Windows                          | macOS                                     |
| --------- | ----------------------------- | -------------------------------- | ----------------------------------------- |
| Config    | `~/.config/imperium`          | `%APPDATA%\Imperium\config`      | `~/Library/Application Support/Imperium` |
| Data      | `~/.local/share/imperium`     | `%LOCALAPPDATA%\Imperium\data`   | `~/Library/Application Support/Imperium`  |
| Cache     | `~/.cache/imperium`           | `%LOCALAPPDATA%\Imperium\cache`  | `~/Library/Caches/Imperium`               |

Data holds the daemon's endpoint and lock files and, later, session transcripts (SQLite +
append-only JSONL), the cost ledger, installed plugins, runtime packs, and policy snapshots.
Setting `IMPERIUM_HOME` puts all three under one root as `config/`, `data/`, and `cache/`.

## Concurrency and failure

- **One process.** `imperium-core` hosts everything; model runtimes and sidecars are the only
  child processes.
- **Task per agent (implemented).** The daemon runs one multi-threaded Tokio runtime with a
  worker per core. Every agent (a session's root and each subagent) is its own task on it, so
  agents run in parallel across cores, a busy agent cannot block the transport, and an idle
  agent waiting on the model, the user, or its subagents uses no CPU. Agents in a session
  share state through `Arc` and short-held locks; the transport talks to sessions over
  channels.
- Within a session, each agent turn, tool call, and runtime request is a task with a
  cancellation token. Cancelling a parent cancels its subtree unless a child is marked
  `detached`.
- Runtime crashes surface as events; in-flight requests on that runtime fail with a retryable
  error and the router may re-route them.
- Session state is journaled before each model call, so a daemon restart resumes sessions at
  the last completed step.
