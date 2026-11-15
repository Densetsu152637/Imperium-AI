# Final cleanup: plan

**Status:** planned, revised after the first plan review. This is the last roadmap item
([12](../12-roadmap.md), item 46). It is **one pull request** against `experimental`, opened
after every other roadmap pull request has merged, and merged with a **merge commit** (not
squashed) so its commit structure survives. This document is the plan for that pull request:
the evidence, the decisions, the order of commits, what is out of scope, how the result is
checked, and how it is reviewed.

Figures were measured on `experimental` at `a08de87` (roadmap 34c, #189, merged). Line counts
include in-file test modules. Measure them again when the work starts.

## Goal and rules

Tidy the repository, split oversized modules, remove duplicated helpers, and route callers
through proper interfaces, **without changing behaviour**.

- **No behaviour change.** Model-visible text, files written to disk, CLI output, and timing
  stay as they are. The one decided exception is `imperium workflow resolve`
  ([decision 1](#decisions-already-made)).
- **No protocol change.** `PROTOCOL_VERSION` stays at 0.41; the generated TypeScript in
  `packages/protocol/src/generated/` and the tool schemas in `crates/imperium-protocol/schemas/`
  stay byte-identical.
- **No compatibility shims.** Code that moves has every consumer updated in the same commit; no
  `pub use` is left at an old path only so old imports keep working.
- **Move commits keep today's names.** A move commit moves code and changes only `use`, `mod`,
  `pub`/`pub(crate)` and `Cargo.toml` lines; renames (for example of `safety_floor`) are
  separate edit commits. A reviewer reads a move with
  `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change`.
- **No local Rust builds.** CI is the only compiler: the branch is pushed after each stage
  group and the next group starts only when CI is green. `cargo fmt` runs locally; nothing is
  `cargo install`ed locally.

## Decisions already made

Recorded from the user and the plan review (round 1):

1. **`imperium workflow resolve` is aligned with the daemon's pack loader** (named behaviour
   change to a diagnostic command): it will include the bundled default pack, skip an
   unreadable pack with a notice instead of failing, mark listed packs trusted, and apply the
   name-collision check to pinned packs. Caveat: the CLI finds the bundled pack by its own
   lookup (see [the policy crate](#decision-the-policy-crate)), so a CLI installed apart from the
   daemon, or run without `IMPERIUM_RESOURCE_DIR`, may find a different bundled pack or none;
   it prints which one it used. Settled when the work started: the aligned command **honours
   `workflow.enabled`** like the daemon (a turned-off workflow resolves nothing and the command
   says so), and an explicit **`--pack` stays exclusive**: it replaces `workflow.packs`, the
   pinned packs and the bundled pack, and the `enabled` switch is not consulted, exactly as
   today. The new behaviour applies only to the configured path (no `--pack`). A group-0 CLI
   test (`crates/imperium-cli/tests/workflow_resolve.rs`) pins today's behaviour; the commit
   that aligns the command rewrites the tests marked "today" in it, so the change is visible in
   that diff.
2. **The policy crate** is built as designed [below](#decision-the-policy-crate).
3. **Errors matched by text** become shared constants; no protocol change.
4. **Deprecated protocol names** (0.22 `persistent_agent`/`agent_session_id`, 0.31 memory scope
   names) are removed later, in a separate breaking-protocol pull request.
5. **Case-sensitive escaping in `engine/instructions.rs`** is fixed in a separate small
   security pull request **before** this one; this plan does not touch it and its golden test
   pins the fixed output.
6. **`unreachable_pub`** is enabled for library crates only.
7. **Desktop:** split the big components **and** regroup `apps/desktop/src` into feature folders.
8. **Untracked root files** (`plan.md`, `questions.md`, `emperor-root.md`) are in the user's
   working copy, not in git, and are out of scope.

## Inventory

### Oversized files

| File | Lines | Shape |
| ---- | ----: | ----- |
| `crates/imperium-local/src/supervisor.rs` | 5,699 | `impl Supervisor` 1243–3843 (largest fns: `swap_in` 276, `acquire_slot` 170, `plan` 158, `step` 150); tests from 4158 |
| `crates/imperium-engine/src/agent.rs` | 5,565 | one `impl Agent` 264–3521 plus two more; `run_turn_with` 376 lines, `await_tasks` 288, `spawn_agent` 250; 24 `*_spec()` builders; 4 test modules |
| `crates/imperium-protocol/src/methods.rs` | 4,963 | 360 public types; partly sectioned (`// ---- Checkpoints …`) |
| `crates/imperium-core/src/state.rs` | 3,394 | `impl DaemonState` 291–3236, 150 methods; `create_session_with` 480 lines |
| `crates/imperium-core/src/rpc.rs` | 2,742 | `Connection::handle` is one match of about 120 method arms (189–1715) |
| `crates/imperium-config/src/settings.rs` | 2,197 | settings structs and defaults |
| `crates/imperium-cli/src/chat.rs` | 2,041 | the TUI chat client |
| `apps/desktop/src/App.tsx` | 1,827 | `App()` is one component of about 1,580 lines (16 `useEffect`, 10 `useRef`, 8 `useState`) |
| `apps/desktop/src/SettingsWindow.tsx` | 1,405 | 14 top-level components; sections in one file |
| `apps/desktop/src/ConnectorsWindow.tsx` | 1,243 | 14 top-level components |
| `apps/desktop/src/ModelConfigDialog.tsx` | 1,109 | |
| `apps/desktop/src/panels/SessionPanel.tsx` | 1,104 | |

Crate sizes (src / tests lines): `imperium-core` 36,650 / 18,260; `imperium-local` 20,678 /
4,576; `imperium-engine` 17,676 / 16,283; `imperium-cli` 9,856 / 1,619; `imperium-config`
9,053 / 0. `apps/desktop/src` has 58 loose files at its root next to `panels/`,
`components/`, `layout/`, `mods/`, `commands/` and `daemon/`.

Not split: files of 1,000–2,000 lines that hold one cohesive thing (`initiative.rs`,
`browser.rs`, `vllm_env.rs`, `oauth.rs`, `secrets_file.rs`).

### Duplicated helpers

Only copies with **identical semantics** are merged; the others are listed so the reviewer can
see why they stay.

| Helper | Merged | Kept apart, and why |
| ------ | ------ | ------------------- |
| `hex(&[u8])` | all 5: `media/attachments.rs`, `workflow/update.rs`, `core/phone.rs`, `local/runtime.rs`, `local/hf.rs` (same body, lowercase `{:02x}`) | none (`core/tests/downloads.rs` is a test helper; it may use the shared one) |
| Unix time now | the sites whose failure fallback matches (`map_or(0, …)`/`unwrap_or(0)` of seconds or milliseconds); 30 sites in 28 files, 5 named `now_ms`, 1 `now_secs`, `workflow/update.rs::now` | sites that `expect` or take a `SystemTime` argument (`state/archive.rs::now_ms(now)`) keep their own handling around the shared call |
| `home_dir()` | **none**: the four differ | `workflow/imports.rs` (`HOME` then `USERPROFILE`, empty filtered, `PathBuf`); `core/extension_install.rs` (one variable chosen by OS, no filter); `tools/permissions.rs` (`HOME` then `USERPROFILE`, UTF-8 only, blank after trim filtered, `String`); `local/runtime_check.rs` (`USERPROFILE` first). Each gets a doc comment saying how it differs |
| Write a temporary file, then rename | 13 sites that do exactly `fs::write(temp)` then `fs::rename(temp, path)` with the temp beside the target: `core/{bench,budgets,config_changes,environments,initiative,mod_install,model_config}.rs`, `config/registry.rs`, `workflow/update.rs`, `engine/agents.rs`, `memory/sessions.rs`, `local/{vllm_env,calibration}.rs`. The helper takes the **temp path from the caller**, since names differ (`json.tmp`, `toml.tmp`, `tmp`) and tests check them | `memory/environment.rs::write_atomic` (creates the parent, `sync_all`); `core/writers.rs` (removes the temp when rename fails); `web/cache.rs` (unique temp names, stale-temp sweep); `config/secrets_file.rs` (private file mode, its own protocol); `core/{checkpoints,review}.rs` (git index files, not file replacement) |
| Clip text to a length | copies identical to `imperium_api::truncate_chars` | the rest (byte vs character limits, different suffixes) stay, renamed to say what differs |
| Defuse a tag inside framed text | none merged: four output styles | moved into one `imperium_api::framing` module as four named functions with byte-identical output: `cli/mcp_bridge.rs::defuse` (`&lt;`, case-insensitive), `engine/harness.rs::defuse` (`< tag`), `engine/knowledge.rs::escape_tag` (`<\/`, case-insensitive), `engine/instructions.rs::escape` (as fixed by decision 5); `engine/compact.rs::escape_item` and `web/tools.rs::escaped` (control characters) stay where they are |
| Model id → spec (desktop) | into two named functions, one per precedence: id first (`budgets.ts:16`, `modelAvailability.ts:68`, `modelOptions.ts:165`) and spec first (`telemetry.ts:41`, `modelOptions.ts:115`) | |
| Environment by id (desktop) | the 8 copies (`App.tsx` ×5, `NavigationPanels.tsx`, `NewSessionPanel.tsx`, `sessions.ts`) | |
| Model name → spec (daemon) | `core/budgets.rs::resolve_spec`, `core/onboarding.rs::spec_of` only if their fallbacks match; otherwise documented | |
| Workflow pack layering | `core/state.rs::context` and `cli/workflow.rs::resolve` (decision 1) | |

Backoff is not duplicated: `local/gpu_windows.rs::Backoff` (probe retries) and
`local/residency.rs::crash_backoff` (crash streaks) do different jobs and stay apart.

`imperium-workflow` depends on no Imperium crate today; taking the shared `hex` and file
helper makes it depend on `imperium-api`, which itself depends on no Imperium crate, so no
cycle appears.

### Interfaces bypassed or leaking

- **Errors matched by their text** in production code:
  - `cli/mods.rs:92,193` match `"trust was not granted"`, which `core/rpc.rs:686` builds with
    `format!`.
  - `core/voice.rs:57` tells a refused recording from a broken server by
    `error.to_string().contains("client error")`, the wording of `reqwest`'s status error that
    `imperium-media` passes through.
  - `engine/agent.rs:1106` detects a llama.cpp context overflow by
    `contains("Context size has been exceeded")`, in the agent loop instead of the client
    that talks to llama.cpp (`imperium-models`).
  - `apps/desktop/src/agentView.ts:55` matches `"no running agent"`, the `Display` text of
    `engine/queue.rs` `QueueError::UnknownAgent`, sent as a plain `-32602`.
- **Permission decisions composed by each caller**: see the
  [caller table](#every-caller-of-the-decision-code).
- **Compatibility re-exports** kept from earlier moves: `imperium-models` re-exports
  `ChatMessage`, `ToolSpec`, … from `imperium-api`; `imperium-engine` re-exports from
  `imperium-api` and `imperium-models`; `imperium-extensions` re-exports `markdown`;
  `imperium-tools::permissions` re-exports `PermissionRule` and friends.
- **Visibility.** `imperium-core` has 380 `pub` items against 259 `pub(crate)`, almost all in
  private modules. No crate enables `unreachable_pub`.
- **Deprecated protocol names still served** (decision 4: later, separately).

### Dead code, unused dependencies, stale docs

- **Unused dependencies** (no use in `src/`): `thiserror` in `imperium-automation` and
  `imperium-github`; `tracing` in `imperium-github`, `imperium-models` and
  `imperium-workflow`; `serde` in `imperium-cli` (may only enable a feature). Confirmed with
  `cargo machete` from its prebuilt release binary (it scans sources, it does not build), or a
  one-off CI step; never `cargo install` locally.
- **Suppressions:** one `#[allow(dead_code)]` (`core/state/branch.rs:31`); three
  `clippy::too_many_arguments`, left alone. Four `TODO`s in the tree.
- **Outdated docs:** [02](../02-architecture.md)'s crate table lists `imperium-sandbox`,
  `imperium-telemetry` and `imperium-remote` as if they existed (sandboxing lives in
  `imperium-tools`), says `imperium-engine` does not depend on `imperium-media` (it does, for
  attachments), and its repository layout lists `apps/mobile/`, `packages/ui/` and
  `sidecars/media/`, which do not exist, and omits `mods/`.

## Decision: the policy crate

[08](../08-agent-policy.md#default-policy-pack) deferred `imperium-policy` ("pack layering and
prompt assembly") to this cleanup, waiting for "a stable interface (pack list in, instruction
blocks out)". The user's request adds that policy *decisions* should sit behind one interface.

**Decision:** one new crate, `imperium-policy`, with no dependency on `imperium-config` (it
takes plain inputs). Dependencies: `imperium-api` and `imperium-workflow`. `imperium-tools`
depends on it; `imperium-engine`, `imperium-core`, `imperium-web` and `imperium-cli` import
from it. No cycle: `floor.rs`, `profile.rs` and the rule code use only each other and
`imperium-api` today.

### Packs

`packs::resolve(&PackInputs) -> ResolvedPacks`. Inputs: the workflow switches and listed
folders, the pinned packs from `PackStore`, **the bundled pack path as a caller-supplied
`Option<PathBuf>`**, and the project folder. Output: the `ContextIndex`, the personality
sources, and notices for skipped packs. It takes over the layering in
`DaemonState::context` and `personality_sources`.

Finding the bundled pack is not the policy crate's job. The daemon keeps `bundled_pack` and
`bundled_dirs` in `imperium-core` (they also find bundled `mods` and `pets`). The CLI gets its
own small lookup with the same order (`IMPERIUM_RESOURCE_DIR`, beside the executable, one
level up, the source tree in development builds) in `imperium-cli`, rather than a dependency
on `imperium-core`: the desktop sets `IMPERIUM_RESOURCE_DIR` only for the daemon it starts, so
the two can disagree, and `imperium workflow resolve` prints the bundled pack it used
(decision 1). Rejected: moving `bundled_dirs` to `imperium-config` (it would carry daemon
resource layout into the config crate for one CLI command); asking the daemon over the
protocol (a protocol change).

### Decisions

The pure kernel stays as it is: `decide_call(mode, kind, rule, floor) -> Decision`, and the
narrow functions `safety_floor`, `Permissions::evaluate`, `suggest`, `validate_rule`,
`validate_grant`, `protect_dir`, `expand_home`, `home_dir` keep their names and signatures
(moved, not changed).

One new method for the two places that compose all of it:
`Permissions::judge(&self, tool, target, kind, floor_override: Option<bool>) -> Judgement`,
where `Judgement { decision, rule, floored }`. It evaluates the rules for `tool`, takes the
floor from `floor_override` or else `safety_floor(kind, target)`, reads the mode, and calls
`decide_call`. It is a method on the existing `Permissions` type (which already holds the
rules and the mode) rather than a new `Policy` wrapper; the review called it `Policy::judge`.

**The agent profile check stays in the engine** (`agent.rs` 2207–2227 decides tool visibility
and refusals by tool source, which only the engine knows). Floor-only and rule-only callers
stay on the narrow functions.

What moves from `imperium-tools` to `imperium-policy`: `permissions.rs` (rules, modes,
`Permissions`, `SharedRules`, `SharedMode`, `Target`, `CallKind`, `Decision`, `Refusal`,
`decide_call`, `safety_floor`, `protect_dir` with its process-wide list, `evaluate`,
`suggest`, `validate_*`, `home_dir`, `expand_home`, `split_command`), `floor.rs`,
`profile.rs`. `imperium-tools` keeps `Workspace` and its path grants and imports the rest.

### Every caller of the decision code

| Caller | Inputs | Order today | Outcome used | After the move |
| ------ | ------ | ----------- | ------------ | -------------- |
| `engine/agent.rs::run_builtin` 2451–2498 | tool, target from the invocation, kind from its access | read-only-mount refusal, then rule, then `safety_floor(kind, target)`, then `decide_call`; a fork refuses any ask | `Decision`, `floored` (passed to `approve_with`), `suggest` | `judge(tool, target, kind, None)` |
| `engine/agent.rs::run_provider` 1852–1899 | provider check: command or not, `check.ask`, `always_asks` | rule for `run_command` (command) or the tool; floor is `safety_floor(Execute, …)` for a command, `provider.always_asks(name)` otherwise; `decide_call`; a fork refuses any ask | `Decision`, `floored`, `suggest` unless floored without a command | `judge(rule_tool, target, kind, None)` for a command, `judge(name, Other, kind, Some(always_asks))` otherwise |
| `engine/agent.rs::approve`, budget gate 2137, MCP sampling 3768 | none from rules | asks directly; sampling and budget pass `floored = true` | approval | unchanged |
| `engine/agent.rs` 4903 (writer commit) | mode, rule for `git_commit` | not plan mode and no deny rule | commit or not | `evaluate` + `mode`, unchanged |
| `engine/agent.rs` 823–859, 2207–2227 (profile) | `AgentProfile` | tool visibility, then refusal by source | specs offered, refusal text | unchanged, in the engine |
| `engine` knowledge forks (`agent/fork.rs`) | `self.fork.is_some()` | after `decide_call`: an ask becomes `fork::ask_refusal` | refusal | unchanged, after `judge` |
| `engine/bridge.rs::check` | tool name, `allow_write` | bridge allowlist first, then the normal `run_builtin` path | refusal or run | unchanged |
| `tools/search.rs:138` | tool and `read_file`, resolved target | rule only: deny or ask withholds the match | withheld count | `evaluate`, unchanged |
| `tools/workspace.rs` | grants, `expand_home` | path confinement | resolved paths | imports from `imperium-policy` |
| `web/tools.rs:403` (downloads) | `Edit`, saved path | writable check, then floor only | refuse to save | `safety_floor`, unchanged |
| `core/writers.rs:347` | `Edit`, staged names | floor only | names held back from the commit | `safety_floor`, unchanged |
| `core/review.rs:631` | tool, `Target::Other` | rule only | allowed, ask, refused | `evaluate`, unchanged |
| `core/permissions.rs` | wire rules and grants | `validate_rule`, `validate_grant`, `home_dir`; `Permissions`, `SharedRules`; profiles | stored rules, RPC errors | imports change only |
| `core/state.rs` 168–174 | config and data folders | `protect_dir` at start-up | floor guards mods, plugins, packs | imports change only |
| `core/state.rs` 550, 568, 1882 | environment grants | `PathGrant` construction | workspace grants | imports change only (`PathGrant` is `imperium_api`'s) |
| `core/state.rs` 1725 | config rules | `validate_rule` | notices | imports change only |
| `core/state.rs` 621, 1762, 1861–1865, 2771–2804 | `AgentProfile` | MCP and plugin filters | session setup | imports change only |

Tests that import the moved items and change imports only: `core/tests/{protected_dirs,
parallel_writers,bridge_tools}.rs`, `engine/tests/{session,preview,budget_gate,
instruction_files,agent_profile}.rs`, and the in-file tests of `permissions.rs`, `floor.rs`,
`profile.rs` (which move with their code).

**Prompt rendering stays in `imperium-engine`** (`root_prompt`, `harness_prompt`,
`subagent_prompt`, `instructions.rs`): model-visible text tied to `Agent` state.

Rejected:

| Option | Why not |
| ------ | ------- |
| Packs only, as 08 described | Leaves decision composition in two engine sites unshared |
| One `Policy::decide(&Call)` covering profile, rules, floor and mode | Over-reaches: the profile check is tool-visibility logic only the engine has, and the floor-only and rule-only callers would be forced through a call shape they do not need |
| A facade module inside `imperium-tools` | Keeps pack layering in `imperium-core` and the CLI copy |
| Two crates (`imperium-policy`, `imperium-permissions`) | Two tiny crates for one concept |

## Commit sequence

One pull request, merged with a merge commit; eight stage groups, about 25 commits. Shared
helpers come before the big moves that use them; the large mechanical moves, which conflict
with everything, come last and quickly; the supervisor split is last of all and optional.

**Preconditions (the freeze):** #189 merged (done, `a08de87`); the separate escaping fix
(decision 5) merged; Dependabot #159 (Cargo dependency bumps against `main`, touching
`Cargo.lock`) merged into `main` and synced into `experimental` first, or closed and re-opened
after this pull request, so group A's `Cargo.lock` change does not conflict with it; no other
open pull request against `experimental` touches `crates/` or `apps/desktop/src/`. Branch
`chore/cleanup` from the newest `experimental`.

### Group 0: guard tests (adds tests only)

1. Golden tests pinning today's output of every defusing function, and of the pack layering in
   `DaemonState::context` (no packs, listed packs, pinned packs, name collision, bundled
   default off).
2. A decision matrix over `decide_call` × floor (true/false) × rule (none/allow/ask/deny) for
   every `CallKind` and mode, plus the two `judge` input shapes (floor computed, floor
   overridden). The agent profile is not in it (it stays in the engine and has its own tests).
3. Snapshots checked by `scripts/check-rust.sh` like the generated TypeScript: the CLI help,
   built from `Cli::command()` (the command is named `imperium` already) with every
   subcommand's long help, generated by a unit test in `imperium-cli/src/main.rs` that runs
   on Linux only so platform-specific arguments cannot make it flaky; and the list of protocol
   method names. The first CI run prints the files; they are committed from the log.

### Group A: hygiene

1. Remove the unused dependencies confirmed by `cargo machete`; `Cargo.lock` follows.
2. Resolve the `#[allow(dead_code)]` in `state/branch.rs` (use or delete the field).

(Removing the compatibility re-exports is folded into D1, which rewrites the same imports.)

### Group B: shared helpers in `imperium-api`

`imperium-api` already holds `truncate_chars` and `markdown` and depends on no Imperium crate,
so the helpers go there (a new `imperium-util` crate was rejected: one more crate for about 150
lines).

1. `time::{unix_ms, unix_secs}` for the sites with a matching fallback.
2. `encoding::hex` (all five copies); `fs::replace_file(path, temp, bytes)` for the 13
   identical temp-then-rename sites, each keeping its temp name and its own error mapping.
   `home_dir` copies stay (all differ) and gain comments saying how.
3. `text::clip`: only copies identical to `truncate_chars`.
4. `framing`: the four tag-defusing functions moved under their current names (move commit),
   then given descriptive names (edit commit); group-0 golden tests unchanged.

### Group C: typed errors instead of text

1. `imperium-media`: transcription returns an error that keeps the HTTP status;
   `core/voice.rs` checks `is_client_error()`. A test pins equivalence with the old text check
   against a fake server: a 400 does not mark the server failed, a 500 does, a 200 with an
   unreadable body does.
2. `imperium-models`: `ChatError::is_context_overflow()` owns the llama.cpp wording;
   `agent.rs` calls it.
3. `"trust was not granted"` and `"no running agent"` become named constants in
   `imperium-protocol`, used by the daemon, the engine and the CLI, and one constant in the
   desktop with a test pinning the same literal. No wire change.

### Group D: `imperium-policy`

1. Create the crate and **move** `permissions.rs`, `floor.rs`, `profile.rs` out of
   `imperium-tools` under their current names; every consumer's imports updated, including
   dropping the compatibility re-exports in `imperium-models`, `imperium-engine`,
   `imperium-extensions` and `imperium-tools::permissions` (folded in from group A, since the
   same imports change).
2. Add `Permissions::judge` and route the two engine composition sites through it; the
   group-0 matrix passes unchanged.
3. Add `packs::resolve` with the bundled pack as a parameter; `core/state.rs` calls it.
4. `cli/workflow.rs` calls `packs::resolve` with its own bundled-pack lookup and prints the
   pack it used (decision 1, the one behaviour change in the pull request, in its own commit
   with a test).
5. Docs: [02](../02-architecture.md) crate table and dependency paragraph,
   [08](../08-agent-policy.md) status, [05](../05-tools-and-isolation.md) where it names
   `imperium-tools` as the home of rules and the floor, and the CLI reference for
   `workflow resolve`.

### Group E: module splits (move-only)

Each split is one move commit plus, if needed, one commit for the visibility it requires. Child
modules may hold further `impl` blocks of the parent's type and see its private fields, so no
field becomes more visible.

1. `imperium-protocol/src/methods.rs` → `methods/{sessions,checkpoints,branching,review,models,
   environments,mods,connectors,config_changes,…}.rs`, re-exported from `methods` (its public
   surface, not a shim). Generated TypeScript must not change.
2. `imperium-engine/src/agent.rs` → `agent/{turn,tools,specs,delegation,memory,compaction,
   prompts,sampling}.rs` beside the existing `agent/attachments.rs` and `agent/fork.rs`.
3. `imperium-core/src/state.rs` → more `state/` modules (`sessions`, `create`, `environments`,
   `extensions`); `rpc.rs` → `rpc/` with one dispatch function per method group and the
   `*_error` mappings in `rpc/errors.rs`.
4. Visibility: a probe CI run with `unreachable_pub = "warn"` (not denied) lists the findings;
   the commit narrows those items to `pub(crate)` and enables the lint for library crates.

### Group F: desktop

1. `models/spec.ts` with the two lookups (id first, spec first) and `environments/byId`; the
   copies call them.
2. `App.tsx`: extract hooks (`useSessionStore`, `useEnvironments`, `useWindows`, `useTour`)
   and `ConnectionBanner`; `App()` keeps layout and wiring.
3. `SettingsWindow.tsx`, `ConnectorsWindow.tsx`, `ModelConfigDialog.tsx`: one file per section
   under `settings/`, `connectors/`, `models/`.
4. Regroup the loose files into feature folders (`sessions/`, `models/`, `environments/`,
   `settings/`, `tour/`, `review/`, `shared/`); a move commit, then the import updates in
   `apps/desktop/test/`.

### Group G: documentation

[02](../02-architecture.md) crate table and repository layout corrected to what exists;
[07](../07-desktop-ui.md) where it names desktop files; [12](../12-roadmap.md) item 46 marked
implemented; every owner document touched by a stage checked against the final code.

### Group H: supervisor split (last, optional)

`imperium-local/src/supervisor.rs` → `supervisor/{launch,slots,presence,plan,swap,kept,
local_models}.rs` and `supervisor/tests.rs`, move-only. Dropped from the pull request, and
left for later, if review capacity runs short; nothing else depends on it.

## Non-goals

- No new features and no fixes of behaviour bugs found on the way (listed in the pull request,
  filed separately); the one exception is decision 1.
- No protocol change: no new error codes or error data, no removal of deprecated names, no
  version bump.
- No config schema change, no change to files written on disk, no dependency upgrades.
- No change to prompts or other model-visible text.
- No work on `policy/agent-custom-workflow`, `pets/`, `mods/` content, or Emperor and Consul.
- No CI workflow change; only `scripts/check-rust.sh` gains the snapshot drift check.

## Acceptance

Base counts, from CI on `a08de87` (run 43697124712): the Ubuntu Rust job ran **2,103 passed
and 1 ignored** across 142 test binaries; `apps/desktop` `node --test` ran **333**;
`mods/imperium-nursery` ran **87**. Statically the tree has 1,778 `#[test]`/`#[tokio::test]`
attributes in `crates/` and `apps/`.

- CI green on the pull request head: Rust on Ubuntu, Windows and macOS (`cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), web
  (lint, typecheck, test, build), docs (markdownlint, links), actionlint.
- `git diff origin/experimental -- packages/protocol/src/generated crates/imperium-protocol/schemas`
  is empty; `PROTOCOL_VERSION` unchanged; the method-name snapshot unchanged.
- The CLI help snapshot is unchanged.
- Test counts not reduced: the Ubuntu Rust job's `passed` + `ignored` is at least 2,104 plus
  the tests the pull request adds, and the desktop and nursery `node --test` counts at least
  333 and 87. Tests move with their code and are never deleted.
- Group-0 golden and matrix tests pass unchanged after groups B and D.
- Every move commit shows no changed lines under `--color-moved` apart from `use`, `mod`,
  `pub`/`pub(crate)` and `Cargo.toml` lines.
- Desktop: root runs a browser check of the main window, Settings, Connectors and the model
  dialog after group F (the desktop has no UI tests for these screens).
- The pull request is merged with a merge commit.

## Risks

| Risk | Mitigation |
| ---- | ---------- |
| Every compile error costs a CI round trip (no local builds) | Mechanical moves, small groups, push per group; `cargo fmt` locally |
| Conflicts with work merged meanwhile | The freeze preconditions; groups E, F and H last and fast; rebase, not merge |
| Defusing or framing output drifts (security relevant) | Golden tests first; four named functions, not one parametrised one |
| Permission decision order drifts | Decision matrix first; `judge` is a composition of today's functions; the caller table is the review checklist |
| `protect_dir`'s process-wide list moves crates | It moves with the floor unchanged, and the tests that rely on it move with it |
| The CLI finds a different bundled pack than the daemon | Printed by `workflow resolve`; documented (decision 1) |
| Platform-only code (`gpu_windows.rs`, the named pipe in `imperium-ipc`) compiles only on its runner | All three OS jobs required |
| ts-rs output changes when types move | The drift check fails on any change; moved types keep their names and attributes |
| Review fatigue on a very large diff | Move-only commits; critics per group; group H optional |

## Review split

Root assigns one independent critic per group, after that group's CI is green:

- **Groups 0, A, B, C:** helpers and errors, with a security lens on framing and defusing.
- **Group D:** architecture and security lens, using the caller table: the decision order,
  the floor, pack trust (listed packs trusted, pinned and bundled not), the CLI change.
- **Group E (and H):** move verification: every move commit is move-only, visibility changes
  are the minimum the split needs.
- **Group F:** desktop critic plus root's browser check.
- **Group G:** docs against the final code, folded into the last critic.

## Open questions

None left from round 1; the decisions are recorded [above](#decisions-already-made). Anything
new found while implementing goes to root before it changes scope.
