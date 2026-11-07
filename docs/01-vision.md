# 01 — Vision and scope

## Problem

Existing open-weight front ends (for example Open WebUI) are chat-first, tied to one model per
conversation, hard to extend beyond themes and prompt presets, and scale poorly on large or
ultrawide displays. Agent tools that do support delegation, plugins, and tool use are usually
bound to one vendor's models.

## Goal

A desktop workbench that treats open-weight models as first-class agents:

1. Run agents against **any mix** of local runtimes and hosted endpoints in one session.
2. Delegate work to **parallel subagents**, each routed to the cheapest model able to do it.
3. Respect the **machine's hardware** when deciding what to load locally.
4. Expose **all state and telemetry** to a moddable interface and to scripts.
5. Ship a **sensible agent policy** by default, fully user-overridable.

## Non-goals

- Replacing Claude Code, Codex, or any vendor agent. Imperium may call vendor endpoints, but it
  is designed around open-weight models and open formats.
- Training or fine-tuning models (a possible later plugin; see [roadmap](12-roadmap.md)).
- Hosting a multi-tenant web service. Imperium is a single-user desktop app; remote access is
  through SSH or an authenticated local port.
- Building its own inference engine. Imperium orchestrates llama.cpp, vLLM, ComfyUI, and
  OpenAI-compatible servers.

## Platforms

Linux (x86_64, aarch64), Windows (x86_64, arm64), and macOS (Apple Silicon and Intel, universal
bundle) are supported from the first release. Feature differences are explicit, never silent:

| Capability                 | Linux                     | Windows                        | macOS                  |
| -------------------------- | ------------------------- | ------------------------------ | ---------------------- |
| llama.cpp                  | CPU, CUDA, Vulkan, ROCm   | CPU, CUDA, Vulkan              | Metal, CPU             |
| vLLM                       | Native                    | WSL2, Docker, or remote host   | Remote host only       |
| Container isolation        | Docker / Podman           | Docker Desktop / Podman (WSL2) | Docker / Podman (VM)   |
| Keychain                   | Secret Service            | Credential Manager             | Keychain               |

## Principles

- **Headless first.** Every feature works through the daemon protocol before it gets a UI.
- **Reactive, not polling.** Idle agents cost zero tokens.
- **Honest isolation.** The UI always shows what boundary actually protects the user.
- **Open formats over new ones.** MCP, Agent Skills, OpenAI-compatible APIs, GGUF.
- **Everything overridable.** Defaults are opinionated; users can replace any layer.
- **Observable.** Every token, second, byte of memory, and cent is accounted and exposed.

## Success criteria for 1.0

- A session can run a root agent on a hosted endpoint that delegates three subagents to three
  different backends (local llama.cpp, a second local model or reused instance, a hosted
  endpoint), with live per-agent telemetry and a final merged result.
- Installers for all three platforms come from CI, launch, and pass a smoke test.
- A third-party UI mod and a third-party MCP server install and run without code changes to
  Imperium.
