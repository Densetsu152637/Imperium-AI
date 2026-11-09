# Imperium Nursery

A UI mod for the Imperium desktop app where many pets live at once. It is built on the
Pixel Pets v3 behaviour engine from [Imperium Nursery](https://github.com/Densetsu152637/Imperium-Nursery) (MIT), ported to
run inside a mod frame, and gives every pet its own arbiter, so each one idles, works, waits,
sleeps and reacts on its own.

## Enabling it

The mod ships with Imperium and is on by default. Its Nursery panel *is* the app's pet panel
(the built-in Pet panel is gone): it docks in the left column where that panel was, and the
Session nursery sits in the right column. Turn the mod off in **Settings → Mods**. Its
permissions are:

| Permission     | Why |
|----------------|-----|
| `sessions:read`| Session snapshots and the raw session events that drive the pets. |
| `pets:read`    | The installed pets and their animation strips. |
| `storage`      | Your settings, your pet and the extra pets. |

Pets see activity status only (a turn started, a tool ran, a subagent finished). They never see
transcript text.

## The two panels

- **Session nursery** (panel `session`): one pet per agent of a session, the root agent plus
  each subagent, with a line such as "3 subagents working". It follows the session selected in
  the app; the *Session* selector pins it to a specific session instead (remembered). A
  subagent's pet appears when it is spawned and leaves about 30 seconds after it finishes, after
  playing `jumping` or `failed`. Each pet hears only its own agent's events.
- **Nursery** (panel `nursery`, left column): one room with three kinds of pets.
  - **Your pet** is always there and cannot be taken out. Choose it with the *Your pet* select
    (any installed pet; the first one is the default, stored as `resident`). It reacts to every
    unarchived session's activity combined (working while any session works, waiting while any
    asks for permission), as the old single pet panel did.
  - **One pet per unarchived session**, labelled with the session title, with a badge for its
    working subagents. It hears all of its session's events, so a subagent that needs you makes
    it show `alert`. A new session's pet pops in; archiving the session makes its pet walk out
    and fade; unarchiving brings it back. The summary line reads "4 sessions, 2 working, 5
    subagents", and working and waiting sessions come first.
  - **Extra pets**: open *Add a pet*, pick any installed pet, give it an optional nickname and
    press **Put in**; press **×** on one to take it out. They idle, roam, play with each other
    and fidget, fall asleep after a minute, and play `petted` when clicked. Who is there is
    kept across restarts (the `residents` storage key, as in the former My nursery).

Clicking a pet pats it; every pet is also a focusable button, so the keyboard works too.

## One shared room

Each panel is a single room that fills the panel below its controls. There are no cards or
cages: pets stand anywhere in it, and a pet lower on screen (nearer) is drawn in front of one
higher up, with a small shadow on the floor. A pet's label stays under it, the badge of the
session pets and the **×** of an extra pet (shown on hover or focus) travel with it.

- **Roaming.** A free pet picks a spot anywhere in the room, walks there at a steady pace
  (`walk-left`/`walk-right` by direction; a pet without walk strips glides in `idle`), pauses
  a few seconds and goes on. Pets keep from stacking up: they push each other apart softly.
- **Busy pets** (working, waiting, reacting) stop where they are and play their animation.
  A finished subagent walks to the nearer side of the room and fades out as it is removed.
  A new subagent appears (`pop-out`) next to its parent in the session nursery.
- **Size.** The room follows the panel (pets are pulled back inside when it shrinks) and grows
  taller when crowded, about 11 000 px squared per pet; the panel scrolls only past that.
- **Reduced motion.** With `prefers-reduced-motion` nothing walks: the pets stand spread out
  over the room in a fixed layout and show their first frames.

## Play

Every 15 to 45 seconds, if two pets are both free, they act out a short scripted interaction,
picked at random. Any two free pets in the room can play: usually the nearest pair, now and
then two far apart, in which case they first walk across the room to each other (up to about
12 seconds). Two roaming pets that pass close to each other may also greet in passing.

- **greet**: they turn toward each other with the `look` cells, then both wave.
- **chase**: one runs after the other, which runs away across the room; both jump at the end.
- **high-five**: they approach each other and jump in sync.
- **nap together**: drowsy pets settle down next to each other, sleep for a while, then wake (`waking`).
- **show-off**: one celebrates, the other watches (`look`) and then waves.

A pet is free when it is awake, shows an idle-level animation (idle, fidget, look), is not at
work for an agent, is not a finished subagent waiting to leave, and has not been touched in the
last few seconds. Play goes through each pet's own arbiter as ordinary one-shots, plus a pose
(walk, look, sleep) laid over `idle`; it never outranks anything. Any real event for either
pet, or a pat, ends the interaction for both at once and they stop and go back to roaming. A pet that
does not draw an animation falls back along its chain (a pet without walk strips slides
instead, one without `celebrating` jumps). With `prefers-reduced-motion` nothing moves and
nothing plays. The **Play** checkbox next to the Pets select (stored as `play`, on by
default) turns it off.

## Which pet an agent gets

A session's pet is, in order: the **default pet of its environment** (set in the environment
window; the host passes it as `SessionSnapshot.pet`) when that pet is installed; else the
**Pets** select at the top of the panel: *A mix of my pets* (the default; picked from the
installed ones by hashing the session id), *Same as my pet* (Nursery panel only), or one
specific pet; else your pet. The choice is stored as `agentPets`. In the Session nursery the
select picks the pets of agents (the root agent gets the first pet in a mix).

## Adding your own pets

Any pet installed for Imperium shows up: Pixel Pets v3 packages in the pets folders
(`pets.folders`, `<config>/pets`, `~/.claude/pets`, a `pets/` folder next to the daemon). Pets
made with the `hatch-pet` skill of the [pet-hatchery plugin](https://github.com/Densetsu152637/Imperium-Nursery/tree/main/plugins/pet-hatchery) land in the pets folder and appear in the
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

```sh
pnpm install
pnpm --filter @imperium/nursery run typecheck
pnpm --filter @imperium/nursery run test
pnpm --filter @imperium/nursery run build   # writes index.js, gitignored
```

The build bundles `src/` into one self-contained ES module, `index.js`, because the app
inlines the entry into a sandboxed frame where relative imports do not resolve. Only
`@imperium/mod-sdk` stays external; the frame supplies it. Tests use Node's built-in runner
with type stripping (Node 24 or newer) and cover the arbiter, the pet package parser, frame
timing, the room (bounds, roaming, separation, pairing, layout), the per-nursery event routing and the nursery state; none of it needs a DOM.

## Licence

MIT. Parts are ported from Pixel Pets (also MIT, same author); see `NOTICE` and
`LICENSE-pixel-pets`.
