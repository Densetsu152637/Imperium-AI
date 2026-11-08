# Imperium Nursery

A UI mod for the Imperium desktop app where many pets live at once. It is built on the
[Pixel Pets](https://github.com/Densetsu152637/Pixel-Pets) behaviour engine (MIT), ported to
run inside a mod frame, and gives every pet its own arbiter, so each one idles, works, waits,
sleeps and reacts on its own.

## Enabling it

The mod is opt-in. Turn it on in **Settings → Mods**, then add its panels from the panel
menu. Its permissions are:

| Permission     | Why |
|----------------|-----|
| `sessions:read`| Session snapshots and the raw session events that drive the pets. |
| `pets:read`    | The installed pets and their animation strips. |
| `storage`      | Your settings and "My nursery" residents. |

Pets see activity status only (a turn started, a tool ran, a subagent finished). They never see
transcript text.

## The three nurseries

- **Session nursery** (panel `session`): one pet per agent of a session, the root agent plus
  each subagent, with a line such as "3 subagents working". It follows the session selected in
  the app; the *Session* selector pins it to a specific session instead (remembered). A
  subagent's pet appears when it is spawned and leaves about 30 seconds after it finishes, after
  playing `jumping` or `failed`. Each pet hears only its own agent's events.
- **Global nursery** (panel `global`): one pet per session the app knows about, labelled with
  the session title, with a badge for its working subagents and a total line ("4 sessions, 2
  working, 5 subagents"). Working and waiting sessions come first. The pet hears all of its
  session's events, so a subagent that needs you makes it show `alert`.
- **My nursery** (panel `mine`): your own pets. Pick any installed pet, give it an optional
  nickname and press **Put in**; press **×** on a pet to take it out. Residents idle, wander
  and fidget, fall asleep after a minute, and play `petted` when clicked. Who lives there is
  kept across restarts.

Clicking a pet in any nursery pats it.

## Which pet an agent gets

The **Pets** select at the top of the session and global panels chooses the pet for agents and
sessions: *A mix of my pets* (the default; the pet is picked from the installed ones by hashing
the agent or session id, and the root agent of the session nursery gets the first pet) or one
specific pet. The choice is stored as `agentPets`.

## Adding your own pets

Any pet installed for Imperium shows up: Pixel Pets v3 packages in the pets folders
(`pets.folders`, `<config>/pets`, `~/.claude/pets`, a `pets/` folder next to the daemon). Pets
made with the Pixel Pets `hatch-pet` skill land in the pets folder and appear in the
nurseries (reopen the panel after installing one). A pet that does not draw an animation falls
back along its chain, ending in `idle`.

## Events to behaviour

The mapping follows the daemon's own pet (see `docs/16-pets.md`): `turn_started` and
`tool_started` loop `working`; `permission_requested` shows `waiting`; a denied tool call plays
`confused`; `task_spawned` plays `summoning`; `task_finished` plays `jumping` or `failed`;
`turn_completed` clears the loop and plays `jumping`; `turn_failed` plays `failed`;
`memory_saved` plays `eating` for a moment. A subagent whose brief carries a reviewer
`Profile:` tag (critic, validator, inspector, auditor) loops `review` instead of `working`.
App snapshots seed and repair the state, so a pet is already working when you open a panel
mid-turn.

## Development

```
pnpm install
pnpm --filter @imperium/nursery run typecheck
pnpm --filter @imperium/nursery run test
pnpm --filter @imperium/nursery run build   # writes index.js, gitignored
```

The build bundles `src/` into one self-contained ES module, `index.js`, because the app
inlines the entry into a sandboxed frame where relative imports do not resolve. Only
`@imperium/mod-sdk` stays external; the frame supplies it. Tests use Node's built-in runner
with type stripping (Node 24 or newer) and cover the arbiter, the pet package parser, frame
timing, the per-nursery event routing and the nursery state; none of it needs a DOM.

## Licence

MIT. Parts are ported from Pixel Pets (also MIT, same author); see `NOTICE` and
`LICENSE-pixel-pets`.
