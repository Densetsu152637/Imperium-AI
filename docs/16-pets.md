# 16 — Pets

Pets are animated pixel companions that act out what the agents are doing. Imperium uses the
[Pixel Pets](https://github.com/Densetsu152637/Pixel-Pets) **format v3** as its native pet
format and implements its behaviour engine in the Rust runtime, so every client — desktop,
CLI/TUI, and phone — shows the same pet in the same state.

## Compatibility

- **Native format: Pixel Pets v3** (`spriteVersionNumber: 3`): a folder with `pet.json`, an
  8 × 22 atlas (`spritesheet.png`), and per-animation `strips/`. The format document in the
  Pixel Pets repository (`docs/pet-format-v3.md`) is normative; Imperium follows its package
  layout, atlas, pixel-art rules, 22 animations, fallbacks, and P0–P6 priorities.
- **Codex pets** (`hatch-pet` v1/v2) import through the Pixel Pets importer; rows 0–10 keep
  their meaning, so they work without redrawing.
- **Existing pet folders** (`~/.claude/pets`, `CLAUDE_PETS_DIR`) can be added as extra pet
  folders, so pets made for the Claude Code plugin work unchanged.
- Pixel Pets is MIT-licensed; bundled pets and tools ship with its `LICENSE` and `NOTICE`. Each
  pet keeps its own licence from `pet.json`.

## Engine (`imperium-pets`)

The Pixel Pets **arbiter** is implemented in Rust inside the daemon, which removes the v3
plugin's current limits:

| Pixel Pets v3 plugin limit                 | In Imperium                                                     |
| ------------------------------------------ | --------------------------------------------------------------- |
| One chat; sources are the main turn and its subagents | Sources are every session, subagent, project agent, and schedule across all projects |
| Pet lives in a pane inside one window      | Pane, panel module, **and** an optional floating always-on-top overlay that can pop out of the window |
| Permission prompts inferred from tool verdicts | Exact events from the daemon (`approval.requested`, `approval.resolved`) |
| "Denied" recognised from error wording     | Typed permission-decision events                                |
| Terminal shows a text line                 | TUI crops frames itself (Kitty/Sixel graphics, or half-block truecolour) |

Arbiter rules are as in the format: loops are states and one-shots are reactions; one-shots
interrupt only equal or lower priority; one-shots within 1500 ms coalesce; `minMs` prevents
flicker; `waking` plays first after `sleeping`; a `look` glance toward a new source; hidden
after `duck-in` until `pop-out`. **Pets never call a model.**

## Event mapping

| Imperium event                                   | Animation                                        |
| ------------------------------------------------ | ------------------------------------------------ |
| Session opened / daemon starts                   | `pop-out`, then `waving`                         |
| Agent turn running / ends cleanly / errors       | `working` loop / `jumping` / `failed`            |
| `spawn_agent`                                    | `summoning`, then that source's `working` (`review` for reviewer roles) |
| Subagent completes / fails                       | `jumping` / `failed`, with a glance toward it    |
| Approval requested (any source)                  | `waiting`; `alert` if another source holds the pet |
| Permission denied, step retried, re-plan         | `confused`                                       |
| Plan completed and verified; commit, merge, tests pass | `celebrating`                              |
| Context compaction                               | `eating`                                         |
| Schedule fires while idle                        | `waking` (if asleep), then `working`             |
| User pats or clicks the pet                      | `petted`                                         |
| Idle past the sleep threshold / while idle       | `sleeping` / `fidget` every 20–60 s              |

Look directions map sources to screen positions: the active session's panel, the agent tree
row of a subagent, or the Projects panel for another project.

## Imperium extensions (optional)

Pixel Pets ignores unknown top-level fields, so Imperium adds an optional `imperium` object to
`pet.json`. A pet without it behaves exactly as the format defines.

```json
{
  "imperium": {
    "bindings": [
      { "event": "telemetry.call.completed", "when": "tokens_per_sec > 80", "animation": "walk-right" },
      { "event": "budget.warning", "animation": "alert" }
    ],
    "behaviour": "behaviour.wasm",
    "scope": "project"
  }
}
```

- **`bindings`** remap or add triggers for any hook or telemetry event, including custom
  animations (rows 22+).
- **`behaviour`**: an optional sandboxed WASM module (no I/O beyond the pet API) for moods,
  growth, or mini-interactions.
- **`scope`**: `global` (one pet for everything, default), `project`, `session`, or `agent`
  (a small pet per agent in the agent tree).

## Rendering and options

- Desktop: a **Pet module** that can live in any panel ([07](07-desktop-ui.md#modules-inside-panels)),
  plus the floating overlay. Frames are cropped from strips and scaled with nearest-neighbour;
  `prefers-reduced-motion` shows first frames only.
- Options follow the Pixel Pets plugin: `pet`, `sleepAfterMinutes`, `fidget`, `size`,
  `petsDir` (Imperium accepts a list of pet folders), plus `overlay` and `scope`.
- Protocol: `pet.list`, `pet.install`, `pet.select`, `pet.state` (subscription), and
  `pet.interact`. Pets follow the extension permission model
  ([06](06-extensibility.md#permissions-model-for-extensions)) and see agent status events only,
  never transcript content.

## Creating pets

The Pixel Pets skills (`hatch-pet`, `import-codex-pet`, `pet-format`) are Agent Skills, so
they run in Imperium directly; image generation routes through the
[media backends](09-media-generation.md). Imperium validates packages in Rust against the same
rules as `tools/validate_pet.py`.
