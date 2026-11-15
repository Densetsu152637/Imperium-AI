# Final cleanup: plan

**Status:** planned. This is the last roadmap item ([12](../12-roadmap.md), item 46). It is one
pull request against `experimental`, opened only after every other roadmap pull request has
merged (the last one open while this plan was written is #189, WebAssembly pet behaviours).
This document is the plan for that pull request: what is wrong today and the evidence for it,
the decisions, the order of commits, what is out of scope, how the result is checked, and the
questions the user still has to answer.

Figures were measured on `experimental` at `df252da` (roadmap 28 merged). Line counts include
in-file test modules. Measure them again when the work starts.

## Goal and rules

Tidy the repository, split oversized modules, remove duplicated helpers, and route callers
through proper interfaces, **without changing behaviour**.

- **No behaviour change.** Model-visible text, files written to disk, CLI output, and timing
  stay as they are. The one proposed exception is named in [Q1](#open-questions) and is made
  only if the user agrees.
- **No protocol change.** `PROTOCOL_VERSION` stays at 0.41, the generated TypeScript in
  `packages/protocol/src/generated/` and the tool schemas in `crates/imperium-protocol/schemas/`
  stay byte-identical. The one optional protocol addition is [Q3](#open-questions).
- **No compatibility shims.** Code that moves has every consumer updated in the same commit; no
  `pub use` left at the old path only so old imports keep working.
- **Move commits and edit commits are separate**, so a reviewer can read a move with
  `git diff --color-moved=dimmed-zebra --color-moved-ws=allow-indentation-change` and see that
  nothing changed inside it.
- **No local Rust builds.** CI is the only compiler, so the branch is pushed after each stage
  group and the next group starts only when CI is green (`cargo fmt` runs locally).

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
| `apps/desktop/src/SettingsWindow.tsx` | 1,398 | 14 top-level components; sections in one file |
| `apps/desktop/src/ConnectorsWindow.tsx` | 1,243 | 14 top-level components |
| `apps/desktop/src/ModelConfigDialog.tsx` | 1,109 | |
| `apps/desktop/src/panels/SessionPanel.tsx` | 1,104 | |

Crate sizes (src / tests lines): `imperium-core` 36,650 / 18,260; `imperium-local` 20,678 /
4,576; `imperium-engine` 17,676 / 16,283; `imperium-cli` 9,856 / 1,619; `imperium-config`
9,053 / 0. `apps/desktop/src` has about 40 loose `.ts`/`.tsx` files at its root next to
`panels/`, `components/`, `layout/`, `mods/`, `commands/` and `daemon/`.

Not split: files of 1,000–2,000 lines that hold one cohesive thing (`initiative.rs`,
`browser.rs`, `vllm_env.rs`, `oauth.rs`, `secrets_file.rs`); splitting them buys little.

### Duplicated helpers

| Helper | Copies | Where |
| ------ | -----: | ----- |
| Unix time now (`duration_since(UNIX_EPOCH)`) | 30 sites in 28 files; 5 named `now_ms`, 1 `now_secs` | `core/{checkpoints,initiative,connectors,state/archive}.rs`, `engine/agents.rs`, `mcp/oauth.rs`, … |
| `hex(&[u8])` | 5 | `media/attachments.rs`, `workflow/update.rs`, `core/phone.rs`, `local/runtime.rs`, `local/hf.rs` |
| `home_dir()` | 4 | `workflow/imports.rs`, `core/extension_install.rs`, `local/runtime_check.rs`, `tools/permissions.rs` (returns `String`; the others `PathBuf`) |
| Write to a temporary file, then rename | 28 sites in 21 files; 2 named `write_atomic` | `workflow/update.rs`, `memory/environment.rs`, `config/secrets_file.rs`, `web/cache.rs`, … |
| Clip text to a length | 1 shared (`imperium_api::truncate_chars`) and 7 local | `cli/{changes,mcp_serve}.rs`, `core/scheduler.rs`, `local/card.rs`, `engine/{agent,knowledge,harness}.rs` |
| Defuse a tag inside framed text | 6, in four different output styles | `cli/mcp_bridge.rs::defuse` (`&lt;`, case-insensitive), `engine/harness.rs::defuse` (`< tag`), `engine/knowledge.rs::escape_tag` (`<\/`, case-insensitive), `engine/instructions.rs::escape` (`<\/`, **case-sensitive**), `engine/compact.rs::escape_item`, `web/tools.rs::escaped` (control characters) |
| Model id → spec (desktop) | 4, two precedences | `budgets.ts:16`, `modelAvailability.ts:68`, `modelOptions.ts:165` look up by id first; `telemetry.ts:41` and `modelOptions.ts:115` by spec first |
| Model name → spec (daemon) | 2 | `core/budgets.rs::resolve_spec`, `core/onboarding.rs::spec_of` |
| Environment by id (desktop) | 8 | `App.tsx` (5), `NavigationPanels.tsx`, `NewSessionPanel.tsx`, `sessions.ts` |
| Workflow pack layering | 2, **diverged** | `core/state.rs::context` (2681–2730) and `cli/workflow.rs::resolve`; see [Q1](#open-questions) |

Backoff is not duplicated: `local/gpu_windows.rs::Backoff` (probe retries) and
`local/residency.rs::crash_backoff` (crash streaks) do different jobs and stay apart.

### Interfaces bypassed or leaking

- **Errors matched by their text** in production code:
  - `cli/mods.rs:92,193` match `"trust was not granted"`, which `core/rpc.rs:686` builds with
    `format!`; nothing ties the two strings together except a test.
  - `core/voice.rs:57` tells a refused recording from a broken server by
    `error.to_string().contains("client error")`, the wording of `reqwest`'s status error.
  - `engine/agent.rs:1106` detects a llama.cpp context overflow by
    `contains("Context size has been exceeded")`, inside the agent loop instead of the client
    that talks to llama.cpp (`imperium-models`).
  - `apps/desktop/src/agentView.ts:55` matches `"no running agent"`, the `Display` text of
    `engine/queue.rs` `QueueError::UnknownAgent`, sent as a plain `-32602`.
- **Permission decisions composed by each caller.** `imperium-tools::permissions` exposes the
  parts (`evaluate`, `safety_floor`, `decide_call`, `AgentProfile`) and callers assemble them:
  `engine/agent.rs` 1774, 1855–1869 and 2471–2472 (rule, floor, mode), 2207–2227 (profile);
  `web/tools.rs:403` and `core/writers.rs:347` call the floor alone; `core/review.rs:631` and
  `engine/agent.rs:4903` evaluate rules alone. The order (deny rule, plan mode, floor and ask
  rules, mode table) lives in one function, but whether the floor and profile are consulted at
  all is up to each caller.
- **Compatibility re-exports** kept from earlier moves: `imperium-models` re-exports
  `ChatMessage`, `ToolSpec`, … from `imperium-api`; `imperium-engine` re-exports from
  `imperium-api` and `imperium-models`; `imperium-extensions` re-exports `markdown`;
  `imperium-tools::permissions` re-exports `PermissionRule` and friends.
- **Visibility.** `imperium-core` has 380 `pub` items against 259 `pub(crate)`, almost all in
  private modules, so `pub` there says nothing. No crate enables `unreachable_pub`.
- **Deprecated protocol names still served**: `persistent_agent`/`agent_session_id` (deprecated
  in 0.22, 36 references) and the old memory scope names that 0.31 accepted "in this version
  only". Removing them is a protocol change and out of scope ([Q4](#open-questions)).

### Dead code, unused dependencies, stale files

- **Unused dependencies** (no use in `src/`, to confirm with `cargo machete`, which scans
  without building): `thiserror` in `imperium-automation` and `imperium-github`; `tracing` in
  `imperium-github`, `imperium-models` and `imperium-workflow`; `serde` in `imperium-cli`
  (may only enable a feature; check before removing).
- **Suppressions:** one `#[allow(dead_code)]` (`core/state/branch.rs:31`); three
  `clippy::too_many_arguments`, left alone. Four `TODO`s in the tree.
- **Root strays:** `plan.md`, `questions.md` and `emperor-root.md` are **not tracked** (the
  tracked root holds only workspace and tooling files), so the pull request has nothing to
  delete; see [Q8](#open-questions).
- **Outdated docs:** [02](../02-architecture.md)'s crate table lists `imperium-sandbox`,
  `imperium-telemetry` and `imperium-remote` as if they existed (sandboxing lives in
  `imperium-tools`), says `imperium-engine` does not depend on `imperium-media` (it does, for
  attachments), and its repository layout lists `apps/mobile/`, `packages/ui/` and
  `sidecars/media/`, which do not exist, and omits `mods/`.

## Decision: the policy crate

[08](../08-agent-policy.md#default-policy-pack) deferred `imperium-policy` ("pack layering and
prompt assembly") to this cleanup, waiting for "a stable interface (pack list in, instruction
blocks out)". The user's request adds that policy *decisions* (permission rules, safety floor,
permission mode, agent profile) should sit behind one interface too.

**Decision:** one new crate, `imperium-policy`, with two modules and no dependency on
`imperium-config` (it takes plain inputs):

- `packs`: `resolve(&PackInputs) -> ResolvedPacks`. Inputs: the workflow switches and listed
  folders, the pinned packs from `PackStore`, the bundled pack path, the project folder.
  Output: the `ContextIndex`, the personality sources, and notices for skipped packs. It
  takes over `DaemonState::context`, `personality_sources` and `bundled_pack` from
  `core/state.rs`, and the CLI's `workflow resolve` calls the same function.
- `decide`: the pure decision layer moved out of `imperium-tools`: `permissions.rs` minus path
  enforcement, `floor.rs`, `profile.rs`. One entry point,
  `Policy::decide(&self, call: &Call) -> Decision`, runs profile, rules, floor and mode in the
  order `decide_call` and `agent.rs` use today; `floor::covers(kind, target)` stays public for
  the two floor-only callers, and `protect_dir` with its process-wide list moves with the
  floor. `imperium-tools` depends on `imperium-policy` and keeps the enforcement side
  (`Workspace` and its path grants).

Dependencies: `imperium-policy` depends on `imperium-api` and `imperium-workflow`;
`imperium-tools` depends on `imperium-policy`; `imperium-engine`, `imperium-core`,
`imperium-web` and `imperium-cli` import decisions and pack resolution from it. No cycle:
`floor.rs`, `profile.rs` and the rule code use only each other and `imperium-api` today, and
`imperium-workflow` depends on no Imperium crate.

**Prompt rendering stays in `imperium-engine`** (`root_prompt`, `harness_prompt`,
`subagent_prompt`, `instructions.rs`). It is model-visible text tied to `Agent` state; moving
it gains nothing behaviour-preserving and risks drift. Revisit if a second consumer appears.

Rejected:

| Option | Why not |
| ------ | ------- |
| Packs only, as 08 described | Leaves decision composition spread over five callers, which the user asked to fix |
| A facade module inside `imperium-tools` | Keeps pack layering in `imperium-core` and the CLI copy; no single owner for policy |
| Two crates (`imperium-policy`, `imperium-permissions`) | Two tiny crates for one concept; the user named one |
| Move prompt assembly as well | Behaviour risk with no consumer gain (above) |

## Commit sequence

One pull request, seven stage groups, about 25 commits. Groups run in this order so that
shared helpers exist before the big moves use them, and the large mechanical moves, which
conflict with everything, happen last and quickly. Each group ends with a push and a green CI
run before the next starts.

**Before starting:** #189 merged; no other open pull request against `experimental` touches
`crates/` or `apps/desktop/src/` (announce a short freeze); branch `chore/cleanup` from the
newest `experimental`.

### Group 0: guard tests (adds tests only)

1. Golden tests that pin today's output of every defusing function and of the pack layering
   (`DaemonState::context` for: no packs, listed packs, pinned packs, name collision, bundled
   default off).
2. A decision matrix test over `decide_call` × floor × profile for every `CallKind` and mode.
3. Snapshots checked by `scripts/check-rust.sh` like the generated TypeScript: `imperium --help`
   and every subcommand's help, and the list of protocol method names. The first CI run prints
   the files; they are committed from the log (no local build).

### Group A: hygiene

1. Remove the unused dependencies confirmed by `cargo machete`; `Cargo.lock` follows.
2. Point imports at the owning crate and delete the compatibility re-exports (`imperium-models`,
   `imperium-engine`, `imperium-extensions`, `imperium-tools::permissions`).
3. Resolve the `#[allow(dead_code)]` in `state/branch.rs` (use or delete the field).

### Group B: shared helpers in `imperium-api`

`imperium-api` already holds `truncate_chars` and `markdown` and depends on no Imperium crate,
so the helpers go there (a new `imperium-util` crate was rejected: one more crate for about 150
lines).

1. `time::{unix_ms, unix_secs}`; replace the 30 sites whose failure handling matches (sites
   that `expect` or treat failure differently keep their own handling around the call).
2. `encoding::hex`, `paths::home_dir` (the `String` form in `imperium-tools` becomes a call
   plus `to_string_lossy`), `fs::write_atomic` (same-folder temporary file, then rename; each
   crate maps the `io::Error` into its own error type as it does now).
3. `text::clip`: merge only copies with identical semantics (characters vs bytes, suffix);
   others stay, renamed to say what differs.
4. `framing`: the four defusing styles as four named functions with their exact current output;
   the group-0 golden tests must pass unchanged. The case-sensitivity gap in
   `instructions.rs::escape` is **not** fixed here ([Q5](#open-questions)).

### Group C: typed errors instead of text

1. `imperium-media`: transcription returns a typed error carrying the HTTP status;
   `core/voice.rs` checks `is_client_error()`.
2. `imperium-models`: `ChatError::is_context_overflow()` owns the llama.cpp wording;
   `agent.rs` calls it.
3. The `"trust was not granted"` and `"no running agent"` texts become named constants in
   `imperium-protocol` used by the daemon and the CLI, and one constant in the desktop with a
   test that pins the same literal. No wire change; [Q3](#open-questions) offers the typed
   alternative.

### Group D: `imperium-policy`

1. Create the crate; move `floor.rs`, `profile.rs` and the decision half of `permissions.rs`
   (move-only commit; every consumer's imports updated; no re-export left in
   `imperium-tools`).
2. Add `Policy::decide` and route `agent.rs`'s three composition sites, `web/tools.rs`,
   `core/writers.rs`, `core/review.rs` through it; the group-0 matrix test passes unchanged.
3. Add `packs::resolve`; `core/state.rs` and `cli/workflow.rs` call it. Whether the CLI adopts
   the daemon's semantics is [Q1](#open-questions); without a yes, the CLI keeps its own
   behaviour through an explicit `PackInputs` option, documented as such.
4. Docs: [02](../02-architecture.md) crate table and dependency paragraph, [08](../08-agent-policy.md)
   status, [05](../05-tools-and-isolation.md) where it names `imperium-tools` as the home of
   rules and the floor.

### Group E: module splits (move-only)

Each split is one move commit plus, if needed, one commit for the visibility it requires. Child
modules may hold further `impl` blocks of the parent's type and see its private fields, so no
field becomes more visible.

1. `imperium-protocol/src/methods.rs` → `methods/{sessions,checkpoints,branching,review,models,
   environments,mods,connectors,config_changes,…}.rs`, re-exported from `methods`. Generated
   TypeScript must not change (ts-rs names files by type).
2. `imperium-engine/src/agent.rs` → `agent/{turn,tools,specs,delegation,memory,compaction,
   prompts,sampling}.rs` beside the existing `agent/attachments.rs` and `agent/fork.rs`.
3. `imperium-core/src/state.rs` → more `state/` modules (`sessions`, `create`, `environments`,
   `extensions`); `rpc.rs` → `rpc/` with one dispatch function per method group and the
   `*_error` mappings in `rpc/errors.rs`.
4. `imperium-local/src/supervisor.rs` → `supervisor/{launch,slots,presence,plan,swap,kept,
   local_models}.rs` and `supervisor/tests.rs`.
5. Visibility: a probe CI run with `unreachable_pub = "warn"` (not denied) lists the findings;
   the commit narrows those items to `pub(crate)` and enables the lint for library crates only
   ([Q6](#open-questions)).

### Group F: desktop

1. `modelSpec.ts` with `specById` and `specOrId` (the two precedences that exist); the four
   copies call them. `environmentById` likewise.
2. `App.tsx`: extract hooks (`useSessionStore`, `useEnvironments`, `useWindows`, `useTour`)
   and `ConnectionBanner`; `App()` keeps layout and wiring.
3. `SettingsWindow.tsx` and `ConnectorsWindow.tsx`: one file per section under `settings/` and
   `connectors/`.
4. If [Q7](#open-questions) is a yes, group the loose `src/*.ts` files into `models/`,
   `sessions/`, `environments/`, `settings/`; test imports follow.

### Group G: documentation

[02](../02-architecture.md) crate table and repository layout corrected to what exists;
[12](../12-roadmap.md) item 46 marked implemented; every owner document touched by a stage
checked against the final code.

## Non-goals

- No new features, no fixes of behaviour bugs found on the way (they are listed in the pull
  request and filed separately).
- No protocol change: no new error codes, no removal of deprecated names, no version bump.
- No config schema change, no change to files written on disk, no dependency upgrades.
- No change to prompts or other model-visible text.
- No work on `policy/agent-custom-workflow`, `pets/`, `mods/` content, or Emperor and Consul.
- No CI workflow change; only `scripts/check-rust.sh` gains the snapshot drift check.

## Acceptance

- CI green on the pull request head: Rust on Ubuntu, Windows and macOS (`cargo fmt --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`), web
  (lint, typecheck, test, build), docs (markdownlint, links), actionlint.
- `git diff origin/experimental -- packages/protocol/src/generated crates/imperium-protocol/schemas`
  is empty; `PROTOCOL_VERSION` unchanged; the method-name snapshot unchanged.
- The CLI help snapshot is unchanged.
- Test count not reduced: the Ubuntu Rust job's summed `passed` + `ignored` is at least the
  count on the base commit, and the desktop's `node --test` count likewise. Statically the tree
  has 1,778 `#[test]`/`#[tokio::test]` attributes in `crates/` and `apps/`; tests move with
  their code and are never deleted.
- Group-0 golden and matrix tests pass unchanged after groups B and D.
- Every move commit shows no changed lines under `--color-moved`.
- Desktop: root runs a browser check of the main window, Settings and Connectors after group F
  (the desktop has no UI tests for these screens).

## Risks

| Risk | Mitigation |
| ---- | ---------- |
| Every compile error costs a CI round trip (no local builds) | Mechanical moves, small groups, push per group; `cargo fmt` locally |
| Conflicts with work merged meanwhile | Start after #189; short freeze; groups E and F last and fast; rebase, not merge |
| Defusing or framing output drifts (security relevant) | Golden tests first (group 0); four named functions, not one parametrised one |
| Permission decision order drifts | Decision matrix test first; `Policy::decide` is a composition of today's functions |
| `protect_dir`'s process-wide list (read by the floor) moves crates | It moves with the floor unchanged, and the tests that rely on it move with it |
| Platform-only code (`gpu_windows.rs`, the named pipe in `imperium-ipc`) only compiles on its runner | All three OS jobs required |
| ts-rs output changes when types move | The drift check fails on any change; moved types keep their names and attributes |
| Review fatigue on a very large diff | Move-only commits; critics per group (below) |

## Review split

Root assigns one independent critic per group, after that group's CI is green:

- **Groups 0, A, B, C:** helpers and errors, with a security lens on framing and defusing.
- **Group D:** architecture and security lens: the decision order, the floor, profiles, pack
  trust (listed packs trusted, pinned and bundled not).
- **Group E:** move verification: every move commit is move-only, visibility changes are the
  minimum the split needs.
- **Group F:** desktop critic plus root's browser check.
- **Group G:** docs against the final code, folded into the last critic.

## Open questions

1. **CLI pack layering.** `imperium workflow resolve` does not match what a session loads: it
   ignores the bundled default pack, fails on an unreadable pack where the daemon skips it with
   a notice, marks no listed pack as trusted, and has no name-collision check for pinned packs.
   Align it with the daemon in group D (a named behaviour change to a diagnostic command), or
   keep it as it is? Recommended: align.
2. **Policy crate scope.** One crate holding pack layering and call decisions, with prompt
   rendering left in the engine, as decided above. Agree?
3. **Errors matched by text.** Shared constants (no protocol change, recommended), or an
   additive `error.data.reason` such as `"trust_not_granted"` with a minor protocol bump to
   0.42?
4. **Deprecated protocol names** (0.22 `persistent_agent`/`agent_session_id`, 0.31 memory
   scope names). Remove them in a separate breaking-protocol pull request after the cleanup?
5. **Case-sensitive defusing in `instructions.rs`.** `</INSTRUCTION-FILE>` in a project file is
   not escaped, unlike the other frames. Fix it in a small security pull request before the
   cleanup (recommended), or leave it?
6. **Visibility lint.** Enable `unreachable_pub` for library crates only (recommended), the
   whole workspace, or skip it?
7. **Desktop folders.** Group the about 40 loose files in `apps/desktop/src` into folders, or
   only split the large components?
8. **Root strays.** `plan.md`, `questions.md`, `emperor-root.md` are untracked local files;
   delete them locally, or add them to `.gitignore`?
