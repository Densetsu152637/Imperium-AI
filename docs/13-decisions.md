# 13 — Decisions

## Accepted

| ID | Decision | Reason | Rejected alternatives | Revisit if |
| -- | -------- | ------ | --------------------- | ---------- |
| D1 | Rust daemon (`imperiumd`) holds all state; CLI, desktop, phone are clients | One implementation for local, remote (SSH), and mobile use | CLI-only core invoked per request; logic in the Tauri process | — |
| D2 | Tauri 2 desktop shell with React + TypeScript | Native Rust integration, smaller footprint, mobile targets | Electron (heavier, separate Node runtime) | WebView inconsistencies block key UI features |
| D3 | OpenAI-compatible HTTP as the internal model interface | Every runtime and most hosts speak it | Per-vendor native clients only | A required capability can't be expressed |
| D4 | Isolation tiers with an always-visible indicator | Path checks alone cannot contain executed code | Claiming folder sandboxing as security | — |
| D5 | vLLM supported on Linux, WSL2, Docker, or remote — never bundled | No native Windows/macOS support | Bundling vLLM | Upstream adds native support |
| D6 | Runtime packs built in CI and downloaded on demand | Keeps installers small; per-backend builds | Bundling all backends; relying on upstream binaries | Upstream ships stable per-backend artefacts |
| D7 | Secrets only in the OS keychain | No plaintext keys on disk | Config-file keys | — |
| D8 | Media models via ComfyUI and a Python sidecar | Ecosystem is Python; ComfyUI covers image/video | Reimplementing pipelines in Rust | — |
| D9 | Agent-Custom-Workflow is the default, overridable policy pack | User's own workflow; open and forkable | Hard-coded prompts | — |
| D10 | Projects are environments; project agents are event-driven and run only while the daemon runs | Matches desktop lifetime; zero idle token cost | Cloud-hosted always-on agents | Users need off-machine persistence |
| D11 | Phone app connects end-to-end encrypted to paired computers (LAN, overlay network, or relay) | No data leaves the user's control | Hosted cloud backend holding sessions | — |
| D12 | Pets use the Pixel Pets v3 format, with the arbiter in the daemon and rendering in every client | Existing format and pets (including Codex imports); consistent state; zero model cost | Own pack format; UI-only pets | Pixel Pets format v4 |
| D13 | Licence: MIT | Owner's choice | — | — |
| D14 | Planning is a runtime feature (plan objects, plan phase, approval modes) | Keeps weaker open models on track; plans visible and editable | Prompt-only planning | — |
| D15 | One daemon process; every agent is a task on a multi-threaded Tokio runtime with a worker per core | Agents run in parallel across cores; idle agents cost no CPU; no thread per session to manage | A thread per session (agents of one session confined to one core); multi-process sessions | CPU-heavy tools need isolation from agent tasks |
| D16 | GitHub auto-fix responders run as ordinary sessions, push only to PR branches, and never merge, close, approve, or force-push | Keeps automation reviewable and reversible | Fully autonomous merging | Users ask for merge automation with required approvals |
| D17 | Memory is one-fact-per-file Markdown in global, project (shared and private), and session scopes, with an index always in context, search on demand, and approval before writes by default | Editable, reviewable, Git-friendly, and compatible with Claude Code memory and policy-pack lessons; approval keeps wrong facts out | A vector database as the source of truth; automatic writes without review | Memory volume makes file-based storage slow |
| D18 | Workflows follow a public, versioned contract; a built-in retriever serves a load manifest (deterministic triggers first, semantic suggestions second) and agents load the items themselves | Every kind of agent, model, and host loads the same instructions, visibly and auditably | Stuffing whole workflows into every prompt; letting each model decide what to read unaided | Manifests prove too coarse for small-context local models |
| D19 | Each llama.cpp model has one unified KV pool budgeted in tokens (`--kv-unified`, `--ctx-size` = pool, slots no longer multiply memory); a call reserves `min(prompt estimate + max_output_tokens, context)` tokens, the lease carries the `max_tokens` to send, a cut reply is retried once uncapped, and a full pool makes calls wait, not fail | Many sessions share one model's memory without the server failing every running call at once when the pool runs out | Keep `context × slots` (slots stay expensive); rely on llama-server alone (its overflow fails every running call); reserve the whole context per call (concurrency falls back to pool / context) | The retry shows up often in logs (raise the default reserve) |
| D20 | There is no global cap on sessions using local models; per-model `slots` (default the larger of 8 and `parallel`) is a throughput knob, and calls beyond it queue in Imperium, first come, first served | The user asked for no limit across models or sessions; queueing in Imperium keeps slot affinity and interactive-before-background ordering | An unbounded `--parallel` (memory not modelled); letting llama-server queue (loses affinity and ordering) | Per-slot host state of hybrid models makes 8 slots too costly |
| D21 | A load for one session never evicts, restarts or interrupts a model another session is using (a call lease, or mid-turn after the first call); idle unload and settings changes spare such a model too; an agent asleep in `await_tasks`, on an approval, or in a usage-limit pause holds no claim, so its model may be swapped out for any load | Sessions must not yield each other's models, and an unanswered approval must not lock a model forever; the `await_tasks` swap stays because two orchestrators each awaiting a subagent that needs the other's memory would otherwise wait for each other | Evicting claims of other sessions after a timeout; a lock per model held for a whole turn | Users report swapped-out orchestrators (prefer evicting the requester's own session's sleepers first) |
| D22 | Per-model settings live in an app-written layer, `<config>/models.toml`, loaded after the user file: strict on save, lenient at load (invalid entries dropped with a warning) | The user's `config.toml` and its comments are never rewritten; the layer shows in `config explain` and the CLI; a bad file never stops the daemon | Rewriting `config.toml` (needs `toml_edit`, can clobber hand edits); a JSON store outside the config layers | — |

## Open questions

| Question | Owner | Needed by |
| -------- | ----- | --------- |
| Add an Imperium host-adapter section to Agent-Custom-Workflow | User | Phase 5 |
| Tauri mobile vs React Native for the phone app (prototype both briefly) | Maintainers | Phase 11 |
| Offer a hosted relay and push service, or self-host only? | User | Phase 11 |
| Windows code-signing provider | User | 1.0 |
| Default web search provider (SearXNG vs a paid API) | User | Phase 8 |
