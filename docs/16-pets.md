# 16 — Pets

Pets are animated pixel companions that act out what the agents are doing. They are built into
the Rust runtime so every client — desktop, CLI/TUI, and phone — shows the same pet in the same
state, and they are fully moddable.

> **To confirm:** the exact "pixel-pets" asset format to stay compatible with. Until a
> reference spec is chosen, Imperium defines its own pack format (below) and supports other
> formats through importers.

## Design

- **State lives in the daemon** (`imperium-pets` crate). The pet engine subscribes to the
  session event bus and drives a state machine; clients only render.
- **Zero model cost.** Pets react to events (tool calls, delegation, errors, approvals,
  completion, idle time, token throughput). They never call a model unless a pet pack opts in
  to an LLM "voice" with its own budget and model rule.
- **One pet per project** by default, optionally one per session or per agent (useful for
  seeing many subagents at a glance in the agent tree).

## States and triggers

| State              | Triggered by                                    |
| ------------------ | ----------------------------------------------- |
| `idle` / `sleep`   | No activity for N seconds / minutes             |
| `think`            | Model call streaming                            |
| `work`             | Tool executing (variants per tool group: `read`, `write`, `exec`, `web`) |
| `delegate`         | `spawn_agent`; pet "summons" helpers per subagent |
| `wait`             | Awaiting subagents or user approval             |
| `celebrate`        | Task or session completed successfully          |
| `fail`             | Error, refusal, budget exhausted                |
| `eat` / `grow`     | Optional stats: tokens processed, tasks completed |

Packs may define extra states and map any hook or telemetry event to them, with conditions
(for example `tokens_per_sec > 50 → run`).

## Pack format

A pet pack is a folder or Git repository:

```text
my-pet/
  pet.toml           metadata, states, transitions, event bindings
  sprites/           sprite sheets (PNG) or individual frames
  sounds/            optional
  script.wasm        optional behaviour logic (WASI, sandboxed)
```

```toml
id = "ember"
name = "Ember"
size = [32, 32]                 # native pixel size; scaled by integer factors only
palette = "sprites/palette.png" # optional, enables theme recolouring

[states.idle]
sheet = "sprites/idle.png"
frames = 4
fps = 6
loop = true

[states.work]
sheet = "sprites/work.png"
frames = 8
fps = 12

[[bindings]]
event = "tool.before"
when = "tool.group == 'exec'"
state = "work"
```

- Optional **WASM behaviour scripts** (sandboxed, no I/O except the pet API) allow custom
  logic such as moods, evolution, or mini-interactions, without trusting native code.
- **Importers** convert other formats (sprite sheets with JSON frame data such as Aseprite
  exports, and the chosen pixel-pets format) into packs.

## Rendering

| Client     | Rendering                                                                 |
| ---------- | ------------------------------------------------------------------------- |
| Desktop    | Canvas in a pane, the status bar, or an optional always-on-top transparent overlay window; nearest-neighbour integer scaling |
| CLI / TUI  | Half-block (▀) truecolour rendering, or Kitty/Sixel graphics where supported |
| Phone      | Notification and session-header widget                                    |

## Protocol

`pet.list`, `pet.install`, `pet.select`, `pet.state` (subscription), and `pet.interact`
(click/pet/feed events routed to the pack's script). Pet packs follow the extension permission
model ([06](06-extensibility.md#permissions-model-for-extensions)); by default they may read
agent status events only, not transcript content.
