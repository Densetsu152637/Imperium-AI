# 05 — Tools and isolation

## Built-in tools

| Tool group  | Tools                                                     | Notes                                            |
| ----------- | --------------------------------------------------------- | ------------------------------------------------ |
| Files       | `read_file`, `write_file`, `edit_file`, `list_dir` (implemented); glob and grep planned | Exact-match unique edits; reads numbered and paged with `offset`/`limit` |
| Execution   | `run_command` (implemented); background processes planned | Runs in the project root via `cmd /C` or `sh -c`; default timeout 120 s; returns exit code, stdout, stderr |
| Web         | `web.search`, `web.fetch`                                 | Search providers: SearXNG (self-hosted), Brave, Tavily, Kagi; pluggable |
| Browser     | `browser.open`, `browser.read`, `browser.act`, `browser.screenshot` | Chromium over CDP; visible or headless |
| Agents      | `spawn_agent`, `await_tasks`, `send_to_task`, `cancel_task` | See [agent engine](03-agent-engine.md)        |
| Media       | `media.generate`, `media.transcribe`                      | See [media](09-media-generation.md)              |
| Interaction | `ask_user`                                                | Pauses the agent until the user answers          |

Implemented in phase 1 (`imperium-tools`): paths resolve inside the project folder and may not
escape it through `..`, absolute paths, or symbolic links (tier 0). Reads and listings run
without asking; writes, edits, and commands ask the user, who can allow once, allow that tool
for the rest of the session, or deny. On timeout the shell is killed, but processes it started
may keep running until the OS sandbox tier adds process-tree cleanup.

All tools are described by JSON Schema and are also exposed over an MCP server endpoint, so
external agents can use Imperium's tools.

## MCP

- Client support for stdio and streamable-HTTP MCP servers, including tools, resources,
  prompts, and server-initiated sampling (routed through Imperium's router and budgets).
- Servers are configured globally, per project, or by plugins; each has its own permission
  scope and runs inside the session's isolation tier when it is a local process.
- OAuth flows for remote MCP servers open in the system browser; tokens go to the keychain.

## Permissions

Each tool call is checked against the effective permission set (config → policy → session →
user prompt). Modes:

- `ask` — prompt the user (default for writes, exec, network, and anything outside the
  environment).
- `allow` / `deny` — rules by tool, path glob, command prefix, domain, or MCP server.
- `auto` — a classifier model approves routine actions and escalates unusual ones.

Decisions are logged in the session journal.

## Isolation tiers

The UI always shows the active tier for each session and agent.

| Tier            | Boundary                                                                     | Protects against                                    | Does not protect against                           |
| --------------- | ---------------------------------------------------------------------------- | --------------------------------------------------- | -------------------------------------------------- |
| 0 — Policy      | Daemon refuses built-in file tools outside allowed roots                     | Model mistakes with built-in tools                  | Code run through `exec`, which has the user's full rights |
| 1 — OS sandbox  | Linux: Landlock + seccomp (bubblewrap); macOS: Seatbelt profile; Windows: AppContainer / restricted token (best effort) | Executed code reaching files outside allowed roots, optional network block | Kernel or sandbox escapes; Windows coverage is partial |
| 2 — Container   | Docker/Podman container with only the workspace mounted                      | Executed code affecting the host                    | Container escapes; mounted folders are writable    |
| 3 — Remote      | Separate machine over SSH                                                    | Anything on the local machine                       | The remote machine itself                          |

- Tier 0 is always on. Higher tiers are opt-in per session or forced by policy (for example,
  "always use a container for `exec` from subagents running hosted models").
- Container images are user-selectable; a default dev image ships with common toolchains.
- GPU access inside containers uses the NVIDIA container toolkit / ROCm devices where available.

## SSH hosts

- Hosts are configured from `~/.ssh/config` entries or added manually; keys stay in the user's
  SSH agent. Imperium never stores SSH passwords.
- On first connect, Imperium installs or updates a matching `imperiumd` on the host (with
  consent), starts it bound to a Unix socket, and forwards that socket.
- The remote daemon owns the remote files, processes, models, and GPUs. The local router can
  still send remote subagents to hosted endpoints or to local-machine models when the user
  allows cross-host routing.
- Model folders, runtimes, and isolation tiers on a host are configured on that host.
