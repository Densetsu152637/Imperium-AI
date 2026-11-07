# Host adapters

Load the running host's section to map host-neutral terms (dispatch, fresh context, effort, worktrees, cancellation, celebration) to tools. Adapters change no policy, authority, or priority; host system instructions and user settings take precedence. A control absent from the live tool list is unavailable.

## Claude Code

Loading:

- `CLAUDE.md` imports [AGENTS.md](../AGENTS.md) with `@AGENTS.md`. Imports are eager; companion links are not. Resolve links relative to the containing file.
- For use in every project, `~/.claude/CLAUDE.md` imports this repository's `CLAUDE.md` by absolute path, escaping spaces (`@D:/Git\ Repositories/Agent-Custom-Workflow/CLAUDE.md`).
- That personal file may declare the root personality with a line such as `Personality: luxion.md`; see [personality selection](../personalities/README.md#selection).
- Subagents load `CLAUDE.md`, so they receive [AGENTS.md](../AGENTS.md) and act as a leaf within their prompt's scope unless it assigns a role; only the top-level conversation agent is root, and no agent but root loads a personality. Built-in `Explore` and `Plan` agents skip `CLAUDE.md`; use them only for routing discovery that needs no manifest.

Workflow term mapping:

| Workflow term | Claude Code control |
| ------------- | ------------------- |
| Dispatch a child | `Agent` with `subagent_type: "general-purpose"`; the brief and manifest carry role, profile, and owners; the brief begins with the [`Role: <leaf\|manager>; Profile: <name>` tag line](../templates/subagent-brief.md#compact-schema) so [the agents panel](https://github.com/Densetsu152637/The-Crew) can identify it. `general-purpose` is only the host carrier; the tag line's profile is always required, since a missing or unknown tag shows as general-purpose in the panel. |
| Fresh context | Any non-`fork` `subagent_type`. `fork` inherits the parent transcript; use it only when inheritance is justified under [dispatch](../workflows/manager-delegation.md#dispatch-and-acceptance). |
| Reuse a worker | `SendMessage` to the agent ID returned at dispatch. |
| Start ready assignments in parallel | Several `Agent` calls in one message; they notify on completion. Do not poll. |
| Cancellation | `TaskStop`. |
| Depth limit | Three subagent layers below the main conversation by default (`CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH`). At the limit the `Agent` tool is removed. |
| Turn limit | No per-dispatch control; turn limits are monitored allowances. |
| Model selection | `Agent` `model` parameter (`haiku`, `sonnet`, `opus`, `fable`). |
| Effort | `Agent` `effort` parameter; this workflow's [effort defaults](../AGENTS.md#execution-defaults) are the explicit instruction that permits setting it. |
| Parallel write isolation | `Agent` `isolation: "worktree"`, or `EnterWorktree`/`ExitWorktree` for the current session. The app syncs a worktree with its base through `sync_with_base_branch` where available. See managed worktrees below. |
| Routing checkpoint / progress | Inline text; the desktop app's task list (`update_plan`) tracks root progress. |
| Plan and usage telemetry | Desktop app `mcp__ccd_session_mgmt__get_usage` (deferred; load with ToolSearch `select:mcp__ccd_session_mgmt__get_usage`). `plan.plan` is the tier name (observed "Max", no multiplier); `plan.windows[]` carry `label` ("5-hour limit", "Weekly · all models", "Weekly · <model>"), `percentUsed`, `resetsAt`. `status` "not_applicable" means API key, Bedrock, Vertex or gateway; "unavailable" means retry per its note. CLI without the app: the status-line JSON `rate_limits` (`five_hour`, `seven_day` `used_percentage`) is user-visible only, not an agent tool, so treat telemetry as unavailable unless a mod relays it. Where the Hit The Ground Running mod is enabled, its `[Hit The Ground Running]` usage notes in context count as telemetry, and its `Agent` spawn denial is the usage gate's hard stop; a denied spawn stays queued. |
| Wait for a free agent-pool slot | Completion notifications from `Agent`; do not poll. |
| Relay a usage-gate halt | `SendMessage`. |
| Completion celebration | None available; skip it. |

Tier mapping, unless the user supplies another: Economy `haiku`, Specialist `sonnet`, Frontier `opus`; map other aliases only from documented capability.

Managed worktrees (where available; verify against the live tool list). Policy, records, and cleanup stay with [worktrees](git-worktrees.md):

- `Agent` `isolation: "worktree"` removes the worktree when the agent made no changes; otherwise the result returns its path and branch, which the manager enters in the [worktree record](git-worktrees.md#worktree-record) and reconciles under [safe cleanup](git-worktrees.md#safe-cleanup).
- `EnterWorktree` creates (under `.claude/worktrees/`) or, with `path`, enters a worktree; when a [configured location](git-worktrees.md#parallel-work-and-worktrees) applies, create it there with `git worktree add` and enter it with `path`; `ExitWorktree` acts only on one `EnterWorktree` created this session. Root and managers use them; a worker only when assigned. Record the worktree; never pass `discard_changes: true` to bypass a refusal, which returns to the [handoff gate and recovery](git-worktrees.md#handoff-gate-and-recovery) preserve step.
- The desktop app's `clean_up_worktrees` and `discard_kept_worktree` are the archive path for app-made worktrees, used under [safe cleanup](git-worktrees.md#safe-cleanup); their `include_uncommitted`, `delete_sessions`, and discard options are the unrecoverable-work tools that rule gates.
- The desktop app creates session worktrees under its `worktree_location` (read-only in `ccd_settings` `get_settings`, where available). When a [configured location](git-worktrees.md#parallel-work-and-worktrees) applies, compare the two before creating a worktree; on a mismatch, report both paths to the user instead of choosing one.

Additional folders:

- `/add-dir`, `--add-dir`, or the app's folder controls grant access to other directories. Their `CLAUDE.md` loads only with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`; their `AGENTS.md` does not load. Treat their instructions as unloaded until read.

## Codex

Loading:

- The personal file is `~/.codex/AGENTS.md` (Windows: `C:\Users\<you>\.codex\AGENTS.md`). It references this repository's [AGENTS.md](../AGENTS.md) by absolute path using the link-reference form: "Read [AGENT-INSTRUCTIONS](D:/Git Repositories/Agent-Custom-Workflow/AGENTS.md) and assume its configuration." followed by "Resolve companion links relative to the instruction file that contains them." The second line matters: without it, relative companion links may be resolved against the personal file's directory. There is no import; companion links are followed lazily. This repository's `CLAUDE.md` is not used.
- That personal file may declare the root personality with a line such as `Personality: luxion.md`; see [personality selection](../personalities/README.md#selection). Complete example: [README Codex setup](../README.md#codex).
- Subagents never load a personality. The brief and manifest carry the role and profile; do not include personality files.

Workflow term mapping:

- Dispatch, parallel start, model selection, effort, depth and turn limits, and parallel write isolation: Codex's agent tools (`spawn_agent`), configured under `[agents]` in `~/.codex/config.toml`. Every mapping below is subject to the live tool list; a control absent there is unavailable. Apply the [worktree](git-worktrees.md) policy with plain Git where no managed isolation is offered.
- Fresh context: `spawn_agent` with `fork_turns: "none"` (multi-agent V2) or `fork_context: false` (V1); always set it explicitly, because V2 omitted `fork_turns` defaults to `"all"`.
- Inherited context: `fork_turns: "all"` (V2) or `fork_context: true` (V1). Full-history forks reject `model`, `reasoning_effort` and `agent_type` overrides, so use them only when inheritance is justified under [dispatch](../workflows/manager-delegation.md#dispatch-and-acceptance) and no override is needed.
- Reuse: `followup_task` (V2), `send_input` (V1). Relay a usage-gate halt: `send_message` (V2), `send_input` (V1). Wait for completion: `wait` where listed.
- Cancellation: `close_agent`.
- Tier mapping (Economy, Specialist, Frontier): none defined; use the user's mapping, else the models Codex exposes by documented capability.
- Routing checkpoint / progress: inline text.
- Plan and usage telemetry: unmapped; use `local-paths/plan.md` for the [agent pool](../workflows/manager-delegation.md#agent-pool) cap.
- Additional folders: unmapped; treat instructions in other directories as unloaded until read.
- Completion celebration: `mcp__codex_app__fire_confetti`.
