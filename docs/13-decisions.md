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
| D12 | Pets run in the daemon and render in every client | Consistent state; zero model cost | UI-only pets | — |
| D13 | Licence: MIT | Owner's choice | — | — |
| D14 | Planning is a runtime feature (plan objects, plan phase, approval modes) | Keeps weaker open models on track; plans visible and editable | Prompt-only planning | — |

## Open questions

| Question | Owner | Needed by |
| -------- | ----- | --------- |
| Which "pixel-pets" format should packs be compatible with? (link to spec) | User | Phase 9 |
| Add an MIT licence and an Imperium host-adapter section to Agent-Custom-Workflow before bundling | User | Phase 5 |
| Tauri mobile vs React Native for the phone app (prototype both briefly) | Maintainers | Phase 11 |
| Offer a hosted relay and push service, or self-host only? | User | Phase 11 |
| Windows code-signing provider | User | 1.0 |
| Default web search provider (SearXNG vs a paid API) | User | Phase 8 |
