# Plan: unified KV pool, per-model llama.cpp settings, and the model settings dialog

Revision: 2026-11-09, revised after the plan-gate review, on `feat/kv-pool-model-config` at base `74f9e21` (top of the stack #53 → #54 → #55 → #56); first draft `ae3994f`. Plan owner: root.

User decisions recorded on 2026-11-09: (a) an orchestrator in `await_tasks` may be swapped out by another session's load; (b) for remote models the dialog edits existing model-table rows only; (c) a local daemon build is allowed for root's G8 in-app check only, while clippy and tests stay in CI.

Goals (boolean; Rust checks run in CI only, see [Validation rules](#validation-rules)):

| Id | Goal | Verified by |
| -- | ---- | ----------- |
| G1 | The workspace's Rust checks pass (fmt, clippy `-D warnings`, `cargo test --workspace --locked`, generated TypeScript matches) | PR CI job `rust` (`scripts/check-rust.sh`) passes on the PR head |
| G2 | `llama-server` arguments contain `--kv-unified`, `--ctx-size <pool_tokens>`, `--parallel <slots>` and `--no-cache-idle-slots`, plus `--kv-unified-per-slot <context>` exactly when `context < pool_tokens` | Unit test `llama_server_shares_one_kv_pool` passes in CI job `rust` |
| G3 | The planner counts the KV cache once for the pool (`kv_bytes_per_token × pool_tokens`), independent of the slot count | Unit test `the_kv_cache_is_one_pool_whatever_the_slots` passes in CI job `rust` |
| G4 | Per-model llama.cpp settings saved through `models.set_config` persist in `<config>/models.toml`, survive a reload, reach the launch arguments, and restart a loaded model only once it is Free | Tests `per_model_settings_resolve_over_the_defaults`, `invalid_app_entries_are_dropped_at_load` (imperium-config), `per_model_settings_reach_the_launch_arguments`, `a_stale_model_restarts_only_when_free` (imperium-local), `set_config_saves_reloads_and_applies` (imperiumd) pass in CI job `rust` |
| G5 | No global cap: no limit across models or sessions; more sessions than one model's slots, and sessions across two models, all proceed (calls beyond a model's slots queue in Imperium and all complete, none is rejected); a full pool makes calls wait, not fail; the reservation bounds what a call may use | Tests `more_sessions_than_the_old_parallel_value_run_at_once`, `calls_beyond_a_models_slots_queue_and_all_complete`, `a_full_pool_makes_calls_wait_not_fail` (imperium-local), `local_calls_reserve_their_estimate_and_cap_the_reply`, `a_local_reply_cut_by_its_reservation_is_retried_once_uncapped` (imperium-core) pass in CI job `rust` |
| G6 | A load for model Y never evicts or interrupts model X while an agent of another session uses X (holds a call lease, or is mid-turn after its first call on X); Y waits and loads once X is released; idle unload spares such a model; an agent parked on an approval or a usage-limit pause holds no claim | Tests `a_load_never_evicts_a_model_another_session_is_using`, `idle_unload_spares_a_model_another_session_is_mid_turn_on`, `a_cpu_lane_model_another_session_is_using_is_never_evicted_for_ram`, `a_session_parked_on_an_approval_holds_no_claim` (imperium-local tests) and `claims_of_another_session_count_as_busy` (supervisor unit) pass in CI job `rust` |
| G7 | Concurrent loads of different models are serialised (at most one launch or stop at a time) and all finish, without deadlock | Test `concurrent_loads_take_turns_and_all_finish` passes in CI job `rust` |
| G8 | The desktop dialog lists local and model-table models, saves per-model settings through `models.set_config`, and the web checks pass | `bash scripts/check-web.sh` exits 0 locally (for `apps/desktop` this is typecheck and build only: the package has no lint or test script), and root's in-app check in Stage 4 against a daemon root builds locally (the one exception to CI-only Rust, confined to root) |
| G9 | Docs describe the shipped behaviour | `docs/02` (Stage 1), `docs/04`, `docs/10`, `docs/13` (Stage 2) and `docs/07` (Stage 3) updated; PR CI job `docs` passes |

Scope:

- `llama-server` runs with one shared KV cell pool per model (`--kv-unified`); the pool is budgeted in tokens; a full pool makes calls wait.
- Per-model llama.cpp launch settings (context per session, pool size, slots, K/V cache types, flash attention, GPU-layer cap, wait cap, extra arguments), stored in a new app-written config layer.
- No global cap on sessions using local models; per-model slot counts replace the global `parallel`.
- Usage locks: a model another session is using is never unloaded for a load, an idle timeout, or a settings change.
- RPC `models.config` / `models.set_config`, and a desktop dialog for local models and model-table rows.

Non-goals: adding or removing model-table rows or endpoints, editing API keys or base URLs, live reload of any other setting, a new runtime engine, vLLM KV dtype options, per-session context tiers, measuring on hardware.

Assumptions:

- The pinned runtime is llama.cpp `b12180` (`runtimes/llama-cpp.env`). Verified against that tag's `tools/server/README.md`, `tools/server/server-context.cpp` and `common/arg.cpp`: `-kvu, --kv-unified` exists ("use single unified KV buffer shared across all sequences"), and so does `--kv-unified-per-slot N` (caps each slot's context; the pool stays `--ctx-size`). Also present: `--cache-type-k/-v` (f32, f16, bf16, q8_0, q4_0, q4_1, iq4_nl, q5_0, q5_1), `-fa, --flash-attn on|off|auto`, `-np, --parallel`, `-ngl`, `--cache-ram`, `--slot-save-path`, and `--cache-idle-slots` / `--no-cache-idle-slots` (`common/arg.cpp`: "save idle slots to the prompt cache on new task, and clear them when using unified KV", default enabled when `--cache-ram` is set). `LLAMA_MAX_SEQ` is 256. The chat endpoint honours `max_tokens`.
- With a unified pool, `llama-server` first purges idle slots' cells when a batch finds no room (`try_clear_idle_slots`), then halves the batch; at batch size 1 it fails every running slot with "Context size has been exceeded." and clears them. Imperium must therefore keep running calls within the pool itself.
- Holders are `<session id>/<agent id>` (`Agent::holder` in `crates/imperium-core/src/agent.rs`); session ids are hex (`new_session_id`) and contain no `/`; subagents share their orchestrator's session id.
- Rust is not built or tested locally (user instruction): implementers may run `cargo fmt --all` and `cargo check --workspace --all-targets` only. Root alone may also build and run the daemon locally for the G8 in-app check (decision (c)).

Relevant paths: `crates/imperium-local/src/{supervisor,arbiter,planner,backend,slot_files,process}.rs`, `crates/imperium-local/tests/`, `crates/imperium-config/src/{settings,layers,paths}.rs`, `crates/imperium-api/src/local.rs`, `crates/imperium-core/src/{agent,model}.rs`, `crates/imperium-protocol/src/{methods,lib}.rs`, `crates/imperiumd/src/{rpc,state,local,load}.rs`, `packages/protocol/src/generated/`, `apps/desktop/src/`.

Related docs: [04 Runtimes](../04-models-and-routing.md#runtimes), [10 Layers](../10-configuration.md#layers), [07 Panels](../07-desktop-ui.md#panels), [13 Accepted](../13-decisions.md#accepted). This plan wins over them until Stage 2 and Stage 3 rewrite them; after that the docs win.

## Decision record

### D-A: one KV pool per model, budgeted in tokens

- Each local llama.cpp model is started with `--ctx-size <pool_tokens> --kv-unified --parallel <slots> --no-cache-idle-slots`, plus `--kv-unified-per-slot <context>` when `context < pool_tokens` (when the context is the whole pool the cap cannot bind, and leaving it out keeps the flag off older custom binaries in the common case). `context` is what one session (one slot) may use; `pool_tokens` is the KV cache all slots share. The planner counts `kv_bytes_per_token(K, V) × pool_tokens`; slots no longer multiply memory.
- `--no-cache-idle-slots`: with a unified pool and `--cache-ram`, llama-server's default moves an idle slot's prompt into the host prompt cache and clears the slot when another task starts. The swap-out path saves slots with `POST /slots/{id}?action=save`, which would then find the slots of agents asleep in `await_tasks` empty; the flag keeps an idle slot's cells until the pool runs short. The pinned-slot switch described in docs/04 "Slot affinity" (a pinned slot's previous prompt saved to the host cache by prompt similarity) is a separate path and still works. Residual degradation: under pool pressure llama-server still purges idle slots (`try_clear_idle_slots`), so a sleeping agent's slot can be empty when its model is swapped out; its prompt is then read again on return. That costs time but nothing breaks.
- The reservation is a hard bound on what a call may use:
  - The agent estimates the prompt `p`:
    - after the first call, the last call's exact `prompt_tokens + completion_tokens`, plus 1.25 × `estimate_tokens` of the messages added since, plus 8 tokens of chat-template overhead per added message;
    - on the first call, 1.25 × `estimate_tokens` of every message and of the serialised tool specs, plus 8 tokens per message and 64 for the template's frame.
  - The supervisor reserves `r = min(p + max_output_tokens, context)` tokens from the model's FIFO token semaphore (`acquire_many_owned`) before the call. A call that does not fit waits, bounded by `max_wait_s`, and a large call is not starved by small ones.
  - The lease tells the agent to send `max_tokens = r − p`, so llama-server cannot generate past the reservation.
  - `max_output_tokens` is per model; the default `runtime.llamacpp.max_output_tokens` is 8192. A value of 0 means uncapped: the call reserves the whole `context` and sends no `max_tokens`.
- Long generations keep working: when a capped local reply stops with `finish_reason = "length"`, the agent retries that call once uncapped (full `context`, no `max_tokens`). The same uncapped retry follows a "Context size has been exceeded." failure, which an underestimated prompt can still cause.
  - Text already streamed from the failed attempt is not re-emitted as if new: the agent emits `SessionEvent::Notice { message: "the local reply was cut off by its token reservation (or the KV pool was full); it starts again below" }` before the retry, and only the retry's completion becomes the assistant message.
  - Chosen over reserving the whole `context` for every call. That would also keep long generations working, but concurrency would fall back to `pool / context`, the old slot count, which is what item 1 removes.
- Idle slots' cached prompts are not reserved: llama-server purges them under pressure. The token pool applies to llama.cpp only. vLLM is not slot-aware and manages its own paged KV, so a vLLM lease reserves nothing and carries no `max_tokens`; `pool_tokens` only sizes its memory plan.
- Rejected: (1) keep `context × slots` (status quo; slots stay expensive, which is what item 1 removes). (2) Rely on llama-server alone: its overflow path fails every running call at once. (3) Reserve the full `context` per call (see above).
- Trade-off: a cut reply costs one wasted attempt; overestimates lower concurrency. Revisit if the retry shows up often in logs (then raise the default reserve or margin).

### D-B: no global cap; slots per model

- Interpretation of "no global cap": there is no limit across models or across sessions on how many sessions use local models. The per-model slot count is a configurable throughput knob (how many calls one server decodes at once), not an admission limit. A call beyond a model's slots or token pool is never rejected: it queues in Imperium, per model, first come first served (which keeps slot affinity and `id_slot` pinning working), until `max_wait_s`.
- The global `runtime.llamacpp.parallel` no longer caps anything. Each model has `slots`, validated `1..=256` (`LLAMA_MAX_SEQ`); its default is `runtime.llamacpp.slots` when set, else `max(8, runtime.llamacpp.parallel)`, so no existing configuration loses concurrency. The CPU lane defaults to 1.
- `parallel` otherwise only sizes the default pool: `pool_tokens = context × parallel` (8192 × 2 = 16384 with the defaults), so an existing configuration plans the same memory as today. The default is 8 slots, not more, because per-slot host state (context checkpoints of SWA/hybrid models, per-sequence state of recurrent models) is not modelled by the planner.
- Rejected: an unbounded `--parallel` (memory not modelled, latency of the user's call suffers); letting `llama-server` queue beyond its slots (loses Imperium's slot affinity and the interactive-before-background ordering).

### D-C: usage locks (item 5) and the arbiter

The memory-changing operations (load, swap-out, idle unload, settings unload) already run one at a time under `Supervisor::swapping` (a tokio mutex). What changes is which loaded models a load may take memory from. For a load requested by holder H of session S, a loaded model is:

| Use of the loaded model | May it be unloaded for H's load? |
| ----------------------- | -------------------------------- |
| Busy: a call lease held or a call queued on it | Never (unchanged) |
| Claimed by another session: an agent of a session other than S took a call on this load of the model and is mid-turn (attached, not in `await_tasks`) | Never: treated exactly as Busy |
| Claimed by S only | As today (#54): waited for up to `min_residency_s`, then loaded beside if it fits, swapped out only when nothing else fits and it has been resident `min_residency_s` |
| Free: every agent that used it is in `await_tasks`, waiting for an approval or a usage-limit pause, or done | Yes, with its slots saved (unchanged) |

- Idle unload and settings-change unload take Free models only (today idle unload also takes a model whose agent is mid-turn but between calls, such as during a 20-minute tool run; that stops).
- An agent parked on a person releases its claim the same way it does in `await_tasks`. While it waits for a permission answer (`Agent::approve`, around `answer.await`) or sits in a usage-limit pause (`controls.pause_until` in `run_turn`), it holds `local.waiting(model, holder)`. Otherwise an unanswered approval would lock a model against every other session indefinitely.
- Attach-to-first-call gap (documented, intended): a turn's `attach` claims nothing until the agent takes its first call on the current load of the model, because a claim requires batch membership. A load may therefore swap out the model between the start of a turn and its first call, or after a reload and before the returning agent's first call. Nothing is interrupted (no call is in flight); the agent's first call reloads the model, restoring its saved slot. This is what keeps newcomers from pinning a model that an earlier waiting load needs.
- Starvation bound for a waiting load: at most the remaining turns of the agents already in the blocking model's batch. Each turn is bounded by `agents.max_steps` model calls, and each call is bounded by its reservation. Newcomers cannot join while the load waits, and the load fails after `max_wait_s`, when routing falls back to the next row.
- `availability_for(model, session)` reports from S's point of view; plain `availability(model)` treats every claim as Busy.
- Decision on the arbiter's swap of an orchestrator awaiting its subagents: it stays, as a sanctioned exception, for a load from any session. An agent in `await_tasks` holds no call lease, is asleep, and has said through `LocalModels::waiting` that it is not using the model; it is not interrupted (no call is in flight) and gets its slot back from disk. Restricting the swap to the waiter's own session was considered and rejected: two orchestrators each awaiting a subagent that needs the memory the other one's model holds would wait for each other forever (hold and wait). Under the chosen rule a blocked load never holds memory (its model is not loaded) and an agent that holds a claim either progresses or enters `await_tasks`, which releases it, so no wait cycle exists; `max_wait_s` still bounds every wait. Same-session claims stay evictable after `min_residency_s` because a session coordinates its own agents; removing that would make a subagent wait up to `max_wait_s` behind an orchestrator that keeps working.
- Fairness: an agent joins a model's batch (and so claims it) only by taking a call on it, and newcomers do not join while an earlier call for another model waits (existing `behind_another_model`). A model in the way therefore drains: its current users finish their turns or start waiting, and the first waiting load proceeds. Background calls keep the #56 rules; background sessions drop their claim at step boundaries while the machine is not quiet (`yield_to_user`), so the user's loads wait at most one background step.
- Lock order (unchanged, now documented and kept): `Supervisor::state` → `Presence::users` → `SlotPool::table`; no std mutex is held across an `.await`; a call takes `swapping` only while it holds no permit; permits are always taken tokens first, then slot. `plan()` keeps the `Presence::users` guard from reading claims until the victims are out of `state.loaded`, so a claim either counts or arrives after the model has left the loaded set (its holder then reloads it, with its saved slot, on its next call; nothing is interrupted).
- Rejected: an RwLock per model held for a whole turn (cannot express the batch-based fairness, and a waiting orchestrator would hold it); evicting claims of other sessions after a timeout (that is exactly the yielding item 5 forbids).

### D-D: where per-model settings live

- New config keys `[runtime.llamacpp.models."<local model id>"]` (a table keyed by id, so it merges key by key across layers) and `[models.overrides."<catalog id>"]` (overrides for model-table rows, which are an array and would otherwise be replaced whole).
- The desktop writes them to a new layer, `<config dir>/models.toml` (`Source::App`), loaded after the user's `config.toml` and before environment variables, so what is saved in the app wins over `config.toml` and the user's hand-written `config.toml` is never rewritten. Hand-written values under the same keys in `config.toml` work too and show as effective when the app has saved nothing.
- Conflicts: the app layer wins over `config.toml`, key by key.
- Strict on save, lenient at start. `models.set_config` refuses anything invalid and writes nothing. When the daemon (or the CLI) loads, each app-layer entry (`runtime.llamacpp.models."<id>"`, `models.overrides."<id>"`) that fails to deserialise or validate is dropped with a warning (`Config::warnings()`, logged by the daemon), and an app file that is not valid TOML is ignored with a warning. A bad `models.toml` never stops the daemon. Unknown override ids are ignored, so removing a row from `config.toml` is safe too.
- Saving validates the merged configuration before the file is replaced (temp file, then rename), then applies the model-related part live: new sessions route with the new table, and loaded models whose launch shape changed restart once Free (swap reason `config`; see [Stale models](#stale-models)).
- `models.config` computes `effective` and `defaults` from the live state: the supervisor's per-model settings snapshot and its global llama.cpp settings, and the daemon's settings snapshot for table rows. It does not re-read files, so it reports what will actually be used. `saved` is read from `models.toml`.
- Remote models (decision (b)): the dialog edits only what existing rows have (`tier`, `tags`, `context_window`, endpoint prices) plus one minimal addition, `enabled` (also accepted on `[[models.catalog]]` rows). It does not add or remove rows or endpoints.
- Rejected: rewriting `config.toml` (would need `toml_edit` and could clobber hand edits and comments); a JSON store outside the config layers (invisible to `imperium config explain` and the CLI).

### Stale models

A loaded model is stale when its launch shape changes: any field of the effective settings that reaches `LaunchSpec` or the token pool.

- `Supervisor::set_model_settings` stores the new map, marks the stale entries in `state.loaded` (`Loaded::stale = true`), raises `changed`, and spawns (`tokio::spawn`, holding a `Weak<Supervisor>`) a stale-unload task if one is not already running. It never awaits. `models.set_config` therefore stays a synchronous `call::<ModelSetConfig>` handler (validate, write the file, update the snapshot, call `set_model_settings`) and never blocks on a load in flight.
- The stale-unload task loops while any entry is stale:
  1. Enable a `changed.notified()` listener before looking.
  2. Await `swapping.lock()` (an await, not `try_lock`, so it takes its turn behind a load in flight).
  3. Under the state and presence guards, take out of `state.loaded` every stale entry that is Free: no call lease, no queued call, no claim by any agent of any session. Then unload them with `swap_out(victims, "config")`, which saves the slots of agents that can come back.
  4. Drop `swapping`, then wait for the listener or `RECHECK`.
- The periodic idle tick (`unload_idle`) runs the same stale check, so a missed notification is caught within `IDLE_CHECK`.
- While an entry is stale, `step()` lets batch members carry on with it (they are using it). A newcomer (a holder not in the batch) gets `Step::Wait`, never `Step::Load`, so it does not start on the old shape and is not counted as a new user. Once the entry is gone, the newcomer's next `step()` loads the new shape.
- `plan()` returns `Plan::Wait { yielded: false }` (never `Plan::Retry`) when its own model's entry is present but stale, so a call cannot spin between `step()` and `plan()`. It wakes on `changed` when the entry leaves.
- Test hook: `pub async fn unload_stale_now(&self)` (documented "for tests and the daemon's tick") runs steps 2 and 3 once, deterministically. The G4 restart tests use it instead of sleeping.

## Contract (pinned; Stage 1 lands it, Stages 2 and 3 code against it)

### Configuration schema

```toml
[runtime.llamacpp]
context = 8192            # tokens one session may use on a local model (was: per slot)
parallel = 2              # deprecated: only sizes the default pool, pool_tokens = context × parallel
# slots = 8               # new, optional: default --parallel of each model; unset = max(8, parallel); CPU lane: 1
# pool_tokens = 65536     # new, optional: default pool for every model
cache_type = "f16"        # default for both K and V
flash_attn = "auto"       # new: auto | on | off
max_output_tokens = 8192  # new: most tokens one local reply may generate before an uncapped retry; 0 = uncapped

# Per local model (written by the app into models.toml; also allowed in config.toml).
[runtime.llamacpp.models."Qwen3-8B-Q4_K_M"]
context = 32768
pool_tokens = 65536       # at least context
slots = 4                 # 1..=256
cache_type_k = "q8_0"     # f32 f16 bf16 q8_0 q4_0 q4_1 iq4_nl q5_0 q5_1
cache_type_v = "q8_0"     # a quantised V type needs flash_attn on or auto
flash_attn = "on"
max_gpu_layers = 20       # cap on the planner's GPU layers
max_wait_s = 900
max_output_tokens = 16384 # 0 = uncapped (reserve the whole context)
extra_args = ["--n-cpu-moe", "8"]   # after runtime.llamacpp.extra_args

# Per model-table row (written by the app into models.toml).
[models.overrides.coder]
enabled = false
tier = "strong"
tags = ["code"]
context_window = 131072
input_usd_per_mtok = 0.3  # both prices or neither
output_usd_per_mtok = 1.2
```

Resolution of a local model's effective settings (pure function in `imperium-config`, used by validation, the supervisor and the RPC), field by field, first set wins:

1. `runtime.llamacpp.models."<id>"` (merged layers);
2. for a CPU-lane model, its `[[models.catalog]]` row's legacy `parallel` (→ `slots`), `context`, `max_wait_s`; if a row and the per-model table both set a field to different values, validation fails naming both keys;
3. CPU-lane defaults: `slots = 1`, `max_wait_s = max(3600, runtime.llamacpp.max_wait_s)` (move `CPU_LANE_SLOTS` and `CPU_LANE_MAX_WAIT_S` into `imperium-config`, re-export from `imperium_local::supervisor`);
4. globals: `context`; `slots` = `runtime.llamacpp.slots`, else `max(8, parallel)`; `cache_type` (for K and V); `flash_attn`; `max_wait_s`; `max_output_tokens`; `max_gpu_layers = none`. `extra_args` is the global arguments, then the model's;
5. `pool_tokens` = per-model, else `runtime.llamacpp.pool_tokens`, else `context × (CPU lane ? slots : parallel)`, and never below `context` when derived.

Validation (in `Settings::validate` and in `models.set_config`; messages name the key; at load, invalid app-layer entries are dropped with a warning instead, see D-D): `context`, `pool_tokens`, `slots`, `max_wait_s` ≥ 1, `max_output_tokens` ≥ 0; `slots` ≤ 256; an explicit `pool_tokens` ≥ the effective `context`; cache types in the list above; `flash_attn` in `auto | on | off`; a V type other than f16, bf16 or f32 with `flash_attn = "off"` is refused; per-model `extra_args` may not contain a flag Imperium sets (`-m --model --host --port -c --ctx-size -np --parallel -kvu --kv-unified -no-kvu --no-kv-unified --kv-unified-per-slot --cache-idle-slots --no-cache-idle-slots -ngl --gpu-layers --n-gpu-layers -ctk --cache-type-k -ctv --cache-type-v -fa --flash-attn --cache-ram -cram --slot-save-path --device`). Match an argument by the part before any `=`, so `--ctx-size=4096` is refused like `--ctx-size 4096`; override prices both set or neither. Global `extra_args` stay unchecked (compatibility). For a vLLM model, `cache_type_k/v`, `flash_attn` and `max_gpu_layers` are refused by `models.set_config` and ignored with a warning when hand-written.

### Launch arguments (G2)

`llama-server`, in this order: `--model <path> --host 127.0.0.1 --ctx-size <pool_tokens> --kv-unified`, then `--kv-unified-per-slot <context>` only when `context < pool_tokens`, then `--parallel <slots> --no-cache-idle-slots --n-gpu-layers <n> --cache-type-k <k> --cache-type-v <v> --flash-attn <auto|on|off> --cache-ram <mib> --jinja`, then `--device none` (CPU lane), `--slot-save-path <dir>`, `--mmproj <path>`, the global extra arguments, the model's extra arguments. vLLM: `--max-model-len <context> --max-num-seqs <slots>` (no longer `context / slots`), then global and model extra arguments. `LaunchSpec` replaces `context` and `cache_type` with `context`, `pool_tokens`, `cache_type_k`, `cache_type_v`, `flash_attn`, and appends the model's extra arguments to `extra_args`. The slot-file key (`slot_files::model_key`) covers context, pool, K and V types. A custom `llama-server` (`runtime.llamacpp.custom_path`) must accept `--kv-unified`, `--no-cache-idle-slots` and, when the context is below the pool, `--kv-unified-per-slot` (llama.cpp b12180 or later). This is documented in docs/04 and not probed: probing `--help` adds a process start per launch path for a rare case. An older binary exits at once, its log names the unknown argument, and the existing quick-failure error surfaces that.

### Rust API inside the workspace

- `imperium-api` (`crates/imperium-api/src/local.rs`, re-exported in `lib.rs`):
  - `pub struct LocalRequest { pub priority: Priority, pub prompt_tokens: Option<u32>, pub uncapped: bool }` (`Default`: interactive, `None`, false). `prompt_tokens` is the agent's estimate `p` (D-A); `None` or `uncapped` reserves the whole `context`.
  - `LocalLease` gains `pub max_tokens: Option<u32>`: what the agent must send as `max_tokens` (`r − p`), or `None` (uncapped, or a runtime without a token pool such as vLLM). `LocalLease::new` takes it.
  - `LocalModels::acquire_with(&self, model, holder, request: LocalRequest) -> AcquireFuture` (default: `acquire_as(model, holder, request.priority)`).
  - `LocalModels::availability_for(&self, model, session: &str) -> Availability` (default: `availability(model)`).
  - `pub fn holder(session: &str, agent: &str) -> String` and `pub fn holder_session(holder: &str) -> &str` (text before the first `/`); `Agent::holder` uses `holder`.
  - Docs: `attach` "while held, after the agent's first call on the loaded model, the model is never unloaded for another session's load, an idle timeout or a settings change"; `waiting` "releases that hold: the model may be swapped out for any load, its slots saved and restored".
- `imperium-models`: `ChatRequest` gains `max_tokens: Option<u32>` (sent as `max_tokens` when set); `Completion` gains `finish_reason: Option<String>` from the stream's last choice.
- `imperium-local` `Supervisor`: `set_model_settings(BTreeMap<String, LocalModelSettings>)` (marks stale and spawns the stale-unload task, see [Stale models](#stale-models)), `pub async fn unload_stale_now(&self)`, `launch(&self, id) -> ModelLaunch` (effective settings), `LoadedStatus` gains `context`, `pool_tokens`, `pool_reserved_tokens`, `stale`; `Activity::interactive_queued` also counts the user's calls waiting for tokens or for room to load. Internal names beyond these may differ; behaviour and the named tests bind.

### RPC methods

| Method | Rust marker | Params | Result |
| ------ | ----------- | ------ | ------ |
| `models.config` | `ModelConfig` | `ModelConfigParams` | `ModelConfigResult` |
| `models.set_config` | `ModelSetConfig` | `ModelSetConfigParams` | `ModelConfigResult` |

- `models.set_config` semantics: each `LocalModelUpdate` / `TableModelUpdate` replaces that model's saved settings whole (null fields are removed; all-null removes the model's section); models not named keep theirs. Unknown ids, vLLM-inapplicable fields and invalid values fail with JSON-RPC `INVALID_PARAMS` and a message `local model <id>: <reason>` or `model table row <id>: <reason>`; nothing is written. `dry_run: true` validates and returns the would-be result without writing or applying (the dialog uses it for live estimates). Writes are serialised by a daemon `std::sync::Mutex<()>`. Both handlers are synchronous `call::<…>` handlers: `models.set_config` validates, writes, swaps the settings snapshot and calls `set_model_settings`, which only marks and spawns, so it never waits for a load or an unload. The result is the state after saving, computed from the live snapshot (D-D).
- `PROTOCOL_VERSION` becomes `{ major: 0, minor: 3 }`; the desktop handshake sends `0.3`.
- Existing types change: `ModelRow` gains `enabled`; `LocalModelRow` gains `context`, `pool_tokens`, `pool_reserved_tokens` and its `required_bytes` doc changes; `ModelSwap.reason` doc lists `config`.

Rust definitions to add to `crates/imperium-protocol/src/methods.rs` after `ModelDownloadCancelResult` (doc comments are part of the contract: the TypeScript below is generated from them):

```rust
/// `models.config`: every local model's llama.cpp settings and every model-table row's
/// settings, as saved from the app and as they take effect.
#[derive(Debug)]
pub struct ModelConfig;

impl Method for ModelConfig {
    const NAME: &'static str = "models.config";
    type Params = ModelConfigParams;
    type Result = ModelConfigResult;
}

/// `models.set_config`: saves model settings to the app's settings file and applies them.
/// Each update replaces that model's saved settings whole; models not named keep theirs.
#[derive(Debug)]
pub struct ModelSetConfig;

impl Method for ModelSetConfig {
    const NAME: &'static str = "models.set_config";
    type Params = ModelSetConfigParams;
    type Result = ModelConfigResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelConfigParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelConfigResult {
    /// Models found in `models.folders`, in id order.
    pub local: Vec<LocalModelConfig>,
    /// Rows of the model table, in table order.
    pub table: Vec<TableModelConfig>,
    /// What a local model gets for each setting it does not set.
    pub defaults: LocalLaunchEffective,
    /// KV cache types llama-server accepts for `cache_type_k` and `cache_type_v`.
    pub cache_types: Vec<String>,
    /// The file saved settings are written to.
    pub file: String,
}

/// A local model's llama.cpp settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelConfig {
    /// The id in `models.local`.
    pub id: String,
    #[ts(type = "\"llamacpp\" | \"vllm\"")]
    pub runtime: String,
    /// What the app's settings file sets; null fields take the effective value from elsewhere.
    pub saved: LocalLaunchSettings,
    /// What the model is started with.
    pub effective: LocalLaunchEffective,
    /// Memory the model needs with the effective settings.
    #[ts(type = "number")]
    pub required_bytes: u64,
    /// The part of `required_bytes` that is the KV cache pool.
    #[ts(type = "number")]
    pub kv_pool_bytes: u64,
    /// How it would load on this machine when nothing else is loaded.
    #[ts(type = "\"gpu\" | \"partial\" | \"cpu\" | \"none\"")]
    pub placement: String,
    pub loaded: bool,
    /// Loaded with other settings: the new ones apply once no session is using it.
    pub reload_pending: bool,
    /// Pinned to the CPU lane by `placement = "cpu"` in the model table.
    pub cpu_lane: bool,
    /// The context the model was trained for, from its header.
    #[ts(type = "number | null")]
    pub context_length: Option<u64>,
}

/// Per-model llama.cpp settings; a null field takes the default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalLaunchSettings {
    /// Tokens one session may use (`--kv-unified-per-slot`; vLLM `--max-model-len`).
    #[serde(default)]
    pub context: Option<u32>,
    /// Tokens of KV cache shared by every slot (`--ctx-size`); at least `context`.
    #[serde(default)]
    pub pool_tokens: Option<u32>,
    /// Calls the server runs at once (`--parallel`; vLLM `--max-num-seqs`).
    #[serde(default)]
    pub slots: Option<u32>,
    /// `--cache-type-k`, one of `cache_types`.
    #[serde(default)]
    pub cache_type_k: Option<String>,
    /// `--cache-type-v`, one of `cache_types`; a quantised type needs flash attention.
    #[serde(default)]
    pub cache_type_v: Option<String>,
    /// `--flash-attn`.
    #[serde(default)]
    #[ts(type = "\"auto\" | \"on\" | \"off\" | null")]
    pub flash_attn: Option<String>,
    /// Most layers placed on the GPU; the planner may place fewer.
    #[serde(default)]
    pub max_gpu_layers: Option<u32>,
    /// Longest a call waits for a slot or for room to load the model.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_wait_s: Option<u64>,
    /// Most tokens one reply may generate before it is retried without a cap; 0 means no cap.
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    /// Arguments added after `runtime.llamacpp.extra_args` (or `runtime.vllm.extra_args`).
    #[serde(default)]
    pub extra_args: Option<Vec<String>>,
}

/// The llama.cpp settings a local model is started with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalLaunchEffective {
    pub context: u32,
    pub pool_tokens: u32,
    pub slots: u32,
    pub cache_type_k: String,
    pub cache_type_v: String,
    #[ts(type = "\"auto\" | \"on\" | \"off\"")]
    pub flash_attn: String,
    /// Absent: as many as the planner fits.
    pub max_gpu_layers: Option<u32>,
    #[ts(type = "number")]
    pub max_wait_s: u64,
    pub max_output_tokens: u32,
    /// Every extra argument, the global ones first.
    pub extra_args: Vec<String>,
}

/// A model-table row's settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelConfig {
    pub id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    #[ts(type = "\"local\" | \"cloud\"")]
    pub location: String,
    /// False when the row names an endpoint that is not configured.
    pub endpoint_configured: bool,
    /// What the app's settings file sets; null fields keep the row's value from the config.
    pub saved: TableModelSettings,
    pub effective: TableModelEffective,
}

/// Settings the app may change on a model-table row; a null field keeps the row's own value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelSettings {
    /// False leaves the row out of routing.
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    #[ts(type = "\"basic\" | \"standard\" | \"strong\" | \"frontier\" | null")]
    pub tier: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Tokens the model reads per request.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Set both prices or neither.
    #[serde(default)]
    pub input_usd_per_mtok: Option<f64>,
    #[serde(default)]
    pub output_usd_per_mtok: Option<f64>,
}

/// A model-table row as routing uses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelEffective {
    pub enabled: bool,
    #[ts(type = "\"basic\" | \"standard\" | \"strong\" | \"frontier\"")]
    pub tier: String,
    pub tags: Vec<String>,
    pub context_window: Option<u32>,
    pub input_usd_per_mtok: Option<f64>,
    pub output_usd_per_mtok: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModelSetConfigParams {
    #[serde(default)]
    pub local: Vec<LocalModelUpdate>,
    #[serde(default)]
    pub table: Vec<TableModelUpdate>,
    /// Check and compute the result without saving or applying anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// New saved settings for one local model; all-null settings remove its saved settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LocalModelUpdate {
    pub id: String,
    pub settings: LocalLaunchSettings,
}

/// New saved settings for one model-table row; all-null settings remove its saved settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TableModelUpdate {
    pub id: String,
    pub settings: TableModelSettings,
}
```

Changes to existing structs in the same file:

```rust
// ModelRow: append after `context_window`.
    /// False when the row is switched off and left out of routing.
    pub enabled: bool,

// LocalModelRow: replace the doc of `required_bytes` with
    /// Memory needed with the model's KV pool and settings.
// and append after `tokens_per_s`:
    /// Tokens one session may use.
    #[serde(default)]
    pub context: u32,
    /// Tokens of KV cache shared by the model's slots.
    #[serde(default)]
    pub pool_tokens: u32,
    /// Of those, tokens reserved by calls running now.
    #[serde(default)]
    pub pool_reserved_tokens: u32,

// ModelSwap: replace the doc of `reason` with
    /// Out: `room` (another model needed the memory), `idle`, or `config` (its settings
    /// changed). In: `swap` (it replaced a model) or `restore` (it came back with saved slots).
```

### Generated TypeScript (hand-written in Stage 1, must equal ts-rs 12 output)

Rules observed in `packages/protocol/src/generated/` (match them exactly; CI regenerates and diffs):

- Line 1 is `// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.`; then `import type { X } from "./X";` lines sorted by name (none when no other generated type is used); then one empty line; then the type's doc block, if any; then `export type`.
- A struct is `export type Name = {`, one space, the fields, then `};`. Each field is `name: T,` followed by one space. A field with docs is preceded by a line break and its `/** … */` block, then a line break. Each doc line is a space, `*`, a space, then the Rust doc line. `u32`/`f64` map to `number`, `Option<T>` to `T | null`, `Vec<T>` to `Array<T>`, `bool` to `boolean`; `u64` only through the `#[ts(type)]` shown. An empty struct is `Record<symbol, never>`.
- Files end with `};` (or `>;`) and one `\n`; LF line endings.
- In the blocks below every line that ends in `{` or `,` has exactly one trailing space in the real file (this document omits it to keep Markdown lint clean).

| Rust type | File | Change |
| --------- | ---- | ------ |
| `ModelConfigParams` | `ModelConfigParams.ts` | new |
| `ModelConfigResult` | `ModelConfigResult.ts` | new |
| `LocalModelConfig` | `LocalModelConfig.ts` | new |
| `LocalLaunchSettings` | `LocalLaunchSettings.ts` | new |
| `LocalLaunchEffective` | `LocalLaunchEffective.ts` | new |
| `TableModelConfig` | `TableModelConfig.ts` | new |
| `TableModelSettings` | `TableModelSettings.ts` | new |
| `TableModelEffective` | `TableModelEffective.ts` | new |
| `ModelSetConfigParams` | `ModelSetConfigParams.ts` | new |
| `LocalModelUpdate` | `LocalModelUpdate.ts` | new |
| `TableModelUpdate` | `TableModelUpdate.ts` | new |
| `ModelRow` | `ModelRow.ts` | append `enabled` |
| `LocalModelRow` | `LocalModelRow.ts` | doc of `required_bytes`; append three fields |
| `ModelSwap` | `ModelSwap.ts` | doc of `reason` |

`ModelConfigParams.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.

export type ModelConfigParams = Record<symbol, never>;
```

`ModelConfigResult.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { LocalLaunchEffective } from "./LocalLaunchEffective";
import type { LocalModelConfig } from "./LocalModelConfig";
import type { TableModelConfig } from "./TableModelConfig";

export type ModelConfigResult = {
/**
 * Models found in `models.folders`, in id order.
 */
local: Array<LocalModelConfig>,
/**
 * Rows of the model table, in table order.
 */
table: Array<TableModelConfig>,
/**
 * What a local model gets for each setting it does not set.
 */
defaults: LocalLaunchEffective,
/**
 * KV cache types llama-server accepts for `cache_type_k` and `cache_type_v`.
 */
cache_types: Array<string>,
/**
 * The file saved settings are written to.
 */
file: string, };
```

`LocalModelConfig.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { LocalLaunchEffective } from "./LocalLaunchEffective";
import type { LocalLaunchSettings } from "./LocalLaunchSettings";

/**
 * A local model's llama.cpp settings.
 */
export type LocalModelConfig = {
/**
 * The id in `models.local`.
 */
id: string, runtime: "llamacpp" | "vllm",
/**
 * What the app's settings file sets; null fields take the effective value from elsewhere.
 */
saved: LocalLaunchSettings,
/**
 * What the model is started with.
 */
effective: LocalLaunchEffective,
/**
 * Memory the model needs with the effective settings.
 */
required_bytes: number,
/**
 * The part of `required_bytes` that is the KV cache pool.
 */
kv_pool_bytes: number,
/**
 * How it would load on this machine when nothing else is loaded.
 */
placement: "gpu" | "partial" | "cpu" | "none", loaded: boolean,
/**
 * Loaded with other settings: the new ones apply once no session is using it.
 */
reload_pending: boolean,
/**
 * Pinned to the CPU lane by `placement = "cpu"` in the model table.
 */
cpu_lane: boolean,
/**
 * The context the model was trained for, from its header.
 */
context_length: number | null, };
```

`LocalLaunchSettings.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.

/**
 * Per-model llama.cpp settings; a null field takes the default.
 */
export type LocalLaunchSettings = {
/**
 * Tokens one session may use (`--kv-unified-per-slot`; vLLM `--max-model-len`).
 */
context: number | null,
/**
 * Tokens of KV cache shared by every slot (`--ctx-size`); at least `context`.
 */
pool_tokens: number | null,
/**
 * Calls the server runs at once (`--parallel`; vLLM `--max-num-seqs`).
 */
slots: number | null,
/**
 * `--cache-type-k`, one of `cache_types`.
 */
cache_type_k: string | null,
/**
 * `--cache-type-v`, one of `cache_types`; a quantised type needs flash attention.
 */
cache_type_v: string | null,
/**
 * `--flash-attn`.
 */
flash_attn: "auto" | "on" | "off" | null,
/**
 * Most layers placed on the GPU; the planner may place fewer.
 */
max_gpu_layers: number | null,
/**
 * Longest a call waits for a slot or for room to load the model.
 */
max_wait_s: number | null,
/**
 * Most tokens one reply may generate before it is retried without a cap; 0 means no cap.
 */
max_output_tokens: number | null,
/**
 * Arguments added after `runtime.llamacpp.extra_args` (or `runtime.vllm.extra_args`).
 */
extra_args: Array<string> | null, };
```

`LocalLaunchEffective.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.

/**
 * The llama.cpp settings a local model is started with.
 */
export type LocalLaunchEffective = { context: number, pool_tokens: number, slots: number, cache_type_k: string, cache_type_v: string, flash_attn: "auto" | "on" | "off",
/**
 * Absent: as many as the planner fits.
 */
max_gpu_layers: number | null, max_wait_s: number, max_output_tokens: number,
/**
 * Every extra argument, the global ones first.
 */
extra_args: Array<string>, };
```

`TableModelConfig.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { TableModelEffective } from "./TableModelEffective";
import type { TableModelSettings } from "./TableModelSettings";

/**
 * A model-table row's settings.
 */
export type TableModelConfig = { id: string,
/**
 * `<endpoint>:<model>`.
 */
spec: string, location: "local" | "cloud",
/**
 * False when the row names an endpoint that is not configured.
 */
endpoint_configured: boolean,
/**
 * What the app's settings file sets; null fields keep the row's value from the config.
 */
saved: TableModelSettings, effective: TableModelEffective, };
```

`TableModelSettings.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.

/**
 * Settings the app may change on a model-table row; a null field keeps the row's own value.
 */
export type TableModelSettings = {
/**
 * False leaves the row out of routing.
 */
enabled: boolean | null, tier: "basic" | "standard" | "strong" | "frontier" | null, tags: Array<string> | null,
/**
 * Tokens the model reads per request.
 */
context_window: number | null,
/**
 * Set both prices or neither.
 */
input_usd_per_mtok: number | null, output_usd_per_mtok: number | null, };
```

`TableModelEffective.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.

/**
 * A model-table row as routing uses it.
 */
export type TableModelEffective = { enabled: boolean, tier: "basic" | "standard" | "strong" | "frontier", tags: Array<string>, context_window: number | null, input_usd_per_mtok: number | null, output_usd_per_mtok: number | null, };
```

`ModelSetConfigParams.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { LocalModelUpdate } from "./LocalModelUpdate";
import type { TableModelUpdate } from "./TableModelUpdate";

export type ModelSetConfigParams = { local: Array<LocalModelUpdate>, table: Array<TableModelUpdate>,
/**
 * Check and compute the result without saving or applying anything.
 */
dry_run: boolean, };
```

`LocalModelUpdate.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { LocalLaunchSettings } from "./LocalLaunchSettings";

/**
 * New saved settings for one local model; all-null settings remove its saved settings.
 */
export type LocalModelUpdate = { id: string, settings: LocalLaunchSettings, };
```

`TableModelUpdate.ts`:

```ts
// This file was generated by [ts-rs](https://github.com/Aleph-Alpha/ts-rs). Do not edit this file manually.
import type { TableModelSettings } from "./TableModelSettings";

/**
 * New saved settings for one model-table row; all-null settings remove its saved settings.
 */
export type TableModelUpdate = { id: string, settings: TableModelSettings, };
```

`ModelRow.ts`: the last line `context_window: number | null, };` becomes:

```ts
context_window: number | null,
/**
 * False when the row is switched off and left out of routing.
 */
enabled: boolean, };
```

`LocalModelRow.ts`: the doc line above `required_bytes` becomes `* Memory needed with the model's KV pool and settings.` (with the usual leading space), and the last line `tokens_per_s: number | null, };` becomes:

```ts
tokens_per_s: number | null,
/**
 * Tokens one session may use.
 */
context: number,
/**
 * Tokens of KV cache shared by the model's slots.
 */
pool_tokens: number,
/**
 * Of those, tokens reserved by calls running now.
 */
pool_reserved_tokens: number, };
```

`ModelSwap.ts`: the `reason` doc block becomes:

```ts
/**
 * Out: `room` (another model needed the memory), `idle`, or `config` (its settings
 * changed). In: `swap` (it replaced a model) or `restore` (it came back with saved slots).
 */
reason: string, };
```

## Validation rules

- Rust: implementers may run `cargo fmt --all` and `cargo check --workspace --all-targets` locally, nothing else (no clippy, no `cargo test`, no `scripts/check-rust.sh`). Code must be fmt-clean and clippy-clean under `-D warnings` (workspace `clippy.toml`) on the first push. G1–G7 are verified by the PR's CI job `rust`; check it with `gh pr checks <pr>` (the `rust` and `gate` rows pass) on the stage's head commit.
- Generated TypeScript is hand-written to the rules above; CI's `rust` job fails if `cargo test` regenerates anything different.
- Web, local: `pnpm install` once at the repo root, then `bash scripts/check-web.sh` (`pnpm -r --if-present run` lint, typecheck, test, build). `apps/desktop` has only `typecheck` and `build` scripts and `packages/protocol` only `typecheck`, so for this change the script runs typecheck and build; there is no desktop lint or unit-test gate. Per package: `pnpm --filter @imperium/protocol typecheck` and `pnpm --filter @imperium/desktop build` (`tsc --noEmit` then `vite build`).
- The one exception to CI-only Rust (decision (c)): root, and only root, builds and runs the daemon locally for the G8 in-app check, by the repo's dev path (`cargo run --bin imperiumd`, README), with the desktop app from `pnpm --filter @imperium/desktop tauri dev`. Clippy and tests still run only in CI; implementers never run the daemon.
- Docs: PR CI job `docs` (markdownlint-cli2, lychee offline link check).

## Stage 1: protocol contract

- Objective: land the pinned protocol types and their TypeScript so Stages 2 and 3 can start in parallel.
- Depends on: none (the branch is already on `74f9e21`).
- Owned paths: `crates/imperium-protocol/src/methods.rs`, `crates/imperium-protocol/src/lib.rs` (`PROTOCOL_VERSION` minor 3), `packages/protocol/src/generated/` (the 11 new files and 3 changed files in the table above), `scripts/check-rust.sh`, `docs/02-architecture.md` (the handshake's "currently 0.2" becomes 0.3), and the minimum compile fixes the new fields force: `crates/imperiumd/src/state.rs` (`ModelRow { enabled: true, .. }`) and `crates/imperiumd/src/local.rs` (`LocalModelRow` new fields filled from today's values: `context = settings.context`, `pool_tokens = context × parallel`, `pool_reserved_tokens = 0`).
- `scripts/check-rust.sh` change: when the generated files drift, print `git --no-pager diff -- "$generated"` and, for each untracked generated file, its name and contents (`cat`), before the existing message and `exit 1`, so the CI log carries the exact expected text.
- Acceptance checks:
  - The 11 new and 3 changed generated files exactly match the blocks above plus the trailing-space rule; `git diff --stat <base>..HEAD -- packages/protocol/src/generated` lists exactly those 14 files.
  - `cargo fmt --all` leaves no diff and `cargo check --workspace --all-targets` exits 0 (local).
  - `pnpm --filter @imperium/protocol typecheck` exits 0 (local).
  - PR CI job `rust` passes (the generated-files check is the decisive part).
  - `docs/02-architecture.md` says 0.3; PR CI job `docs` passes.
- Status: pending
- Outcome:
- Handoff notes: no behaviour changes in this stage; nothing calls the new methods yet. If CI shows a binding mismatch, take the expected text from the diff and the untracked-file contents that the revised `check-rust.sh` prints in the `rust` job log (`gh run view <run> --log-failed`), apply it verbatim, and push again; do not guess. Any change to a pinned name or field needs root's approval, because Stage 3 codes against it.

## Stage 2: backend

- Objective: unified KV pool, per-model settings, usage locks, unlimited concurrency, the two RPC methods, tests and docs.
- Depends on: Stage 1.
- Owned paths: `crates/imperium-config/**`, `crates/imperium-local/**`, `crates/imperium-api/src/{local,lib}.rs`, `crates/imperium-models/src/chat.rs`, `crates/imperium-core/src/{agent,model,breakdown}.rs`, `crates/imperium-core/tests/**`, `crates/imperiumd/src/{rpc,state,local,load}.rs` plus a new `crates/imperiumd/src/model_config.rs` and its `mod` line, `crates/imperiumd/tests/**`, `crates/imperium-cli/src/**` only for compile fixes, `docs/04-models-and-routing.md`, `docs/10-configuration.md`, `docs/13-decisions.md`. Not `packages/**` or `apps/**` (a contract fix goes back through root and Stage 1's files).
- Work, in dependency order:
  1. `imperium-config`:
     - new keys and `LocalModelSettings` / `TableOverride` structs; `enabled` on `CatalogEntry` (`#[serde(default = …)]` true);
     - the resolver and `ModelLaunch` (see [Configuration schema](#configuration-schema)), and validation;
     - overrides applied to the catalog and endpoint prices inside `Config::settings()`, before `validate`;
     - `Source::App(PathBuf)` (displayed as `app settings <path>`), and `Paths::app_models_file()` = `<config dir>/models.toml`, loaded by `load()` after the user file;
     - lenient loading of the App layer (D-D): entries that fail are dropped and reported through `Config::warnings() -> &[String]`; unparsable TOML is ignored with a warning;
     - `pub fn load_layers(...) -> Result<Vec<Layer>, ConfigError>` and `Layer::source()`, so the daemon can strictly validate a candidate file. Update every `LlamaCppSettings { .. }` literal in tests (swap.rs, routing.rs, cpu_lane.rs, background_priority.rs, supervisor.rs, process.rs).
  2. `imperium-local` planner and backend: `Footprint::of(model, &ModelLaunch)` with `kv_bytes = kv_bytes_per_token_kv(k_bytes, v_bytes) × pool_tokens` (split `GgufInfo::kv_bytes_per_token` into key and value element sizes; `bf16` = 2 bytes); a GPU-layer cap honoured by the planner with the memory split recomputed for the capped count; `LaunchSpec` and argument order as pinned; `slot_files::model_key` over the new shape.
  3. `imperium-local` supervisor:
     - per-model launch from `set_model_settings` and the CPU-lane pins;
     - the token pool per loaded llama.cpp model: `SlotPool` gains `context`, `pool_tokens`, `max_output_tokens` and a FIFO token semaphore. A lease holds a token permit and a slot permit, taken in that order under the existing `step` / `Queued` flow and both released when the lease drops. The lease carries `max_tokens` (D-A). vLLM pools get no token semaphore;
     - `LocalModels::acquire_with` and `availability_for`;
     - claims mapped per requester session (see the traps below), which `residents` maps to `Use::Busy`;
     - idle and stale unload take Free models only; stale handling exactly as [Stale models](#stale-models), including `unload_stale_now`;
     - `plan()` keeps the presence guard until victims leave `state.loaded`;
     - `SwapEvent.reason` `config`.

     `arbiter.rs` logic stays; its module docs describe Claimed as "claimed by the requester's own session".
  4. `imperium-models`: `ChatRequest.max_tokens` (sent as `max_tokens` when set) and `Completion.finish_reason`; update `ChatRequest { .. }` literals.
  5. `imperium-core`:
     - `Agent::holder` via `imperium_api::holder`;
     - `route` asks `availability_for` with the session id (add `session: Option<&str>` to `TaskRequest`);
     - `endpoint()` calls `acquire_with` with the prompt estimate `p` of D-A. The agent keeps the last call's exact usage and the message count it covered. Tool specs count on the first call (serialised with `serde_json`) and are inside the exact count afterwards. Template overhead is 8 tokens per added message plus 64 for the frame, and estimates carry a 1.25 margin;
     - `ChatRequest.max_tokens` is set from `lease.max_tokens`;
     - the uncapped retry (`LocalRequest { uncapped: true, .. }`) runs once when a capped local reply ends with `finish_reason == "length"` or the call fails with "Context size has been exceeded". It emits the `Notice` of D-A first, and only the retry's completion is appended to `messages`;
     - `local.waiting(model, holder)` is held around `answer.await` in `Agent::approve` (after the allowed-tools shortcut) and around `controls.pause_until(until).await` in `run_turn`. This is the same mark as `await_tasks`, dropped when the wait ends.
  6. `imperiumd`:
     - `model_config.rs`: read `models.toml` as a `toml::Table`, apply updates, strictly validate the merged layers, write `models.toml.tmp` then rename, with a header comment saying the app writes it;
     - `DaemonState.settings` becomes `RwLock<Arc<Settings>>` behind `fn settings(&self) -> Arc<Settings>`. A save replaces only `models` and `runtime.llamacpp.models` in the snapshot, and a `Mutex<()>` serialises saves;
     - log `Config::warnings()` at start;
     - `LocalRuntime` passes per-model settings to the supervisor at start, at rescan and on save;
     - `DaemonState::catalog()` skips disabled rows, and `model_table()` fills `enabled`;
     - the listing fills the new `LocalModelRow` fields from `LoadedStatus` / `Supervisor::launch`;
     - `rpc.rs`: synchronous `call::<ModelConfig>` and `call::<ModelSetConfig>` handlers (errors as `INVALID_PARAMS`), computing `effective` and `defaults` from the live snapshot;
     - `load.rs` keeps using `Activity::interactive_queued`, which now includes waits for tokens and for room.
  7. Docs (see [Docs to update](#docs-to-update)).
- Tests to add or change (names bind; files are suggestions):
  - G2 `llama_server_shares_one_kv_pool` (`supervisor.rs` tests or a new `backend.rs` tests module): exact order and values of the pool, parallel, `--no-cache-idle-slots`, K, V and flash-attention arguments. `--kv-unified-per-slot <context>` is present when `context < pool_tokens` and absent when they are equal. vLLM `--max-model-len` equals `context`. Update `launch_arguments_carry_the_plan`.
  - G3 `the_kv_cache_is_one_pool_whatever_the_slots` (`planner.rs`): equal `kv_bytes` for slots 1 and 16 at the same pool; doubling the pool doubles `kv_bytes`; `q8_0` K with `f16` V lies between the two uniform types.
  - G4 `per_model_settings_resolve_over_the_defaults` (`imperium-config`): precedence 1–5 including the `max(8, parallel)` slot default; the CPU-row conflict error; an app layer over `config.toml` winning key by key; overrides on catalog rows; unknown override ids ignored. `invalid_app_entries_are_dropped_at_load` (`imperium-config`): an invalid `runtime.llamacpp.models."<id>"` entry and an unparsable app file load without error, the bad parts dropped and listed in `Config::warnings()`.
  - G4 `per_model_settings_reach_the_launch_arguments` (`crates/imperium-local/tests/kv_pool.rs`, new): `set_model_settings`, then a call launches with the model's K/V types, pool and slots.
  - G4 `a_stale_model_restarts_only_when_free` (`kv_pool.rs`):
    - changing the settings of a Free loaded model and calling `unload_stale_now` unloads it with reason `config`, and the next call launches the new shape;
    - with another session's claim held, `unload_stale_now` leaves it loaded and a newcomer's call waits (no launch, no `Retry` spin: the launch count stays 1); dropping the claim and calling `unload_stale_now` lets the newcomer load the new shape;
    - `set_model_settings` returns without awaiting while a load is in flight.
  - G4 `set_config_saves_reloads_and_applies` (`crates/imperiumd`): `models.set_config` writes `models.toml`; `imperium_config::load` sees it; `models.config` returns it as `saved` and `effective`; `dry_run` writes nothing; an invalid value is refused and leaves the file unchanged.
  - G5 `more_sessions_than_the_old_parallel_value_run_at_once` (`kv_pool.rs`): `parallel = 2` and default slots (8); six sessions with small prompt estimates hold leases at the same time (`slots_in_use == 6`, none waited).
  - G5 `calls_beyond_a_models_slots_queue_and_all_complete` (`kv_pool.rs`):
    - model `a` has `slots = 2` and model `b` fits beside it;
    - five sessions call `a` while three call `b`, all at once;
    - at most two leases are on `a` at any moment, the other three queue (`queued == 3` observed), and all eight calls complete without an error.
  - G5 `a_full_pool_makes_calls_wait_not_fail` (`kv_pool.rs`): reservations that exceed the pool make the next call wait until a lease drops, then it succeeds; an estimate above `context` is clamped, never an error; the lease's `max_tokens` equals `r − p`, `None` when `max_output_tokens = 0`, and `None` for a vLLM model.
  - G5 `local_calls_reserve_their_estimate_and_cap_the_reply` (`crates/imperium-core/tests/local_pool.rs`, new; fake `LocalModels` recording each `LocalRequest`, fake chat client recording each `ChatRequest`): the first call's `prompt_tokens` counts messages and tool specs with the margin and overhead; the second uses the exact usage plus the added messages; `ChatRequest.max_tokens` equals the lease's.
  - G5 `a_local_reply_cut_by_its_reservation_is_retried_once_uncapped` (`local_pool.rs`):
    - a reply ending with `finish_reason = "length"` is retried once with `uncapped: true` and no `max_tokens`;
    - so is a "Context size has been exceeded" failure;
    - a `Notice` precedes the retry, only the retry's text is in the assistant message, and a second failure is not retried.
  - G6 `a_load_never_evicts_a_model_another_session_is_using` (`swap.rs`, replacing `swap_reloads_a_model_evicted_between_calls`):
    - `first/root` attaches to `emperor` and makes a call;
    - `second/root` asks for `worker` with `min_residency_s = 0`;
    - after 200 ms `emperor` is still loaded and the log has no `stop emperor`;
    - dropping `first/root`'s attach lets `worker` load;
    - the same holds with a call lease held.
  - G6 `idle_unload_spares_a_model_another_session_is_mid_turn_on` (`swap.rs`): attached and past its first call, with no lease held, `unload_idle_now(Duration::ZERO)` keeps the model; after the attach drops, it unloads.
  - G6 `a_cpu_lane_model_another_session_is_using_is_never_evicted_for_ram` (`cpu_lane.rs`): a lane model claimed by another session is not evicted when a newcomer needs its RAM; the newcomer waits, then loads once the claim drops.
  - G6 `a_session_parked_on_an_approval_holds_no_claim` (`kv_pool.rs`, using the real agent loop with the fake SSE server as in `routing.rs`): a session whose tool call waits for a permission answer does not block another session's load of a model that does not fit beside its own (its model is swapped out with its slot saved), and `unload_idle_now(Duration::ZERO)` unloads its model. Answering the approval lets its turn continue, reloading the model.
  - G6 `claims_of_another_session_count_as_busy` (supervisor unit): `residents` maps another session's claim to Busy and the requester's own to Claimed; `availability_for` differs accordingly. The existing same-session tests (`swap_out_an_awaiting_orchestrator_for_its_subagent_and_restore_it`, `swap_parallel_sessions_batch_by_model`) keep passing unchanged.
  - G7 `concurrent_loads_take_turns_and_all_finish` (`swap.rs`): six concurrent callers over three models that fit one at a time, from different sessions, attaching and detaching; the fake launcher records the maximum of concurrent launches plus stops (must be 1), every call finishes within a 10 s timeout, and `stopped_in_use` stays empty.
  - Update rather than delete tests whose expectations change (for example `cpu_lane.rs` context and slot assertions, `local.rs` `coder.slots == parallel`).
- Acceptance checks:
  - G1–G7 true: PR CI job `rust` passes on the stage head, and `git grep -n -E "fn (llama_server_shares_one_kv_pool|the_kv_cache_is_one_pool_whatever_the_slots|per_model_settings_resolve_over_the_defaults|invalid_app_entries_are_dropped_at_load|per_model_settings_reach_the_launch_arguments|a_stale_model_restarts_only_when_free|set_config_saves_reloads_and_applies|more_sessions_than_the_old_parallel_value_run_at_once|calls_beyond_a_models_slots_queue_and_all_complete|a_full_pool_makes_calls_wait_not_fail|local_calls_reserve_their_estimate_and_cap_the_reply|a_local_reply_cut_by_its_reservation_is_retried_once_uncapped|a_load_never_evicts_a_model_another_session_is_using|idle_unload_spares_a_model_another_session_is_mid_turn_on|a_cpu_lane_model_another_session_is_using_is_never_evicted_for_ram|a_session_parked_on_an_approval_holds_no_claim|claims_of_another_session_count_as_busy|concurrent_loads_take_turns_and_all_finish)\b" crates` lists all eighteen.
  - `git diff <stage1>..HEAD -- packages apps` is empty.
  - G9 (backend part): the sections listed for docs 04, 10 and 13 are updated; PR CI job `docs` passes.
- Status: pending
- Outcome:
- Handoff notes: the `Claimed` → Busy mapping per requester is the whole item-5 change in the arbiter's terms; do not add a timeout that evicts another session's claim. Keep `swapping` the only memory mutex. `Activity` counts must not double-count a call waiting first for tokens then for a slot. Windows: `std::fs::rename` replaces an existing `models.toml`. Model ids may contain dots: always write them as quoted TOML keys (the `toml` crate does) and never look them up through `Config::explain`'s dotted paths.
- Implementer traps (from review):
  - `residents()` locks `Presence::users` itself today. `plan()` must hold that guard until the victims leave `state.loaded`, so thread the guard through: `residents(&state, &users, protect, session)` takes the guard as a parameter. Locking `users` twice deadlocks: std mutexes are not reentrant.
  - `Presence::claims` becomes a tri-state, not a bool: `enum Claim { None, Own, Others }`, where `Others` wins when holders of both kinds claim. `residents` maps `Others` to `Use::Busy`, `Own` to `Use::Claimed` and `None` to `Use::Free` (after the Busy and protect checks). Plain `availability` treats `Own` as `Others`.
  - The forbidden-flag check compares each `extra_args` item's text before the first `=`, so `--ctx-size=4096` and `-c=4096` are refused; a value alone (`4096`) is never a flag.
  - vLLM: the token pool applies to llama.cpp only, because vLLM is not slot-aware. A vLLM lease reserves no tokens and has `max_tokens: None`; `pool_tokens` only sizes its memory plan. Do not create a token semaphore for vLLM pools.
  - The G6 coverage is the four named tests, including the idle-unload exemption (`idle_unload_spares_a_model_another_session_is_mid_turn_on`) and the CPU-lane path (`a_cpu_lane_model_another_session_is_using_is_never_evicted_for_ram`); do not fold them into one.

## Stage 3: desktop dialog

- Objective: a modal to view and edit local models' llama.cpp settings and model-table rows, saving through `models.set_config`.
- Depends on: Stage 1 (types). Runs in parallel with Stage 2; until Stage 2 lands, the daemon answers the new methods with "method not found", so develop against the pinned types (a temporary in-memory stub in the component, removed before handoff) and verify end to end in Stage 4.
- Owned paths: new `apps/desktop/src/ModelConfigDialog.tsx`; `apps/desktop/src/App.tsx`, `apps/desktop/src/SettingsMenu.tsx`, `apps/desktop/src/panels/LocalModelsPanel.tsx`, `apps/desktop/src/panels/ModelsPanel.tsx`, `apps/desktop/src/styles.css`, `apps/desktop/src/daemon/client.ts` (handshake `minor: 3`); `docs/07-desktop-ui.md`. Not `packages/protocol/**`.
- Design:
  - There is no modal in the app yet (the settings menu and icon picker are `role="dialog"` popovers). Use a native `<dialog>` opened with `showModal()` in an effect (focus trap, Esc and backdrop for free); `onCancel` closes; `aria-labelledby` its heading. Reuse `.setting`, `.primary`, `.ghost`, `.badge`, `.muted`, `.warning` classes and the CSS custom properties (`--ui-scale`, `--space-scale`) so density and scaling apply.
  - Opened from three places: a "Models…" button in `SettingsMenu` (new `onOpenModels` prop), a "Configure" button in the Local models panel toolbar (opens the Local tab) and one in the Models panel toolbar (opens the Model table tab). `App` owns `modelDialog: null | { tab: "local" | "table"; id?: string }` and refreshes `models.list` and `models.local` after a save.
  - Layout: heading "Models"; tabs Local and Model table; a list of models on the left (id, runtime or location badge, "loaded", "reload pending"); the form for the selected model on the right; footer with the error text, Cancel and Save.
  - Local form, every field optional with the effective value as placeholder and a per-field reset (sends `null`): context per session, KV pool tokens, slots, K cache type and V cache type (select of `cache_types` plus "default (…)"), flash attention (default, auto, on, off), max GPU layers, max wait (s), max output tokens (0 = uncapped), extra arguments (one per line). For `runtime === "vllm"` the cache types, flash attention and GPU layers are disabled. Show `required_bytes`, `kv_pool_bytes` and `placement` from a debounced (500 ms) `models.set_config` with `dry_run: true`; show its error inline. Client-side checks mirror the server (integers ≥ 1, pool ≥ context, quantised V needs flash attention not off) but the server is authoritative.
  - Model table form: enabled (checkbox), tier (select), tags (comma-separated), context window, input and output price per Mtok (both or neither).
  - Save sends only models whose form differs from `saved` (each as a whole-settings update), then closes and refreshes; a failure keeps the dialog open with the message. "Reset model" clears all its saved settings.
  - Local models panel: when loaded, the placement badge reads `loaded · <in use>/<slots> busy · pool <reserved>/<pool_tokens>`. Models panel: rows with `enabled === false` use the existing `unusable` style and an "off" badge.
- Acceptance checks:
  - G8 (static part): `pnpm install`, then `bash scripts/check-web.sh` exits 0 locally. For the desktop this is typecheck and build; there is no desktop lint or test script.
  - `git diff <stage1>..HEAD -- crates packages` is empty; no stub remains (`git grep -n "stub" apps/desktop/src/ModelConfigDialog.tsx` finds nothing).
  - G9 (UI part): `docs/07-desktop-ui.md` describes the dialog and its entry points; PR CI job `docs` passes.
- Status: pending
- Outcome:
- Handoff notes: `LocalLaunchSettings` fields are all required in TypeScript (`T | null`); send `null`, not `undefined`. Numbers from inputs must be integers (`Number.parseInt`); empty input means `null`. Errors arrive as `RpcError` from `daemon/client.ts`.

## Stage 4: integration (root)

- Objective: verify the combined result.
- Depends on: Stages 2 and 3 merged on one branch.
- Owned paths: none (fixes go back to the owning stage).
- Acceptance checks:
  - G1–G7: PR CI job `rust` passes on the integrated head.
  - G8: `bash scripts/check-web.sh` exits 0. Then, in the running app (root only: a daemon root builds locally from the integrated head with `cargo run --bin imperiumd`, plus `pnpm --filter @imperium/desktop tauri dev`, per decision (c); no clippy or tests locally): the dialog opens from all three entry points; changing a local model's K cache type and slots and saving writes `<config>/models.toml`, `models.config` shows the new `saved` and `effective`, and `models.local` shows the new slots (after reload); switching a model-table row off greys it in the Models panel and routing no longer picks it; an invalid value shows the server's error and writes nothing.
  - G9: PR CI job `docs` passes; the four docs read consistently with the shipped behaviour.
- Status: pending
- Outcome:
- Handoff notes: none yet.

## Docs to update

- `docs/04-models-and-routing.md` [Runtimes](../04-models-and-routing.md#runtimes):
  - "A lease per model call" and "A queue": token reservation, `max_tokens`, the uncapped retry, and waits for the pool.
  - "Slot affinity": keep the pinned-slot switch through the host prompt cache. Add that Imperium starts llama-server with `--no-cache-idle-slots`, so an idle slot keeps its cells for the swap-out save, and that under pool pressure llama-server still purges idle slots, after which the agent's prompt is read again.
  - "Who may be swapped out" and "When": usage locks per session, approvals and usage-limit pauses releasing like `await_tasks`, the attach-to-first-call gap, and the starvation bound (D-C).
  - The custom-binary requirement: `runtime.llamacpp.custom_path` needs llama.cpp b12180 or later for `--kv-unified`, `--kv-unified-per-slot` and `--no-cache-idle-slots`.
  - "Slots × context (stage 2c, partly open)": replaced by the unified pool, its defaults and the "no global cap" interpretation (D-B).
  - The CPU-lane block: `parallel`, `context` and `max_wait_s` are now per-model settings (rows still accepted), and `context × parallel` becomes the pool.
  - The phase 7 vLLM arguments: `--max-model-len` = context, no token pool.
  - The last bullet list (`--parallel`).
- `docs/10-configuration.md`:
  - [Layers](../10-configuration.md#layers): the app layer `<config>/models.toml` between the user file and environment variables; it wins on conflict, and invalid entries are dropped with a warning at start.
  - "Implemented settings": `slots`, `pool_tokens`, `flash_attn`, `max_output_tokens`, `runtime.llamacpp.models.<id>.*`, `models.overrides.<id>.*`, `enabled` on catalog rows, `parallel` deprecated.
  - "Example".
- `docs/02-architecture.md` (Stage 1): protocol version 0.3.
- `docs/13-decisions.md`: add accepted decisions for D-A to D-D (pool budgeting with the hard reservation, no global cap, usage locks with the await and approval exception, the app layer).
- `docs/07-desktop-ui.md` [Panels](../07-desktop-ui.md#panels): the model settings dialog and its three entry points; the pool readout in the Local models panel.

## Risks and revisit conditions

- Token estimates: overestimates waste pool; a cut or overflowing reply costs one uncapped retry. Revisit with logged retry counts.
- Idle slot purge under pool pressure (llama-server's `try_clear_idle_slots`) can empty a sleeping agent's slot before its model is swapped out; its prompt is then read again on return. Revisit if swap-ins after busy periods show long prompt reads.
- Per-slot state not planned: SWA/hybrid context checkpoints and recurrent per-sequence state grow with slots; default 8 keeps this near today's scale. Revisit if RAM use on hybrid models exceeds the plan.
- Waiting orchestrators may be swapped out by other sessions (D-C). Revisit if users report it; a cheap refinement is to prefer evicting the requester's own session's waiting models first.
- `--kv-unified-per-slot` and `--no-cache-idle-slots` are recent upstream flags; if a later pinned tag renames them, the backend arguments and the G2 test change together.

## Open questions for the user

None. The earlier two were answered (decisions (a) and (b) in the header).
