# 07 — Desktop interface

## Implemented in phase 2

- The Tauri 2 shell (`apps/desktop`) reads the daemon's endpoint file through a native command
  and the React interface connects over the WebSocket with the subprotocol token. In a plain
  browser (development preview) it asks for the URL and token instead.
- The default view below, built from columns of spanning or stacked panels with draggable
  dividers; sizes are remembered, and stacked panels fold to their header
  ([folding](#columns-spanning-and-stacking)). The [panel editor](#panel-editor-implemented)
  moves panels between columns and adds or removes columns.
- Session panel: the title and status at the top, conversation with assistant replies rendered
  as Markdown (GitHub-flavoured, raw HTML disabled; an option shows the raw text), inline
  permission prompts (allow, always this session, always allow by a rule, deny; see
  [permissions](#permissions-implemented)), a floating composer, and under it a
  footer with the model selector and the context meter
  ([session footer](#environments-and-sessions-implemented)).
- Telemetry panel: a live table of the models in use right now across every session and
  subagent, one row per model, updated in place as calls finish (not a history). It is a
  GitHub-flavoured Markdown table drawn with the shared compact table: the figures stay on one
  line and align right, and the model column takes the rest of the width and ends in an
  ellipsis (its full name is on hover; a keyboard user cannot reach that hover, a known limit).
  Under 22 rem the `in` and `out` columns give way to a muted second line under the model:

  | Model | tok/s | in | out | cost |
  |---|---|---|---|---|
  | Qwen 3 (4B) | 120.0 | 1.00M | 1.60k | — |
  | Claude (Sonnet) | 60.0 | 54.00k | 2.00k | $1.60 |

  A model is active while an agent using it works: a session's root agent during a turn
  (`turn_started` to `turn_completed` / `turn_failed`, unless the session is stopped) or a
  subagent whose task is running. `in` and `out` add up the prompt and completion tokens of
  that work only (the root's calls since its turn started, each running task's calls since it
  was spawned or resumed), across agents sharing the model. `tok/s` is the latest call's
  generation speed (`tokens_per_sec`), one decimal. Counts under a thousand are plain, larger
  ones two decimals with `k` or `M`. `cost` is dollars with two decimals from the calls'
  `cost_usd`, and an em dash for local models (a local runtime's spec or a table row marked
  local) or when no cost was reported. The model shows its model-table id, else the spec
  without its endpoint (`modelLabel` in `models/telemetry.ts`, the one place to switch to aliases).
  A row the window saw go idle stays for a minute with its last values, then drops off.
  Session totals and per-turn usage stay in the session panel.
- Display settings: UI scale, prose and code text scale, density, and line length. They are
  stored per viewer in local storage for now and move into the daemon's configuration with
  the settings UI.
- Light and dark themes follow the system: flat graphite surfaces (dark) and light neutral surfaces
  (light), quiet hairline borders and one accent, with the chrome finish kept to the wordmark.
  Side columns are plain panels; the conversation is a centred column with the user's turns
  as pills on the right.

- Detachable panels: every panel, built-in or from a mod, has *Open in its own window* in its
  header's ⋯ menu, which moves it to a window of its own. A detached window shows only that
  panel under a slim header with the panel's primary action, its ⋯ menu when it has secondary
  actions, and two icon buttons: Pin (always on top) and Dock. Many can be open at once; each keeps
  its position, size, and pin in the saved layout, and all reopen at the next start (moved onto
  a visible monitor if theirs is gone). Closing one with the OS button docks it; quitting from
  the main window leaves them detached. Only the main window writes the layout, so detached
  windows report pinning, moving, and docking to it; documents are synced between windows, and a
  detached window follows the main window's selected project and session. In a plain browser
  the window is a popup and cannot be pinned.

### Agents panel (implemented)

The Agents panel is a compact list of the selected session's subagents, one line each: a status
dot (`running` pulses, `paused`, `finished`, `failed`), the subagent's title, and how long its
current run has taken (counting up while it runs, from the `at_ms` times on its task events;
older journals show none). Hovering a line shows its task id, model, and cost. Titles are the
3-to-6-word names the root agent gives `spawn_agent` (or ones derived from the brief,
[03](03-agent-engine.md#subagent-titles)); task ids such as `t1` are secondary everywhere: the
transcript's delegation line (*→ Audit the auth flow · t1 · model*), permission requests from a
subagent, the context breakdown's agent list, and "Continued <title> on …" notices all name the
title. With no subagents the panel shows the shared empty state. While the session is stopped,
running subagents show as `paused`. A paused subagent's ⋯ menu also has **Resume** and **Cancel**
(`session.agent_cancel`); archived sessions offer neither.

A subagent kept as a [knowledge agent](21-knowledge-agents.md) shows a **Knowledge** badge after its
title (its topic and status on hover), and its ⋯ menu has **Retire knowledge agent**; a finished one
that is not kept has **Keep as knowledge…**, which opens its view. In the view
(`panels/KnowledgeSection.tsx`) a finished subagent offers **Keep as knowledge**, which asks what it
knows about and then keeps it (the daemon refuses writers, single-call and chunk agents and
conversations that leave no room for answers, and the reason is shown). A kept agent's view lists
its card (topic, model, what it read with the changed files marked, the share of its files that
changed, its context, what one question costs, how much of the last one was cached, and any web
pages or tool results in its context), its questions newest first with who asked, how each ended,
what it cost and the answer (marked when degraded or possibly holding outside material), an
**Ask** box that asks it as you (`session.agent_ask`; the question and answer appear in the list),
and **Retire**. The cards and questions come from the session's events (`sessions/knowledge.ts`).

Choosing a line (or the subagent's title in the transcript) opens its log in the main session
panel in place of the root's transcript; choosing it again, the breadcrumb's **←
<session title>**, or Escape outside a text field goes back (`panels/AgentView.tsx`). The log is
the subagent's transcript drawn exactly as the root's (its brief and follow-ups as messages,
steering, replies, tool calls and their [folded groups](#tool-calls-implemented)), its plan,
permission requests it waits on, the [waiting status line](#waiting-status-line-implemented)
before its first output, and its failure or pause. The footer shows its task id, model, run
time, and cost.

The view's composer messages the subagent directly, as the root composer does the root:

- While it runs, Enter queues the message for its next turn (`session.send` with its
  `agent_id`); its queue shows above the composer with **Edit**, **Steer**, and **Remove**
  (`session.queue` with its `agent_id`), and **Steer** (or Ctrl+Shift+Enter) joins its running
  turn at its next safe point. Steering a subagent pauses nothing; it reads the message as
  steering, as the root does ([03](03-agent-engine.md#queueing-and-steering)).
- A finished or failed subagent takes the message as a follow-up (`session.agent_resume`): it
  continues with its earlier conversation, its line goes back to `running`, and its cost adds
  up across runs. A subagent a daemon restart cut off is shown as failed and is continued the
  same way. Files attached to a follow-up are named in its text, since follow-ups carry none.
- A paused subagent (stopped for a steering message to the root) shows **Resume** (continues
  from its saved conversation) in the view's header, with **Cancel** in the ⋯ menu beside it; a
  message re-briefs it.
- A message sent just as the subagent finished is sent again as a follow-up.

### Session column (implemented)

The session panel, a subagent's log and the new-session view share one column (`styles/session.css`).
Everything under the header sits in it: the transcript, the plan, banners, the queue, the footer
drawer, the composer and the footer share one reading width and one side gutter
(`--session-gutter`: 16 px, 12 px when the panel is narrower than 36 rem) and are separated by
`--sp-2`, so no component touches the prompt box's side padding and the footer's controls sit
under the well's own padding. The width is the *Max prose width* setting times the prose scale,
resolved once on `.session` (`--col-max`, a registered length) so a rule's own font size never changes it.

- **Transcript:** the user's turn is a soft `--surface-active` bubble at `--radius` on the right of
  the column; the assistant's turn is unboxed prose at `--fs-base` and `--lh-prose` (times the
  prose scale); headings, lists, block quotes, tables and code use the type scale. Tables and code
  blocks scroll inside themselves, so a narrow column never scrolls sideways. Failures and
  notes in the transcript are `Notice`s (*Turn failed* with the daemon's reason); the error paragraph
  elsewhere keeps `.error`.
- **Tool calls:** one quiet row each, 24 px tall: a chevron, the state, the tool's name in the code
  face, the summary (which ellipsizes), and the run time. Opened, the arguments and result hang off a
  hairline under the chevron.
- **Composer:** one `--radius-lg` well on `--surface-sunken` with a `--border-control` edge, holding
  the attach and microphone buttons (`IconButton`), the message field, and a `--control-h-lg` send
  button. Typing in it draws the one 2 px `--focus` ring on the well's edge. Attachment chips and the
  push-to-talk status sit above it; queued messages stack above them. The footer under it (model,
  effort, approval mode, cost, options, context meter) is `--fs-sm` and muted; the effort control
  stays, greyed out, for a model that cannot reason.
- **Waiting:** the *Working…* line, the core's wait line (spinner, text, elapsed time and one action)
  and steering marks use the same type scale; the spinner and dots stop under reduced motion.

### Tool calls (implemented)

Every tool call, the session's own in the transcript and each subagent's in its log
([above](#agents-panel-implemented)), is one quiet line, collapsed by default: a spinner, check, or cross, the tool's name, a
short summary of what it does (`write src/main.rs`), and how long it ran. Opening the line
shows the arguments (JSON, indented) and the result; text over about 1,200 characters or 16
lines folds behind **Show more**. A call denied or blocked before it ran shows its reason on
the line and opens only when the reason runs past one line. The body is built only while open,
so long transcripts stay light. The daemon sends the arguments on `tool_started` and the tool
name and run time on `tool_finished`, each cut to 8,000 characters for display; the model
always receives the full result ([03](03-agent-engine.md#agent-loop)).

Consecutive calls, with no other entry (assistant text, a message, an approval) between them,
fold into one line so a busy turn does not flood the transcript: what they did by kind, in the
order each kind first ran (*Read 3 files, ran 2 commands, searched 1 time*; tools without a
kind count as *used 2 other tools*, or *Used 4 tools* when none has one). While any of them
runs the line shows a spinner and the latest running call's summary; once all have ended it
shows a cross when any failed, otherwise a check. Opening it lists the calls as the rows above,
each still opening on its own. A lone call stays its own row. A subagent's log folds its calls the same way, and a session search hit on a folded call lands on its group's
line (`apps/desktop/src/sessions/toolCalls.ts`, `components/ToolGroup.tsx`).

### Free scroll while streaming (implemented)

The session transcript and a subagent's log follow the newest output as the agent writes, while
*Follow new output* (Settings → Chat) is on. Scrolling up by wheel, keyboard (Up, Page Up, Home,
Shift+Space), touch, or the scroll bar stops following at once, even mid-stream, and from then
on new text, tool groups opening or closing, and Markdown re-rendering leave the reader's place
alone (the browser's scroll anchoring keeps what is on screen still). A quiet **Jump to latest**
pill with a down arrow floats over the bottom of the transcript while it is not at the end; it
shows how many items arrived since following stopped, or a dot when only streamed text did.
Choosing it, scrolling back to within 32 px of the bottom, sending a message, or opening another
session or subagent follows again. A session search hit stops following so the hit stays in view.
The rules are pure functions in `apps/desktop/src/sessions/follow.ts` (tested in `test/follow.test.ts`);
`sessions/useFollowScroll.ts` wires them to the transcript for both views, and
`components/JumpToLatest.tsx` draws the pill.

### Waiting status line (implemented)

After a message is sent, and before the agent's first output (a streamed token, a tool call,
or the end of its turn), the core may be busy with work that holds the turn up. A status line
under the sent message says what, with an elapsed timer and the action that ends or answers the
wait; it clears on the first output. A message this window sent shows at once, dimmed, until
its `turn_started` adds it to the transcript (or `message_queued` puts it in the queue, or the
session turns out to need a model). The line reads, from the session's events
(`apps/desktop/src/sessions/waits.ts`):

- `local_model_waiting` by reason, one sentence naming the model by its alias: *Loading
  Qwen 3…*, *Growing Qwen 3's context pool…*, *Waiting for room in Qwen 3's context cache…*,
  *Waiting for a free slot on Qwen 3…* (queued behind other agents' calls on the model),
  *Waiting for another model before Qwen 3…*, *Making room on the GPU for Qwen 3…*, *Saving
  other agents' context to make room for Qwen 3…*, *Restoring this agent's context on Qwen 3…*;
  a reason a newer daemon adds is worded from its name. While a model waits to load and
  llama.cpp is being installed (`runtime.install_progress`) it reads *Installing llama.cpp
  (42%)…*. These offer **Stop** (`session.control` `stop`).
- A `model.residency` move made for the waited-on model since the wait began (on the model
  itself, or an eviction whose `detail` says it made room for it) replaces that sentence with
  why memory is moving, one sentence per reason: `crashed` *Restoring Qwen 3 after a crash…*,
  `room` *Swapping out Gemma to make room for Qwen 3…*, `more_gpu` *Swapping out Gemma so Qwen
  3 fits on the GPU…*, `swap_gain` *Swapping out Gemma to give Qwen 3 more of the GPU…*,
  `gpu_rule` *Making room on the GPU for Qwen 3…*, `critic` *Giving the GPU to Qwen 3 while
  its review runs…*, `aged` *Swapping out Gemma, loaded longest, for Qwen 3…*, `grow`
  *Restarting Qwen 3 with a larger context pool…*, `promote` *Moving Qwen 3 onto the GPU…*,
  `config` *Restarting Qwen 3 with its new settings…*, `resume` *Bringing Qwen 3 back for this
  agent…*, `yield` *Saving a waiting agent's context on Qwen 3…*, `stopped` *Saving a stopped
  session's context on Qwen 3…*, `idle` and `request` *Reloading Qwen 3 after it was unloaded
  …*. The call's own load or restore (`call`), the budget, and failed moves keep the
  `local_model_waiting` sentence; a reason a newer daemon adds is worded from its name.
- `compaction_started` until `context_compacted` (or a notice or the turn's failure):
  *Compacting conversation…*, with **Stop**.
- An unanswered approval or questions: *Waiting for your approval…* / *Waiting for your
  answers…* (the card above has the actions).
- A pause: *Usage limit reached; waiting until 14:00*, *Paused while the machine is busy…*, or
  *Waiting for a quiet machine…*, with **Resume now**. A stopped session has its own banner.
- A used-up budget that asks first (`-32008` with `ask`): *A budget is used up: …* with **Send
  anyway** (sends again with `confirm_budget`) and **Don't send**, in place of the native
  confirmation dialog this used before.
- Sent, with nothing reported yet: *Starting…*. Once the turn runs with no known wait, the line
  is the usual *Working…*.

A message sent while the agent is busy waits in the queue, which already shows it.

### Review panel (implemented)

The Review panel (beside Agents; closable and detachable like the others) shows what the open
session changed and puts it back or commits it (roadmap item 6):

- **Since:** the shared Select of the session's checkpoints, the first (*session start*) by
  default, each named by the message it came before; the list follows new checkpoints
  (`session.checkpoint`) and is read again whenever a turn ends. Under 22 rem the *Since* label
  sits over the picker.
- **Files:** one line per changed file (status letter, path, `+added −removed`), open to its
  unified diff, added and removed lines tinted. Each hunk header, and each file line, has
  *Accept* (keep it; it stops being shown) and *Reject* (revert it in the working tree) as row
  tools that appear on hover or focus. Binary and large files are accepted or rejected whole.
- **Commit:** a message box prefilled with the session's title and its files, *Commit*, and
  *Open pull request* (the message's first line is the title, the rest the body). Both ask
  first, inline, unless a permission rule allows them (`git_commit`, `open_pull_request`);
  without `gh` the pull request's commands are shown to copy. A repository picker appears when
  the session has several (the same Select). Failures, successes and the missing-`gh` hint are
  shared Notices; an empty review, a binary file and a session with nothing to review use the
  shared compact empty state, and the first read shows the delayed loading state. Rows wrap
  instead of scrolling sideways, diff lines included.
- **Rewind:** the history menu beside the picker rewinds to a checkpoint: the files, the files
  and the conversation, or only the conversation; a conversation rewind opens the new session it
  makes. Everything is disabled while a turn runs.

It uses `session.review`, `session.review_hunk`, `session.review_commit`,
`session.review_pull_request`, and `session.rewind`
([review pane](14-projects-and-automation.md#review-pane-implemented)). Sessions outside git say
why there is nothing to review.

### Preview and Browser panels (implemented)

Two panels show pages beside the chat (roadmap item 9). Both are closable and detachable like
the others and follow the open session.

- **Preview** shows the page the session's project serves. An agent opens it with
  `preview_open` ([05](05-tools-and-isolation.md#live-preview-implemented)); a `preview_changed`
  event (`url`, and the `command` that serves it) fills the panel, and the panel says which
  command it started and that it stops with the session. The page is an iframe, so the dev
  server's own hot reload keeps it current; *Reload* fetches it again. The address bar also
  takes an address the user types, which wins until the agent opens a different page. Only
  `localhost` and `127.0.0.1` (http or https, no user name or password; not `[::1]`, which the
  window's content security policy cannot name) are shown: the page runs scripts inside the
  app window, so `previewUrl` (`src/sessions/preview.ts`) refuses anything else with the reason (and
  checks every page it frames, including the one an agent opened), the daemon applies the same
  rule to agents' URLs, and the window's content security policy allows frames from those
  origins only, so a redirect cannot load a remote site either. A development build is served
  from localhost itself, so the app's own origin is refused too. The iframe is sandboxed to
  scripts, forms, and modals, with `allow-same-origin` so the project's page keeps its own
  origin (cookies, storage, hot-reload socket); that is safe because its origin is never the
  app's. No popups or top navigation, and no referrer is sent. The daemon's `preview_open` asks
  for a URL unless it is the port the session's own preview command started.
- **Browser** mirrors the agent's browser (`browser_view`) and lets the user take it over.
  *Take over* (`session.browser_control`, `user: true`) starts the browser if needed, shows its
  page, and replaces the bar with Back, Forward, Reload, an address field, and *Give back to
  agent*. While the user drives, a click on the screenshot clicks that spot, typed characters
  and keys (Enter, Backspace, arrows, and the like) go to the focused element, and the wheel
  scrolls the page, each through `session.browser_act`, which answers with a fresh screenshot
  (no streaming; the page updates on interaction). Tab and Escape stay with the app so the
  keyboard is never trapped. The agent's `browser_*` tools are refused with a message saying the
  user is driving until *Give back to agent*; `browser_control` events keep every client's
  panel in step. Open tabs show in the shared Tabs above the page (arrow keys, Home and End move
  between them), each with its own latest screenshot (the `browser_view` event names its tab and
  lists them all); the agent's active tab carries a dot. Anyone can look at another tab; while
  the user drives, choosing a tab switches to it, the close button beside the strip closes the
  tab on screen and the plus button opens a new one (`switch_tab`, `close_tab`, `new_tab` in
  `session.browser_act`). The strip wraps onto more rows and long titles end in an ellipsis, so
  the panel never scrolls sideways. Downloads are not shown here: the browser refuses them and the agent's
  `browser_download` result tells the transcript what was saved.

Both panels use the shared states: the "You are driving", preview-command and error messages
are Notices, and an empty panel is the compact empty state. Their toolbars wrap, and under 22 rem
the address field takes a row of its own. The preview frame keeps a white canvas in either theme
(`Canvas` under `color-scheme: light`), so its page is not tinted by the app's theme.

### Resources panel (implemented)

The Resources panel (in Settings → Panels, closable and detachable like the others) shows what
the machine and the sessions are using: CPU, RAM, and per GPU its utilisation and VRAM as bars
with a two-minute sparkline each (hover a bar for per-core figures or swap), a table of the
local models loaded, in use or idle, with placement (or the CPU lane), the most context one agent may use
and the KV pool's reserved and total tokens, memory, busy slots and queued calls (or "held by a
turn" while an agent is mid-turn on it) or the idle-unload countdown, the sessions on each, and
an Unload button for idle ones, and a table of the hosted
endpoints sessions use with requests, tokens, latency, and errors over the last 15 minutes.
Both tables use the shared compact table, a long model name ends in an ellipsis, and the
panel's own width decides the columns: State and Sessions (or Tokens and Latency) drop below
38 rem, Placement (or Sessions) and the sparklines below 22 rem, so the panel never scrolls
sideways. Each dropped value moves to a muted second line under the model, so none is lost.
Before the first reading it shows the shared loading state, a failed read the shared notice,
and a table with no rows the shared empty state.
The panel subscribes to `system.resources_changed` only while it is open and its window is
visible, so the daemon samples nothing otherwise
([04](04-models-and-routing.md#active-models-and-resource-monitor)).

**Model download suggestions (implemented):** a `model_suggestion` event shows as a card in the
conversation, a `.card` with its title (*Download* and the repository) at `--fs-lg` and a one-row
`.table` of the model, quantisation, size and how it fits this machine (on the GPU, split with
system memory, or on the CPU), then why it is suggested, with the primary **Download**, a ghost
**Don't ask again** (which turns off *Suggest model downloads* in Settings) and a ghost × icon
button for **Not now**. Downloading shows a progress bar from `models.download_progress`;
when it is done the card offers **Switch to it** (`session.control` `set_model`). Nothing is
downloaded until Download is clicked. The answers go through `session.suggestion_respond`
([04](04-models-and-routing.md#model-download-suggestions)).

Not yet: themes beyond light and dark.

## Shell

Tauri 2 with a React + TypeScript + Vite frontend. The Tauri Rust side is thin: it attaches to a
running `imperium-core` or starts the one installed beside the app (see
[platform bundles](11-ci-cd.md#platform-bundles)), handles native menus, notifications, file dialogs, deep links, and
the updater ([desktop updates](11-ci-cd.md#desktop-updates)). All application logic goes through the daemon protocol, so the web UI could also
be served to a browser for remote use.

### Source layout (implemented)

`apps/desktop/src` holds `App.tsx` (the daemon connection, the panels and the layout), `main.tsx`
and `styles/` (the stylesheets, below), and these folders: `sessions/` (the session store hook, sessions, branching,
the queue, waits, tool calls, attachments, the transcript's follow-scroll and voice),
`models/` (model names, options, availability, preferences, settings table and memory, the
model configuration dialog and its forms, telemetry and budgets), `environments/` (the
environment window, icons, the environments hook and the lookup by id), `settings/` (the
Settings window, one file per section, the app settings and the shortcuts), `connectors/` (the
Connectors window and its views and forms), `tour/` (the first-run tour and its hook), `review/`,
and `shared/` (the connection banner, the daemon cache, the windows hook, notifications, the
store, panel windows and the icon registry), beside `panels/`, `components/`, `layout/`, `mods/`,
`commands/` and `daemon/`. Pure rules live in `.ts` files, tested under `apps/desktop/test/` (for example
`models/spec.ts` by `modelSpec.test.ts`); only those files are loaded by the tests.

**Stylesheets (implemented):** `main.tsx` imports `styles/index.css`, which imports one file per
surface in cascade order: `tokens.css` (every custom property, dark and light), `base.css`
(the page, element defaults, the keyframes), `dialog.css` (the dialog shell, its navigation,
menus and the floating lists of `Select`), `shell.css` (title bar, columns, panel headers, the
panel editor, the connect screen), `sidebar.css`, `palette.css`, `session.css` (the session
column, including the model picker in its footer), `settings.css`, `connectors.css`,
`models.css`, `environments.css`, `panels.css` (the side panels), `tour.css`,
`house-models.css`, `mods.css` and `primitives.css` (the shared components: small and
icon-only buttons, fields, the select trigger, switches, badge variants, status dots). The order
matters: rules of equal specificity are decided by it, so `primitives.css` stays last, where
those rules sat in the original single file, and a rule that must beat or lose to one in another
file is placed to keep that order. `tokens.css` holds the shared tokens, but a few surface-local custom properties live in
their surface files (`--tile` in `environments.css`, `--session-gutter` and the `--ctx-color`
set in `session.css`, `--settings-control-w`, `--settings-number-w` and `--settings-swatch` in `settings.css`), and the `.badge` base rule, `.error`
and the model picker rules (`.model-select`, `.effort-select`, `.mode-select`,
`.model-status`, `.model-note`) sit in `session.css` because of their cascade position.
`models/modelConfig.css` belongs to the model configuration
dialog, which imports it before `styles/index.css` loads; it and `models.css` hold no literals
(their budgets are zero). Each stylesheet except `tokens.css`
starts with a `/* style-budget: … */` comment counting the literal colours, font sizes and
weights, radii, shadows, durations and z-indexes it still holds; `test/styleTokens.test.ts`
fails when the count differs, so every cleanup lowers its budget in the same change, and
it also checks that every `var(--name)` is defined and counts native `<select>` elements
(`// style-budget: native-select=N` in a component file).

### The app shares the daemon (implemented)

The app attaches to whichever `imperium-core` is running, one an `imperium` command started,
one left by an earlier run, or one started by hand, and starts the bundled one only when none
answers. The one it starts is its own child process, so Windows Task Manager shows it under
Imperium. Only the previous version's `imperiumd` found beside the app is stopped and replaced.

**Lifetime:** the app is one client among others ([02](02-architecture.md#shared-daemon-lifetime-implemented)).
From its native process (not the window's WebSocket, whose process is the web view) it holds a
lease on the daemon for as long as it runs, and takes it again when the daemon restarts. The
daemon the app starts is `imperium-core --exit-when-idle`: it exits once the app and every
other program using it (an `imperium chat` in a terminal, an MCP bridge) are gone and it has
no work (a running turn, an enabled schedule, an agent with initiative, auto-fix, a download or other long job; it does not stay up for the phone app or the OpenAI-compatible API, which need Keep running), so closing
the app never ends a daemon a terminal or a schedule still needs, and nothing is left behind
when the app was the last user. The app no longer stops the daemon as it quits, and there is no
Windows job object: the lease ends with the app's process, a crash included. Sessions are saved
as they go and resume when a daemon starts again. With **Settings → Daemon → Keep running after
Imperium closes** on, the daemon is started persistent (and made so at once if it is running,
by `system.lifetime`): it outlives everything and then shows as its own process. Turning it off
makes this install's own daemon shared again; a daemon started outside the app keeps the
choice its owner made. Settings → Daemon says whether the running daemon was started by
Imperium, left from an earlier run, or started outside the app. **Stop** and **Restart** still
act at once whatever holds the daemon.

## Panels

The whole interface is made of **panels**. There is no fixed main screen: the window is a
layout of panels, and every panel — including the built-in ones — is customisable by mods.

- **Built-in panels use the public mod SDK.** Sessions, conversation, agent tree, plan, task
  inspector, files, diff, terminal, browser, worktrees, models and hardware, telemetry,
  schedules, pets, gallery, and settings are all ordinary panel packages. Anything they can do,
  a third-party panel can do.
- **Panel contract.** Each panel declares an id, a settings schema (rendered automatically in
  its settings sheet), commands, the data it subscribes to, and **extension slots** — named
  places inside it (toolbar, header, item row, footer, context menu, message renderer) that
  other mods can fill.
- **Customisation levels**, from lightest to heaviest:
  1. *Settings* — per-panel options (columns, density, filters, font overrides).
  2. *Theme* — per-panel design-token overrides.
  3. *Extend* — add content to a panel's extension slots (a cost badge on each agent row, an
     extra toolbar button, a renderer for a new message type).
  4. *Wrap* — decorate a panel with a higher-order component (for example, add a header strip).
  5. *Replace* — provide a different implementation for a panel id; the original remains
     available to fall back to.
- **Instances.** A panel type can be opened many times with different settings (for example,
  two telemetry panels, one per GPU; or conversations from two sessions side by side).
- **Failure isolation.** Each panel renders inside an error boundary; a crashing mod panel shows
  a recoverable error and a "reset to built-in" action without affecting other panels.

## Layout

### Default view

```text
┌────────────────────┬──────────────────────────────────────────────────────┐
│ Environments       │ <icon> Session title                        <status> │
│ (and folders)      │ ┌──────────────────────────────────────────────────┐ │
│                    │ │ Conversation and plan                            │ │
│ stackable          │ │                                                  │ │
├────────────────────┤ ├──────────────────────────────────────────────────┤ │
│ Sessions           │ │ (opened from the footer: context breakdown,      │ │
│ (of the selected   │ │  totals, outputs — or options)                   │ │
│  environment)      │ ├──────────────────────────────────────────────────┤ │
│                    │ │ Composer                                         │ │
│ stackable          │ └──────────────────────────────────────────────────┘ │
│                    │  model ▾                     $cost  ⚙  ▬▬▭ 12k/32k   │
└────────────────────┴──────────────────────────────────────────────────────┘
```

- **Environments panel** (top left): environments and their folders; planned: their location
  (local, container, SSH host), project-agent status, and schedules.
- **Sessions panel** (bottom left): sessions in the selected environment, one line each:
  status dot, title, and time since activity, with the rest on hover.
- **Main session panel** (middle, full height): the selected session, or the
  [new-session composer](#environments-and-sessions-implemented) when none is open. It is built
  from sections — title bar, conversation and plan, composer and its footer (model, cost,
  options, context meter), context breakdown (tokens by system
  prompt, tools, workflow context, memories, and conversation against the window size; see
  [context breakdown](#environments-and-sessions-implemented)), outputs, and worktrees —
  each of which is itself a movable, replaceable module.

Wide screens add further columns (agent tree, telemetry, terminal, browser) through layout
presets; see [scaling and wide displays](#scaling-and-wide-displays).

### Columns, spanning, and stacking

- The window is a row of **columns**; each column holds either one **spanning** panel or a
  **stack** of panels.
- A panel declares its height behaviour:
  - `span` — always takes the full height of its column (default for the main session panel).
    Other panels share its column only when the panel editor puts them there; it never folds.
  - `stack` — shares its column with other stackable panels, split by draggable dividers,
    collapsible to a title bar, and tabbable when space is short (default for projects and
    sessions).
  The user can override a panel's default in its settings.
- Columns and stack dividers are resizable; sizes are stored as proportions so layouts adapt
  across monitors.
- **Folding (implemented):** each stacked panel has a chevron before its title that folds it to
  its header (double-clicking the header's title or empty space does the same); the open panels
  in its column take the room it gives back, and a divider between two open panels still
  resizes them across folded ones. The first header of a column also has *Collapse all* /
  *Expand all in this column* in its ⋯ menu (see [Quiet interface](#quiet-interface-implemented)). The
  chevron is a button with `aria-expanded` naming its panel, so folding works from the keyboard.
  The fold animates briefly and not at all under the system's or the app's *Reduce motion*.
  Folded panels stay mounted (a mod's frame keeps its state) and are kept in the local layout
  with sizes and closed panels; Settings → *Reset layout* opens them again.

### Modules inside panels

- **Mods live in panels.** A UI mod contributes **modules** (self-contained views such as a
  telemetry chart, a pet, or a cost meter) that can be placed into a panel's section areas or
  slots, or opened as panels of their own.
- Built-in sections (environment header, context breakdown, outputs, worktrees, ...) are
  modules too, so users can reorder, remove, or swap them with mod-provided modules.

### Panel editor (implemented)

- The toggle at the top right of the main window, beside **Settings**, uses the icon of
  Settings → Panels and turns the panel editor on and off (`aria-pressed`). Outside the editor
  panels cannot be moved: there are no handles or drop zones, only the panels' own buttons and
  the resize dividers.
- In the editor a slim bar under the title bar says what to do and has **+ Column** (an empty
  column at the right) and **Done**; Esc leaves the editor too (when no dialog is open). Panel
  bodies are dimmed and inert, and each header becomes a handle with a grip, the panel's buttons
  hidden. The session panel shows its header there as well.
- Drag a panel by its header (it starts after a few pixels): a ghost with its title follows the
  pointer, and an accent line shows where it lands — above another panel, at the end of a
  column, or at a column's left or right edge, which makes a new column there with that panel.
  Esc during a drag cancels it. An empty column reads *Drop a panel here* and offers
  **Remove column**; panels it still lists (closed, detached, or a mod not loaded) move to the
  column beside it. Outside the editor empty columns are not drawn.
- The session spans its column when it is alone there; stacked with other panels it does not
  fold. Columns left of the session's are drawn as left rails, the others as right rails.
- The arrangement (each column's id and its panels' ids, in order) is saved as `arrangement` in
  the local `layout` document with the sizes and closed panels ([persistence](#settings-and-persistence-implemented));
  until the editor changes it, it is `null` and the default applies. Panels the saved
  arrangement does not list (new built-ins, newly enabled mods) join their default column.
  Column widths are kept per set of columns, and a new set gives the session's column the most
  room. Settings → *Reset layout* restores the default arrangement. The rules are pure
  functions (`layout/arrangement.ts`) with tests (`test/arrangement.test.ts`).

### Moving panels and modules (implemented)

The [panel editor](#panel-editor-implemented) moves panels between and into columns and locks the
layout outside it. Roadmap item 35 adds, on the same pure rules (`layout/placement.ts`,
`layout/moves.ts`, `layout/shapes.ts`, `layout/presets.ts`, each with node tests):

- **Out into a window, and back.** In the editor, dragging a panel past the window's edge (the
  pointer is captured, so it keeps reporting there) or onto the *Drop a panel here… to open it in
  its own window* zone under the editor bar opens it in its own window, the same detached window
  as *Open in its own window*. Panels already in windows appear as chips
  in that strip: drag one into a column to dock it there (its window closes), or press it to dock
  it where it was.
- **Modules between panels.** A mod's panel is a *module*: dragged onto the middle of another
  panel it is shown inside that panel as a titled section below the panel's own content, with
  its own ⋯ menu; the host's editor header lists its modules as chips to drag back out. Built-in
  panels host modules but are not modules, and a module never hosts another. The arrangement
  keeps listing a hosted module in its column, so taking it out puts it back where it was; if
  its host is closed or in a window it shows in that column place meanwhile. Placement is saved
  as `modules` (host → module ids) in the local `layout` document. The built-in sections inside
  panels are not yet modules of their own; only panels that mods contribute move this way.
- **Keyboard alternatives.** Every drag has a menu entry in the panel's ⋯ menu, which works with
  the editor on or off: *Move up*, *Move down*, *Move to column N*, *Move to a new column at the
  left / right*, *Move into <panel>* (module panels), and *Open in its own window*; a hosted
  module's *Move to column N* takes it out. Entries list only panels and columns that are on
  screen. A menu choice and the matching drag land the same way, since both go through
  `dropPanel`.
- **Project layouts.** The layout is global until the user chooses *Save as project layout*
  (Settings → Panels, or the editor bar) for the selected environment, which copies what it
  shows; from then on that environment's panel edits (arrangement, closed panels, modules) go to
  its own copy and other environments are untouched. While one is active the title bar shows a
  *Project layout* marker, and *Use global layout* removes it. Overrides are stored in the local
  `layout` document as `overrides`, keyed by environment id; an environment without one, or
  whose id is gone, uses the global layout. Sizes, folded panels and detached windows are about
  the screen and stay global. Settings → *Reset layout* resets the layout that applies. A
  project layout file in the repository (`.imperium/layout.json`) is not built.
- **Layout presets.** Settings → Panels has *Export…* (the layout that applies now, saved as
  `imperium-layout-<name>.json` through the save dialog) and *Import…* (a file picker). A preset
  is data only:
  `{ format: "imperium-layout-preset", version: 1, name, arrangement, hidden, modules, mods }`,
  where `mods` lists the mods whose panels it places. Import is strict and applies nothing on any
  failure, showing the reason: invalid JSON, a file over 64 KiB, another format, a version other
  than 1 (a newer one says to update), fields beyond these, or a malformed arrangement (distinct
  panels, each in one column), closed list, or modules (only a mod's panel can be a module, and
  never in the session). Panel and mod ids are whatever mods choose: only a length limit,
  control characters and prototype-like names are refused. Importing never installs or runs
  anything: panels of a mod that is not installed stay out of sight until it is. A valid file
  first asks *Replace the current layout with <name>?* (naming the layout it replaces and any
  missing mods), and only *Replace* applies it, to the layout that applies (the environment's
  own when it has one). Saved layouts with modules that cannot be hosted lose just those
  entries.
- Mods contribute new panels, modules, and slot content through [UI mods](06-extensibility.md#ui-mods).
- **Agent tree** shows every agent with its model, status, tokens, cost, and budget; selecting
  one opens its transcript.
- **Command palette** reaches every action; all actions are keyboard-bindable.

## Scaling and wide displays

Three independent controls, each per window and per monitor:

| Control         | Affects                                         | Range           |
| --------------- | ----------------------------------------------- | --------------- |
| UI scale        | Everything (zoom)                               | 50–300%         |
| Font scale      | Text only, separately for prose and code        | 50–300%         |
| Density         | Padding, row heights, icon sizes                | Compact / Comfortable / Spacious |

- **Readable measure:** prose is capped at a configurable line length (default ~90
  characters) and centred within its pane; code blocks and tables may use the full width.
- **Ultrawide presets:** three- and four-column layouts (for example sessions · conversation ·
  agent tree · telemetry) that auto-apply above a configurable aspect ratio.
- Layout uses CSS container queries, so panes adapt to their own width, not the window's.
- Per-monitor DPI changes are handled live when windows move between displays.

### Narrow windows and column minimums (implemented)

Every column has a minimum width, in layout pixels (window pixels over the UI scale, so a column
keeps its size on screen at every scale): the sidebar (the columns left of the session's) 220, the
session 360, and the side columns (right of it) 280. They are `--col-min-sidebar`, `--col-min-session`
and `--col-min-side` in `styles/shell.css` and `COLUMN_MIN` in `layout/fold.ts`; a test fails when
the two drift apart. A column never draws narrower than its minimum, whatever proportion was saved.

- **Folding.** When the window cannot fit every column at its minimum, the rightmost side
  columns fold away until the rest fit (a width equal to the total fits). The session's column never
  folds. A folded column keeps its share of the saved proportions, so widening the window brings it back as it was; nothing
  about folding is written to the layout document. A folded column stays mounted, hidden, so its panels keep their state (a
  browser tab, scroll, typed text, a mod's frame) across a resize. The title bar's *More panels* menu lists the panels of
  the folded columns; choosing one opens it in a sheet from the right, 340 px wide over a dimmed workspace. The sheet is the
  folded column's own cell moved to the window's edge (`layout/Sheet.tsx`, `.sheet-cell` in `styles/shell.css`), so the panel is the
  same instance, not a copy.
- **Dragging a divider** never takes a column below its minimum: the drag stops there, and dragging back answers at once
  (`dragShares` in `layout/fold.ts`).
- **Below 600 px** the window shows one column at a time. The session fills the window; the
  sidebar's panels (the first column left of the session: Environments, Sessions, Approvals) open from a button at the left of the title bar
  as a sheet from the left that leaves a strip of scrim to tap, and the other panels open from *More
  panels* as a sheet that fills the window. The title bar drops the connection text and the project-layout badge, and the
  sidebar button carries a dot while approvals are waiting. A sheet closes on Esc, a click on the scrim or its close button (and
  the sidebar sheet when a session is chosen; choosing an environment only filters the sessions beside it). The sheet is not
  modal: every other column is inert while it is open, so focus cannot reach the dimmed workspace, but the title bar stays in reach
  (the button that opened the sheet closes it). Focus moves in when it opens and goes back to the button that opened it; when
  that button is gone (the window widened past 600 px with the sheet open, which closes it) focus goes to the app root. A menu or dialog opened from inside
  the sheet takes Esc first.
- **The panel editor is off below 600 px** (its button is disabled with a tooltip, and a window
  narrowed while editing leaves the editor). In the editor, wider than that, no column folds and the
  minimums are not applied, so every column can be a drop target.
- The rule is the pure function `foldColumns` (`layout/fold.ts`, tested in `test/fold.test.ts`);
  `App.tsx` measures the window (`layout/useWidth.ts`; a reading of 0, a minimised window, keeps the last width) and `layout/Columns.tsx` draws what it returns.

## Theming

Design tokens (colour, type, spacing, radius, motion) are CSS custom properties. Themes are
token sets; light, dark, and high-contrast ship by default. Fonts are user-selectable,
including ligatures for code.

**Icons (implemented):** every icon is a [Lucide](https://lucide.dev) line icon from
`lucide-react` (pinned in `apps/desktop/package.json`), the same thin, rounded style as
ChatGPT's and Claude's apps. One stroke width is set for all of them in `main.tsx`
(`LucideProvider`), and their size follows the text around them (`.lucide` in `styles/primitives.css`),
so they scale with the interface. Icons beside a label are hidden from screen readers; a
button with only an icon (`.icon-only`, such as a panel's close button) carries an
`aria-label` and a tooltip. Project glyphs, the send button, tool-call states (spinner, check,
cross), panel and title-bar actions, Settings sections, and daemon and update controls all use
them; there are no text symbols or hand-drawn SVGs left for controls.

**Selects (implemented):** `components/Select.tsx` replaces native `<select>` elements, which
cannot be themed: a raised button showing the choice and a floating list (rendered above
dialogs so nothing clips it) with a check by the selected option and optional descriptions.
It follows the WAI-ARIA select-only combobox pattern: arrow keys, Home/End, and Page Up/Down
move; typing jumps to a matching option (also on the closed control, like a native select);
Enter or Space chooses; Escape or a click outside closes without closing the dialog around
it. Settings, the context breakdown's agent picker, and the Hugging Face quantisation picker
use it.

**Theme (implemented):** Settings → Appearance → Theme picks System (the default, following the
operating system and its changes), Light, or Dark. `settings/settings.ts` sets `data-theme` on the root
element and `styles/tokens.css` keys the light tokens on `:root[data-theme="light"]`; the command
palette's *Switch to light/dark theme* flips it. Mod panels pick up the tokens when they open.

**Mod frames (implemented):** a mod panel's sandboxed frame is built with the tokens and the theme
the app has at that moment (`mods/ModFrame.tsx`): `color-scheme` is `dark` or `light`, the app's
resolved theme (never `light dark`), so a mod's native controls (selects, checkboxes, scrollbars)
match the app instead of the operating system, and the frame body's padding is `--space`. A frame
opened before a theme switch keeps its old scheme and tokens until it is reopened. A frame's requests to the daemon (pets) made before the app has connected wait for the connection (up to 30 s) instead of failing. The contract is
unchanged: the same tokens (`THEME_TOKENS`) and bridge messages. The bundled Imperium Nursery
fills its frame with one scroll owner (its own panel; the frame document and the room do not
scroll), wraps its controls bar into one column under 220 px, styles its selects with the
tokens, and draws pets smaller only when their whole boxes (sprite plus label) would not fit the
room (labels keep at least 72 px while they can, and sprites never go below 30 %); the room is
made taller for two to eight pets so that they, and a spare box to walk into, fit at 70 % (50 %
from five pets), so a fifth pet does not halve the others; the panel scrolls instead. When the room
has space at that minimum size, new and wandering pets are placed where their boxes stay clear of
the others; a very crowded room cannot guarantee it, and pets that walk past each other may
overlap briefly.

**Tokens and accents (implemented):** every length derives from `--px` (one CSS pixel at 100 % UI
scale), spacing runs on 4 px steps (`--sp-1` to `--sp-8`), the control-height tokens provide a 24 px floor at any density (surfaces adopt them in their
stages), and there are six type sizes, three weights, five radii, three elevations and
the motion and layer tokens, all in `styles/tokens.css`; today's names (`--panel`, `--fg`,
`--muted`, `--fast`, `--slow`, `--ease`, `--space`, `--radius`) remain as aliases, and the mod
token names keep their meaning. Surfaces are flat (`--bg`, `--surface`, `--surface-raised`,
`--surface-sunken`); only floating things have a shadow, and the title-bar wordmark is the one
gradient (`--wordmark`). Input, select and switch edges use `--border-control` (3:1 against every
surface), and focus is one 2 px `--focus` outline. Each of the twelve accents has a dark and a light
value (fill, hover, wash, ink, accent text, focus) under `:root[data-accent="…"]`; `settings.ts`
sets `data-accent` rather than writing colours inline, so the light theme's values win.
`test/tokenContrast.test.ts` checks the contrast of text, accent text, ink, focus and control edges
for both themes and all accents. The UI face is Inter Variable, bundled with
`@fontsource-variable/inter`.

### Loading screen and no white flash (implemented)

Between launching the app and the first panel render (reading the saved settings and layout
takes a moment) nothing shows white:

- **Loading screen:** `index.html` carries a themed screen with the Imperium mark and a
  spinner, styled by the linked `public/boot.css` (never an inline `<style>`: Tauri would add a
  style nonce to the release CSP and turn off `'unsafe-inline'` for every other inline style). `aria-busy` is set on `<body>` and the
  screen is `role="status"`. `src/shared/boot.ts` fades it out (180 ms) once `App` has mounted and the
  panels have rendered, not merely when the script loads; an 8 s fallback removes it regardless,
  so it can never stick. With reduced motion the spinner is static and the fade is instant.
- **Theme before the daemon (or settings) is read:** `settings/settings.ts` remembers the chosen theme in
  `localStorage` (`imperium.theme`); the linked `public/boot.js` sets `data-theme` from it
  before first paint. When the webview has nothing remembered (first launch after an upgrade,
  cleared data), `main.rs` seeds that key from the saved `settings.json` with an initialization
  script. With "System", `prefers-color-scheme` decides. The loading screen uses the theme's
  accent (`#1fb2c8` dark, `#0a7186` light).
- **Native window:** the configuration gives the main window the dark `--bg` as its
  `backgroundColor`. `main.rs` creates it hidden, reads the saved theme from `settings.json`
  (the window's system theme under "System"), sets the matching native and webview background,
  then shows it, so the operating system's white never appears. Detached panel windows pass the
  current theme's background when created and show the same loading screen, since they load the
  same page. Settings, Connectors, and environments are dialogs inside the main window; mod
  panels are frames that inherit the theme tokens.

### Quiet interface (implemented)

Panels and windows show their content and at most one primary action; everything else is one
click away, never removed.

- **Panel headers:** every header is one height (`--panel-header-h`, a control plus 8 px, following
  density), with the fold chevron, the title (13 px semibold), the primary action, and one ⋯ menu
  (`Menu`, the same menu button session rows use) that stays invisible until the header is
  hovered or holds focus (always shown on touch screens). The menu lists the panel's own
  secondary actions (`PanelSpec.menu`) first, then *Collapse all / Expand all in this column* on
  a stack's first panel, the *Move…* entries (the keyboard alternatives to dragging, see
  [moving panels and modules](#moving-panels-and-modules-implemented)), *Open in its own window*,
  and *Close panel* (`layout/panelMenu.ts`).
  Header actions are small icon-only buttons with an `aria-label` and tooltip: *+* for New
  environment and New session (beside the Sessions sort), the sliders for *Configure models*.
- **Title bar:** a flat strip with the wordmark (the one place the chrome gradient stays), the
  connection state and the version; Connectors, the panel editor toggle and Settings are one group of
  `IconButton`s, each with a tooltip that also shows on keyboard focus. A project layout is marked
  with an accent `Badge`.
- **Launch focus:** once the panels have rendered, the app asks for document focus and moves it to
  the app's root element (`useLaunchFocus` in `shared/useWindows.ts`) unless a field already has it,
  so Ctrl+, and Ctrl+K do not need a click first. Only the web view's own focus is requested; if
  the web view lacks operating-system focus the shell asks for it: the Tauri shell (`src-tauri/src/main.rs`)
  calls `set_focus` once on each window it builds, right after `show`, so the OS gives the window
  (and its web view) keyboard focus at launch. It is a single call at creation, not repeated on
  events, and the login-time background run builds no window, so it never takes focus.
- **Rows:** a row's secondary actions show on hover or focus: a model row's favourite and hide
  toggles (shown while set), a queued message's *Edit* and *Remove* (*Steer* stays visible), an
  environment folder's remove. Connector rows keep their on/off switch and put *Test
  connection*, *Configure*, and *Remove* in a ⋯ menu.
- **Decisions:** a permission request shows *Allow* and *Deny*; *Always this session* and
  *Always allow <rule>* are in its ⋯ menu. A paused subagent's view shows *Resume* with *Cancel*
  in a ⋯ menu. The environment window's delete is in its Danger zone section, and
  Settings → Notifications plays its three sounds from one *Play* menu.
- **Tokens:** control heights (`--control-h`, `--control-h-sm`), paddings, icon sizes (`--icon`,
  `--icon-sm`) and type steps (`--fs-xs`, `--fs-sm`, `--fs-md`) are shared custom properties in
  `styles/tokens.css`; `.icon-only`, `.small`, `.panel-tool`, and `.row-tool` buttons use them instead
  of one-off sizes.

Every moved action keeps its accessible name: menu buttons follow the WAI-ARIA menu button
pattern (arrows, Home/End, Enter or Space, Escape), and hover-revealed buttons appear when
tabbed to.

## Command palette (implemented)

**Ctrl+K** (Cmd+K on macOS) opens the command palette in the main window and in detached panel
windows; pressing it again closes it. It is the shared `Dialog` (one column, no navigation, near the
top of the window) with one search box over:

- **Actions:** new session, new session in each environment, new environment, Open Settings,
  Open Connectors, Configure models, switch to the light or dark theme, check for updates, and
  for the open session: compact the conversation, archive or unarchive, pin or unpin.
- **Sessions** by title (with the environment's name), and **Archived sessions** in a group of
  their own; choosing one opens it.
- **Environments:** show one (its sessions and new sessions there), or edit it.
- **Models:** switch the open session's model to any usable model-table row or local model
  (`session.control` `set_model`, as the model selector does); favourites first, hidden models
  left out ([favourite and hidden models](#environments-and-sessions-implemented)).
- **Panels:** open a closed panel, close or detach a docked one, show or dock a detached one
  (the Usage and Local models panels among them; mod panels too).
- **Settings:** every section by name (*Settings: Budgets*, *Settings: Keyboard*, …), opened
  directly.

Matching is fuzzy: every typed word must match the title, a subtitle (a session's
environment, a model's details), or a command's keywords, as a substring (best at a word start)
or as letters in order. With nothing typed, the last few commands run lead under **Recent**;
while typing, recently run commands are lifted above equal matches. The last 20 are kept in the
layout document. Arrow keys and Page Up/Down move, Enter runs, Tab and Shift+Tab jump between
groups, Escape (or a click outside, or the close button) closes only the palette (not a window under
it), and the mouse works too. Key hints are `Kbd` chips. Commands with a shortcut
show their current binding (Open Settings: Ctrl+, unless rebound).

**Registry:** `src/commands/registry.ts` holds named command sources; a feature registers one
with `useRegisterCommands(name, build)`, and the palette calls each source when it opens, so
commands always run the feature's current handlers. The app's own commands
(`commands/appCommands.ts`) are built from the handlers `App.tsx` already uses for the same
buttons. Ranking and grouping are pure functions in `commands/rank.ts`, tested in
`test/rank.test.ts`. A panel window's palette lists the same commands but sends the chosen one's
id to the main window (a `command` panel-window event), which runs it, records it as recent, and
comes forward; theme changes leave the panel window in front. The shortcuts are listed, and
rebindable, under Settings → Keyboard ([below](#rebindable-shortcuts-implemented)).

## Rebindable shortcuts (implemented)

Settings → Keyboard is a table of the shortcuts: the action, its binding drawn as one `Kbd` chip per
key, and **Change** and **Reset** after a 24 px gap, followed by the fixed shortcuts. **Change** waits for the next key
combination; **Reset** returns one to its default. The rebindable ones are the command palette
(Ctrl+K), Settings (Ctrl+,), and stop or resume the session (Ctrl+.); on macOS Cmd stands for
Ctrl, and a chord is stored as "Mod+Shift+K" so a binding means the same everywhere. Sending
(Enter, Ctrl+Enter), New line, Esc and the palette's own keys are fixed, and the push-to-talk
key keeps its own setting under Chat.

A binding must hold Ctrl (Cmd) or Alt unless it is a function key, so it cannot fire while
typing; Escape, dead and lock keys, and the editing chords (Ctrl+C, V, X, A, Z, Y, Backspace)
cannot be bound. Bindings follow the keyboard layout (they match the character the key types,
not its position), so one made on a layout may sit elsewhere on another. AltGr (Ctrl+Alt on
many Windows layouts) is never read as a shortcut, and Ctrl+Alt with a typed character is
refused, so characters like @ and € keep typing (pressing AltGr while binding says so). Choosing a
push-to-talk key that a shortcut already uses is refused, with a note naming the shortcut. While Change waits for keys, no other shortcut
runs; Esc, leaving the button or switching windows cancels. A combination another action holds, or a fixed use (sending, the
push-to-talk key), is refused with the holder named, so no two shortcuts ever share a key. Only
the bindings the user changed are saved (`shortcuts` in the app settings document), so a later
version's new default reaches everyone who has not rebound that action; on load, saved bindings
for actions that no longer exist, that do not parse, or that now clash are dropped back to the
defaults. The palette shows each command's current binding, and the Settings button's tooltip
follows it.

**Registry:** `src/settings/shortcuts.ts` declares the actions (id, label, description, default) and holds
the pure logic (parsing, matching an event, conflict detection, sanitising what was saved);
`useSettings().shortcuts` is read by the handlers in `App.tsx`, `commands/CommandPalette.tsx` and
`panels/SessionPanel.tsx`. A command that has a shortcut names its action (`shortcutAction`) rather
than a fixed key. Tested in `test/shortcuts.test.ts`. Adding a rebindable shortcut is a new entry in
the registry and a handler that asks `triggers(event, settings.shortcuts, id)`.

## New-user tour (implemented)

For a new user, a guided tour walks through the main areas: environments
and Home, the sessions list, the composer, panels and layouts, the command palette, permissions
and approvals, memory, and Settings. Each step is a small card beside a highlighted element
(or centred, for a step about the app as a whole) with Back, Next, and Skip tour. The card is the shared
`.card` floating at `--elev-2`, with a row of step dots (the current step in the accent) above its
title at `--fs-lg`; the highlight is a `--radius` ring in `--focus` and the dimming uses `--scrim`.

- **For new users, once.** How the tour ended (`finished` or `skipped`, and when) is the app
  setting `tour` in the local settings document, so it starts by itself only once. It starts in the
  main window only, once imperium-core is connected and the composer is showing (after eight
  seconds, any panel will do), and never while a dialog or a pending approval is on screen. An install
  whose settings document predates the tour is marked `existing` when loaded, so people updating
  are not shown it (they can still start it). Resetting app settings keeps the setting.
- **Restartable.** *Take the tour* is a command in the palette (keywords: tutorial, help,
  onboarding) and a button in Settings → About; both close the windows over the workspace and start
  from the first step.
- **Accessible.** The card is a modal dialog named and described by its title and text; focus moves
  to it on each step and stays inside it (Tab and Shift+Tab wrap), Right Arrow (or Enter on the card)
  goes on, Left Arrow goes back, Esc skips, and each step after the first is announced, with its
  text, in a polite live region. Focus returns to where
  it was when the tour closes. It uses no animation.
- **No network and no library.** The tour is `src/tour/Tour.tsx` (the card and highlight, on the
  app's theme tokens) over `src/tour/tourSteps.ts` (the steps and pure logic); it adds no dependency.
- **Declarative steps.** `TOUR_STEPS` in `tour/tourSteps.ts` is a list of `{ id, title, body, target? }`,
  where `target` is a CSS selector (panels have `data-panel`, and the title bar's Edit layout and
  Settings buttons `data-tour`). A step whose target is not on screen (no session open for the
  composer, a closed or detached panel) is skipped, in both directions, and the step counter counts
  only the steps that can be shown. Adding a step is adding an entry. The highlight follows its element as the window or any
  container scrolls or the layout changes, and a step whose element goes away is dropped.
- **Never in the way.** While another dialog is open (the palette, Settings, a confirmation) the
  tour hides and leaves keys to that dialog, then returns. If an approval arrives, the tour
  that started by itself hides the same way until none is waiting, then carries on from the same
  step, so the approval is never covered and a first-run user does not lose the tour. A tour the
  user asked for (the palette, Settings → About) is not hidden by an approval.

Sequencing, skipping missing targets, the persisted outcome, and card placement are pure functions
tested in `test/tourSteps.test.ts`.

## Accessibility

Full keyboard navigation, visible focus, screen-reader labels, reduced-motion support, and
WCAG AA contrast for shipped themes.

## Connectors (implemented)

**Connectors** (the plug icon in the title bar, or Settings → Connectors) opens a window like Claude's and
ChatGPT's connector directories. It is the shared `Dialog` (one column, a header, the scrolling body; below
600 px wide it is the full-screen sheet, with no rule of its own for narrow windows), with `Tabs` (*Installed*
with its count, and *Browse*) and a search box between the header and the body. Escape steps back from a form,
then closes the window. Failures show as a `Notice`; empty and loading states are the shared `EmptyState` and
`LoadingState`.

- **Installed:** rows one `--row-h` tall (the sidebar's row height) with the detail lines under the name.
  MCP servers, each with its icon, command or URL, and status badge (*Not tested*,
  *Connected · N tools* with the tool names, *Error* with the reason, or *Off*), and buttons
  to test the connection, configure, remove (optionally deleting its saved secrets), and a
  switch to turn it off. Missing keychain secrets are flagged. Below them, plugins (skills,
  commands, hooks, and servers they bring, with an on/off switch) and skills; one an agent
  installed says where it came from (the URL and commit, or the folder).
- **Browse:** an even grid of `.card.interactive` cards (the whole card opens *Add*), filtered by category
  chips and search, each card with an icon, publisher, description, category, local or remote, and *Add* (or
  *Added*). *Add* opens a form for the connector's settings: a name, then its fields (folders,
  paths, URLs, tokens). Secret fields are password inputs saved to the system keychain and
  never shown again. A *Custom connector* card adds any server by command (arguments,
  environment, secret variables) or URL (bearer token, headers), with the tools that run
  without asking and a timeout; *Configure* opens the same form for an installed server, where
  saved secrets are kept unless replaced. Forms use `Field`: a hint under each control and a problem (a
  bad name, a missing required value) under the field it belongs to, with focus moved to the first one.

Adding or saving a connector tests it straight away. The window talks to the daemon's `mcp.*`
methods and the directory comes from the daemon; see
[managing MCP servers from the app](05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented).

## Settings and persistence (implemented)

**Settings** (title bar, or Ctrl+,) opens a window with sections on the left and search across
every setting:

- **Imperium:** General (reopen where you left off, start imperium-core with the app), Appearance
  (theme, accent colour, interface, message, and code scale, line length, density, message font,
  reduce motion), Chat (send key, Markdown, code wrapping, following new output, the
  [push-to-talk](#attachments-and-voice-input-implemented) key),
  Notifications (sounds, a title badge, and system notifications; see
  [notifications](#notifications-implemented)), Panels (show or
  hide each panel, reset the layout), Keyboard (the shortcuts, rebindable; see [rebindable shortcuts](#rebindable-shortcuts-implemented)), Daemon (start, stop, restart, log, keep
  running), Updates (installed version with **Check for updates** and **Check and install**,
  channel, how often to check, and checking when the computer starts; see
  [desktop updates](11-ci-cd.md#desktop-updates); choosing the other channel and checking
  offers that channel's latest build even when its version number is lower: the section says
  which build it would install, notes when the number is older, and installs only after
  **Install** is pressed, and a channel with no release says so), and Connectors (opens the Connectors
  window, with the MCP and plugin settings).
- **Accent colours:** twelve, shown as a row of swatches in hue order (cyan, teal, green, lime,
  amber, orange, rose, magenta, violet, indigo, blue, silver); the chosen one is ringed.
- **Layout:** every section uses the same rows, and every row starts on one left edge, the
  dialog's gutter (`--dialog-gutter`, the same inset as the title, notices and footer): the
  setting's title and description, then its control under them, flush left (text fields, selects
  and text areas 320 px wide, numbers 144 px, swatches and switches at their own size). Sections
  are separated by space, not lines (a budget in Budgets is set off by a hairline above it, with no box). A setting that depends on the one above it (such as checking
  at startup under the update schedule) sits closer to it on the same edge; status lines and logs
  under a setting use the same edge. Tables (Keyboard, the benchmark) start their first column
  on it. Inline loading, error, empty and warning states in a section use the shared `LoadingState`,
  `Notice` and `EmptyState`, left-aligned on the same edge rather than centred; the benchmark's model picker is a `Select`.
- **Agent and daemon:** every setting the daemon exposes through `config.describe`, grouped by
  section (models, local runtime, memory, workflow, web, media, isolation, GitHub, pets, phone,
  resuming, network). *Personality* is a select of the personalities `personalities.list`
  returns, by name, with the default marked, local ones labelled, and None.
  Changes go to the user config file through `config.set` and apply when the daemon restarts
  (the window offers the restart), except Personality, which applies to new sessions at once.
- **Permissions** (under Agent and daemon): the global path grants and permission rules; see
  [permissions](#permissions-implemented).
- **API** (under Agent and daemon): the OpenAI-compatible local API; see
  [API settings](#api-settings-implemented).
- **Changes by agents** (under Agent and daemon): what agents changed in the app config
  (`config.changes`), newest first: model cards with the card before and after, and mods,
  plugins, and skills installed or removed with their folder and source. Each has **Undo**
  (`config.undo`); undone ones are marked. See
  [agents manage the app config](06-extensibility.md#agents-manage-the-app-config-implemented).
- **Mods** lists each mod's source when an agent installed it; such a mod is off until turned on.
- **Data and storage** shows where everything is kept and resets app settings or the layout.

App settings, the layout (panel sizes, closed panels, the selected environment and session),
and the last-known sessions are files in `desktop/` inside Imperium's config folder, beside the
daemon's `config.toml`, so they persist across updates and reinstalls. The workspace is always
shown: while the daemon is away, a bar above it offers to start or reach it and the lists show
the sessions as last seen.

## Permissions (implemented)

**Settings → Permissions** edits the global [path grants and rules](05-tools-and-isolation.md#permissions)
(`permissions.get` / `permissions.set`, saved to `[permissions]` in the user config), and the
**environment window** has a *Permissions* section with the same editor for that environment
(saved in the environment with *Create* or *Save*). The editor (`PermissionsEditor`) has two
parts, styled like the environment's folder list:

- **Paths outside the folders:** each grant's path with a *Read-only* / *Read and write* select
  and *×*; *Add folder* (the folder picker) or a typed path (`~` allowed) with its mode; and one
  chip per **preset** for common caches and toolchains (Cargo, rustup, npm, pnpm, Yarn, pip, uv,
  Go modules, Maven, Gradle, NuGet). The daemon resolves the presets for its own operating
  system and environment variables (`CARGO_HOME`, `GOMODCACHE`, `%LOCALAPPDATA%`, …), writes
  paths in the home folder with `~`, marks missing ones *(not found)*, and suggests read and
  write for caches and read-only for toolchains. For a remote runtime, presets are hidden and
  paths are typed as paths inside it.
- **Rules:** each rule's action (*Allow*, *Ask*, *Deny*), tool (suggestions such as
  `run_command`, `write_file`, `*`; wildcards like `mcp__github__*` work), and pattern, whose
  placeholder says what it matches for that tool; *Add rule* and *×*.

Settings saves with *Save* (and *Revert* drops the draft). Rules reach running sessions at their
next tool call; paths reach sessions started afterwards.

A permission prompt in a session offers **Always allow** beside *Always this session* when the
daemon suggests a rule (the event's `suggested_rule`: the command and its subcommand, such as
`cargo test`; the file's folder, such as `/home/me/project/src/**`; or the whole tool for other
tools). It adds that allow rule to the session's environment (`permissions.add_rule` with the
session id; global for a session without one) and allows the request.

## Notifications (implemented)

Imperium gets your attention in three ways, each switched in Settings → Notifications:

- **Sounds:** a short chime when an agent needs approval (on by default), and one when a session
  finishes (off by default); a failed turn uses the finish setting with a lower chime.
- **Title badge:** a dot before the window title while something needs you and Imperium is in
  the background.
- **System notifications:** the operating system's own (through `tauri-plugin-notification`,
  or the Web Notification API in the browser preview), for an agent waiting for approval or
  asking a question, a session finishing its turn, a turn failing, a budget or usage alert, a
  model download finishing, and an update ready to install. Each kind has its own switch, all
  on by default. **Only when Imperium is in the background** (on by default) holds them back
  while any Imperium window has focus.

The operating system's do-not-disturb or focus mode applies as usual. Where the system asks
for consent (macOS, browsers), Imperium asks the first time it has something to show;
**Try a notification** shows a sample and says when notifications are blocked.

Answering a notification brings the main window forward on the session it was about. In a
browser that is a click on the notification. The desktop plugin reports no clicks on Windows,
macOS, or Linux, so in the desktop app the next time Imperium comes to the front within ten
minutes of a notification sent while it was in the background (by clicking the notification,
where the system does that, or from the taskbar or dock), it opens the latest such notification's
session.

Notifications come from the main window only, so detached panel windows do not repeat them.
Other parts of the app send their own through one internal entry point, `notify(notice,
settings)` in `apps/desktop/src/shared/notify.ts`, which applies these settings; budget alerts use it
([budgets](#budgets-and-usage-implemented)). Which events notify is decided in `apps/desktop/src/shared/attention.ts` and covered by
`apps/desktop/test/attention.test.ts`.

## API settings (implemented)

**Settings → API** manages the [OpenAI-compatible local API](04-models-and-routing.md#openai-compatible-local-api-implemented)
that lets other tools use the local models (`api.get`, `api.set`, `api.token`; the settings are
saved to `[api.openai_compatible]` in the user config and apply at once):

- an **enable switch** (off by default); a failure to start, such as a port in use, is shown
  under it;
- while it runs, the **base URL** to give tools, with *Copy*, and the **API key**: *Show token*
  fetches the token from the daemon (it is not kept anywhere in the app), with *Copy* and
  *Hide*, and *Rotate* (after a confirmation) replaces it, which ends every tool's access until
  it is given the new one;
- the **port** (chosen for the user the first time the API is enabled, then kept; changing it
  restarts the API);
- **Allow other computers**, off, with a warning that it serves plain HTTP to the network and
  that anyone with the token could then use the models and the hosted ones turned on below;
  while on, an address field sets `bind`. Turning it off puts `bind` back on `127.0.0.1`;
- **Models served**: the local models, by alias and spec;
- **Hosted models**: one switch per hosted row of the model table, all off, since each call
  spends the user's key and money; the list is `api.openai_compatible.expose`;
- a note that the API's calls count toward budgets and that a budget with the scope *API calls*
  (Settings → Budgets) caps them alone.

## Local model benchmark (implemented)

**Settings → Models** ends with a **Local model benchmark** section
([04](04-models-and-routing.md#local-model-benchmark-implemented); `models.bench`,
`models.bench_results`, `models.bench_cancel`, followed live through `models.bench_progress`):

- a model list (*All local models*, or one model by alias and id) with **Run benchmark**, which
  becomes **Cancel** while a run is in progress; nothing runs until it is pressed;
- *Keep a loaded model loaded*, which skips the idle unload that lets the load time be measured;
- while it runs, one status line: the model, what it is doing (*loading the model*,
  *waiting while the machine is saturated*, *prompt 2 of 3*), and how many models are done; a model that failed is
  listed with the reason under it;
- a table of the **latest result of each model on this machine**: load time, first token,
  prompt speed, generation speed, peak RAM and VRAM (a dash where the runtime did not report
  it) and the date, headed by the machine's description; results from other machines or older
  runs are in a collapsed *Earlier results and other machines* list.

The section needs the daemon; it follows runs started from the CLI or another window.

## Budgets and usage (implemented)

**Settings → Budgets** edits the daemon's [budgets](10-configuration.md#budgets)
(`budgets.get` / `budgets.set`, saved to `[budgets]` in the user config): when weekly budgets
start, the day monthly ones reset, the default alert thresholds, and one card per budget with an
optional name, its scope (*All usage*, an environment picked from the list, an endpoint, or a
model, the last two with suggestions from the model table, or *API calls*, the calls other tools
make through the [local API](#api-settings-implemented)), its window (daily, weekly, monthly),
a token limit, a cost limit in US dollars (either may be empty), what happens when it is used up
(*Warn only*, *Ask first*, *Block*), and its own alert thresholds (empty: the defaults). Each
saved budget shows where it stands in its current window. *Save* applies the budgets at once.

The **Usage** panel (in Settings → Panels, closable and detachable like the others) is the
dashboard: every budget's current window as a bar with its share, tokens and cost against the
limits, when it resets, and its action; a bar chart of the last 30 days, by tokens or cost, for
all usage or, after clicking a budget, that budget's scope; and the top five environments,
endpoints, and models over the same days. It follows `budgets.changed`, which the daemon sends
after each model call. It uses the same loading, notice and empty states as the Resources
panel; below 22 rem each budget's name sits over its bar, and the *Tokens* / *Cost* chips wrap.

A **budget alert** (`budgets.alert`, once per threshold per window) becomes a *budget* notice
through `notify`, so it follows *Notify about budgets and usage* and the other notification
settings, and answering it opens the session whose call crossed the threshold. Beside the context
meter in the session footer, a small gauge mark shows when a budget over the session (all usage,
its environment, or its model's endpoint or model) is past an alert threshold: amber with the
share, red once used up; its tooltip lists each such budget, its usage, when it resets, and
whether new turns ask or are blocked. Sending a message while a budget that asks is used up opens
a confirmation; *OK* sends it anyway (`confirm_budget`). A blocked turn says which budget blocks
it and when it resets.

## Environments and sessions (implemented)

**Environment configuration and narrowed sessions (implemented).** The environment window's
*Configuration file* section reports what the environment's own `.imperium/config.toml` changes
(`environments.config`): the files read, whether the folder is in `mcp.trusted_projects`, what
was applied, and what was refused. The new-session panel has an *Only this subfolder* field and a
*Read-only* switch for an environment with a folder (both turn *Start in a worktree* off); a
read-only session is marked `[read-only]` in `imperium session list`
([14](14-projects-and-automation.md#narrowed-sessions-implemented)).

The Environments panel lists the daemon's
[environments](14-projects-and-automation.md#environments-implemented), one row each: the
environment's icon and name, a pin mark when pinned, and a *⋯* menu that takes the pin mark's place on
hover or focus, with *Edit*,
*Pin*/*Unpin*, and *Delete*. The built-in [Home](14-projects-and-automation.md#home-implemented)
environment (a house icon) is always listed first, then pinned environments, and has no *Delete*
(its environment window has no delete button either). Clicking a row selects the
environment (new sessions start there) and shows its sessions; double-clicking opens it for
editing. Environment and session rows are `--row-h` tall at every density, the selected one on the
accent wash, with the project tile or the session's status dot centred in one 20 px column so titles
share a left edge; a session row's time gives way to its *⋯* menu the same way. The new-session composer starts on Home when no other environment is selected, and
allows it without a folder: its sessions work in their own scratch folders.

*New environment* (the *+* in the Environments header) and *Edit* open the **environment window**,
built on the same shared modal as Settings, Connectors and the model editor (`Dialog`: a
navigation column on the left, a header with the selected section's title and a close button, the
scrolling rows, and a footer; Escape or a click outside closes it, Ctrl+S saves). Its sections use
the shared `Field` (label above the control, hint and error under it) and, for switches and
buttons, the Settings `Row`, all on the dialog's one left edge (`--dialog-gutter`, the same x as the
title) with `--sp-4` between them; text fields and selects are at most 360 px wide
(`--environment-control-w`). An error from a save or a folder change is a `danger` `Notice` between
the header and the rows, and *Discard changes?* a `warn` one. Folder and agent lists are rows at
least `--row-h` tall (`.environment-list`), with a `Badge` for *working directory*, limits and
initiative and `IconButton`s for the row tools; styles are in `styles/environments.css` with its
style budgets at zero. Below the narrow breakpoint the dialog is the shared full-screen sheet and
the navigation stacks above the content like the other dialogs:

- **General:** the name; the icon, picked from a searchable grid of every icon the app uses
  (`shared/icons.ts`, the shared registry — Settings sections, Connectors, the titlebar, and project
  glyphs; older saved names still resolve); the tile colour, from a row of rounded-square swatches;
  and the default *Pet* for the nursery.
- **Folders & runtime:** the folders, the first marked as the working directory, with *Add folder*
  (the native folder picker) and *×* to remove one; then the runtime, its *Test* and the *Project
  worktrees* switch ([Runtime and agents](#environments-and-sessions-implemented)).
- **Agents:** a compact list, one row per agent (name, role, limits, initiative, *not running*) with
  *Edit* and *Delete*; *Add agent* and *Edit* open an edit view that replaces the list (name, role,
  permissions, and for an existing agent its initiative, which applies at once) with *Back to
  agents*; a row shows an *N suggestions* badge while its initiative has suggestions waiting. The
  Agents pane stays mounted when another section is selected, so an open form is kept. While the
  form has unsaved changes the footer *Save* is disabled and Ctrl+S saves the agent; an open
  form with no changes leaves Ctrl+S to the environment, which it saves.
- **Memory & working folder** and **Configuration file** (existing environments only): see
  below.
- **Permissions:** the environment's path grants and rules ([permissions](#permissions-implemented)).
- **Danger zone** (existing environments other than Home): a `danger` `Notice` with one destructive
  button, *Delete…*, which becomes *Delete* and *Keep* after one more question.

A new environment is created with all of it at once (it has no working folder, configuration file,
agents to edit or delete yet); for an existing one, folder changes and agents apply at once and the
rest with *Save*. Closing with unsaved edits (Esc, outside click, ×, Cancel) shows a *Discard changes?* notice;
*Discard* (or clicking × or outside again) drops them, while Esc then only dismisses the notice
(a held Esc is ignored), so a double Esc never loses work. A runtime that cannot be saved (a missing image or host)
jumps to *Folders & runtime* with the error. Home has a fixed runtime and no Danger zone. *Delete environment* asks once more
(folders on disk are untouched); *Delete* in a row's menu opens the window at that question, in the
Danger zone. Icons and colours are stored with the
environment in the daemon. The first time the app connects after environments arrived, every
project folder it knew (opened folders, the selected project, folders of running sessions)
becomes an environment of its own, keeping the icon it had.

**Working folder (implemented):** an existing environment's window has a *Memory & working folder* section showing where its [working folder](14-projects-and-automation.md#environment-working-folders-implemented) is (from `workspaces.where`) with *Open Memory.md* and *Open folder* (`workspaces.open`, which hands the file or folder to the computer's own editor or file manager and creates an empty `Memory.md` first if needed). Settings → *Working folders* holds `workspaces.root` as a path with *Choose folder…*; saving it moves the folders, shows the move's progress (`workspaces.progress`), and keeps the old value with the daemon's reason when the move is refused or fails.

**Runtime and agents (implemented):** the environment window (create and edit) has a
*Runtime* group of rows in its *Folders & runtime* section (`EnvironmentRuntimeFields`): a select for *This machine*, *Docker*, *WSL*,
or *SSH* with that runtime's fields (the image and an optional running container, the
distribution, or the host), *Test*, which calls `environments.test_runtime` for the saved
runtime and shows its answer in green or red (for example *Docker is running but image rust:1
is not present; get it with: docker pull rust:1*), and the *Project worktrees* switch (on by default, disabled for a remote runtime), which opens the
folders' [git worktrees](14-projects-and-automation.md#worktrees) to its sessions. They apply
with *Create* or *Save*. For a remote runtime the folders are paths inside it
([runtimes](14-projects-and-automation.md#runtimes-implemented)), typed in with *Add path*
instead of the folder picker.

Each existing agent's edit view (Agents section) has an *Initiative* select (*Off*, *Suggest*, *Act*; off for a new agent) and a
*Review now* button (`AgentInitiative`). With initiative on, its row shows where it stands (reviewing,
waiting and why, the last review, the `act` allowance), the **suggestions** it made with their
reason, source and prompt and **Start** (opens the new session) and **Dismiss**, the handled ones,
and its latest *Work left* reminder, following `initiative.changed`. A session an agent started
carries a note under its header (*Started by Planner in Work on its own initiative: …*) and in its
tooltip in the list, and `initiative.notice` raises an OS notification (Settings → Notifications →
*Notify about agents' suggestions and sessions*; clicking one for a started session opens it).

The window's *Agents* section (`EnvironmentAgents`) lists the environment's
[persistent agents](14-projects-and-automation.md#persistent-agents-implemented): a new
environment has none, *Add agent* asks for a name and an optional role, and each row has edit
(rename, change the role or the [permissions](14-projects-and-automation.md#agent-permission-profiles-implemented):
a *Local files only* checkbox and an *All / None / Only these* choice, with names, for MCP
servers and for plugins) and delete (with a confirmation: the agent's history is archived). A
limited agent's row shows what it is limited to. Each action applies at once; the list follows
`environments.changed`.

**New session (implemented):** *New session* (the *+* in the Sessions panel's header; disabled while
disconnected) opens the new-session composer in the middle panel, which is also what the middle
panel shows whenever no session is open. It is a message box for the first message with the
session's options under it, in the same footer row an open session uses: the *Environment*
(the selected one, else the first, pinned ones leading), the *Model* (the model table and local
models, as in the [session footer](#environments-and-sessions-implemented); it starts on
`models.default`, read with `config.describe`, and offers *No model* when none is configured),
and *Start in a worktree*, shown only for a local environment with a folder inside a git
repository (`environments.list` and `environments.changed` name those in `git`). Sending (Enter,
or Ctrl+Enter, as set in Settings) creates the session with `session.create` (`environment_id`,
`model`, `worktree`) and sends the message to it; the send button alone starts it without one.
An environment with no folders, or none at all, disables sending with the reason beside the
options, and a refused `session.create` shows the daemon's explanation above the box. A
session started without a model keeps its message queued until one is picked
([worktrees](14-projects-and-automation.md#worktrees)).

The Sessions panel lists every session across environments, one line each as in Claude's
app: a status dot (working pulses, needs attention is red, paused or waiting for a model is a
ring, watching and merged are green), the title cut short with an ellipsis, and how long ago it
was active (`5m`, `3h`, `2d`) on the right, which gives way to the *⋯* menu on hover. Everything
else (status in words, environment, model, cost, *Persistent*, worktrees) is in the row's
tooltip. The menu has *Rename*, *Pin*/*Unpin*, *Archive*, and *Show* its environment.

**Session titles (implemented):** after a session's first exchange the core gives it a title of
a few words, written in the background by a model that is cheap to ask at that moment (the
session's own model when it is local and loaded with a free slot, another loaded local model,
or the session's hosted model). It never loads a model for a title and the turn never waits;
without such a model, and for background sessions and project agents, the title is the first
message's first words. Double-clicking a title (or *Rename* in its menu) edits it in place:
Enter or leaving the field keeps it, Escape cancels, and an empty title hands it back to
automatic titling. A title the user gives is kept by the daemon (`session.control` `rename`)
across restarts, archived sessions included, and no generated title replaces it; changes are
announced as `session.updated`.

Archived sessions (archived by hand, or on their own once
their pull request closes or after the inactivity set in Settings → Sessions; see
[archiving sessions](14-projects-and-automation.md#archiving-sessions)) leave the list and are
kept in a collapsed *Archived* group pinned to the bottom of the panel, apart from active
sessions, most recently archived first, with *Unarchive* in their menu. Opening one shows its
history with a note that it is archived and an *Unarchive* button in place of the composer. A sort
control in the panel header, beside *New session*, orders it *Recent* (the default: most
recently active first), *By environment* (grouped under environment headings, pinned
environments first), or *By status* (groups *Waiting for approval*, *Active* — working or
watching —, *Paused*, *Idle*, *Merged*); pinned sessions stay on top within each group, and the
choice is kept in the local layout. Pins on sessions and environments are kept by the daemon
(`session.control` `set_pinned`, `environments.update` `pinned`), so every client agrees and
they survive restarts; the daemon announces a changed session as `session.updated`. Clicking an
environment (or *Show* in a session's menu) opens that environment's view, which lists only its
sessions, with *All sessions* to return; clicking the row opens the session and selects its
environment. An environment's persistent agents are listed above everything else in its group,
titled *<environment> · <agent name>* (not renamed) and marked persistent in their tooltip; a session on
worktrees has a tooltip line for each worktree and branch. Deleting a session is `session.delete` (`imperium session delete`), and a session
deleted elsewhere (`session.deleted`) leaves the list. The session's top bar shows only its
icon, title, and status badge (with *Resume now* while paused); everything else about the
session sits in the footer under the composer.

**Session footer (implemented):** a compact row under the composer, as in chat apps: the model
selector at the left (with the effort selector beside it for a model that can reason, then the
permission mode), then the session's cost once it has one, an options button, and the
context meter at the right. The options button opens *Display* (*Render Markdown*, and the
project and isolation tier) and *Usage limits* (how to wait when usage runs out:
`session.control` `set_resume_policy`) above the composer; the context meter opens the
[context breakdown](#environments-and-sessions-implemented), session totals, and recent outputs
there. Only one of them is open at a time; each closes with its button again or the panel's
close button.

**Effort selector (implemented):** beside the model, a compact list of the session's
[reasoning effort](04-models-and-routing.md#reasoning-effort) (*Low*, *Medium*, *High*, *Max
effort*). While the session's model cannot reason (the session summary's `reasoning`, updated when
the model is switched) the selector stays but is greyed out and disabled, with the tooltip *This
model doesn't reason*, and no effort is sent. Choosing one sends `session.control` `set_effort`, which
applies from the agents' next model call, during a turn too, and is kept across restarts; a
refusal shows its error beside the list. New sessions start at `models.effort`, which the
Settings window lists with the other model settings.

**Permission mode selector (implemented):** after the effort, a list as quiet as the effort
(muted text) of the session's [permission mode](05-tools-and-isolation.md#permission-modes-implemented):
*Plan only*, *Ask every time*, *Ask by rules* (the default), *Auto* (a classifier lets routine
in-scope calls run and asks about the rest; [auto mode](05-tools-and-isolation.md#auto-mode-implemented)),
*Accept edits*, and *Full access*,
each with a one-line description. Choosing one sends `session.control` `set_permission_mode`,
which applies from the agents' next call, during a turn too, and is kept across restarts; a
refusal shows its error beside the list. New sessions start in `permissions.mode`, which the
Settings window lists beside the isolation settings; deny rules and the
[safety floor](05-tools-and-isolation.md#permission-modes-implemented) hold in every mode, and
the floor also holds in unattended sessions (auto-fix, schedules, project agents), whose
floored calls wait in the approval queue with a notification. The floor is a best-effort guard against an agent's mistakes, not a security boundary, so *Full
access* is only as safe as the session's isolation tier.

**Model selector (implemented):** the footer's model is a themed list of the model table
(`models.list`; each row by id, with its spec, location, and tier; rows whose endpoint is not
configured are shown but cannot be chosen) followed by local models found in the model folders
(`models.local`, as `llamacpp:<id>` or `vllm:<id>`, unless the table lists them already); a
model outside both is listed as is, and a session without one reads *No model* in the accent
colour. Choosing one switches the session with `session.control` `set_model`, also for a session
waiting for a model, whose queued messages then run. While a turn runs the choice is held and
marked *After this turn* (with a button to keep the current model), and applied when the turn
ends, since switches apply between turns ([03](03-agent-engine.md#switching-models-and-compaction)).
Choosing never loads the model ([04](04-models-and-routing.md#when-models-load)): the transcript
gets *Switched model from x to y* at once, and when the estimate says the conversation is over the
new model's budget the footer notes that older turns are compacted when you send. The next
message loads the model and, when its context window is smaller than the conversation needs,
compacts it first: the transcript gets a notice naming the target, the token counts, how many
messages were summarised, and by which model. A refused switch (a missing key, a local model
the folders no longer have, a turn that started meanwhile) shows its error beside the list and
leaves the model as it was; a model that cannot load, or a conversation nothing fits, fails that
message's turn with the reason, and another model can be picked.
Breakdowns of a compacted conversation list the summary as its own category, *Conversation
summary*. (Before this, the picker sat among the header pills as a borderless native select
that read like plain text, listed only the table, and fell back to plain text when the table
was empty — so it was easy to miss and absent for local-only setups.) Each local model's option
(and each table row that names a local model) says what it is under its name: quantisation,
the context it was trained for, the most one agent may use (with *(auto)* when it is sized
automatically), the KV cache reserved (now while loaded, or at first) when that is less, and the
daemon's plan for it under its settings: layers on the GPU and the VRAM and RAM it takes (live
values while loaded), such as *Q4_K_M · 256k ctx · up to 128k/agent (auto) · 16k reserved ·
36/36 layers GPU · 3.1 GB VRAM · llama.cpp*, *22/48 layers GPU · 9.8 GB VRAM + 11.2 GB RAM* when
it would split, or *CPU · 6.2 GB RAM* on the CPU lane. The context meter below the input measures
a local model's conversation against that per-agent ceiling, so compaction starts near the
ceiling, not near the current reservation. The Local models panel shows the layers on the GPU
(*36/36 layers on GPU*), says when the rest run on the CPU, and when a fixed KV pool was cut to
fit the GPU. A local model that cannot run here (a vLLM folder off Linux) is
listed but cannot be chosen. The new-session composer's model list is the same.

**Favourite and hidden models (implemented):** every row of the Models panel (the model table,
hosted and local) and of the Local models panel has a star (favourite) and an eye (hide). A
model has one mark at most, so favouriting a hidden model shows it again. Marks are kept by the
spec a session uses (`<runtime>:<id>` for a local model, the row's spec for a table row), so a
local model marked in either panel is marked in both; they are app settings, saved in the
`settings` document in `desktop/` ([persistence](#settings-and-persistence-implemented)), and
survive updates and reinstalls. In the model panels favourites come first, then the other
models in the panel's own order, then the hidden ones in a collapsed *Hidden (n)* group at the
end, dimmed, each with its eye to show it again. The model configuration dialog's navigation follows
the same order with *favourite* and *hidden* badges. The session's model selector, the
new-session composer's model list, and the command palette's *Use …* commands list favourites
first (under *Favourites*, the rest under *Models*, when any model is marked) and leave hidden
models out; the session's current model stays listed, dimmed under *Hidden*, while it is
hidden, so the selector still shows what is selected. The main window also keeps the daemon's
`models.hidden` in step with the hidden marks (through `models.set_config`, only when they
differ), so the root agent's `list_models` leaves hidden models out and a hidden local model
outside the model table is not offered for delegation by name
([model awareness](04-models-and-routing.md#model-awareness-and-model-cards-implemented)).
Routing still sees a hidden model that is in the model table.

### Branches and model comparison (implemented)

**Branch from here.** A message in the transcript shows a quiet branch button at its corner on
hover or keyboard focus. On a reply it starts a new session holding the conversation up to that
reply (`session.branch`, `at` the entry's history event) and opens it; on one of the user's own
messages the branch ends just before the message, which comes back in the composer to edit and
send again. The original is kept as it was, and the new session says where it came from under
its title (*Branched from …*, with a link back). Archived sessions offer no branch button.

**Branches in the list.** A branched session is listed directly under the session it was
branched from, one level in (deeper for a branch of a branch), as the same compact one-line row;
its tooltip names its source. A branch whose source is archived, in another group, or deleted
stands on its own.

**Comparing models.** The compare button in the session footer (disabled while a turn runs)
opens a form: tick two to four models (enabled table rows, then local models found in the
folders) and write the message. Starting it calls `session.compare`, adds the branches to the
list at once, and replaces the session panel with the compare view: one column per model with
its alias, tokens in and out, cost, and model time for the turn, and its reply as it streams
(the quickest and cheapest finished columns are tinted). *Keep this one* calls
`session.compare_pick`, which keeps that branch and archives the others (they stay in the
archive); *Open* shows a column's branch as an ordinary session, whose note links back to the
side-by-side view. The column state, standouts and form rules are in
`apps/desktop/src/sessions/branches.ts` (tested in `test/branches.test.ts`).

### Exporting transcripts (implemented)

A session's ⋯ menu in the Sessions panel has **Export as Markdown** and **Export as JSON**,
for running and archived sessions alike. The app asks the daemon for `session.export`, which
builds the transcript from the session's journal, and saves it through the native save dialog
(a download in a browser build). In the app the shell opens the save dialog itself and
writes only the file picked there (`save_export`; the page cannot name a path, only a `.md` or
`.json` file name to suggest, and transcripts over 64 MiB are refused). Exports are verbatim,
so they can hold anything an agent read or ran, secrets included: when one looks like it
holds a credential (a known key prefix, a private key, a JWT; git hashes do not count) the
shell asks before opening the save dialog, and the CLI prints a warning. No warning does not
mean no secrets: read an export before sharing it. The transcript holds every message, each tool call with its arguments and result, the
subagent task tree with each subagent's own log under the task that started it, compaction
notices, and the files attached to messages or created by agents. Subagents of an archived
session open in the agent view like any other (read-only: no *Resume* or *Cancel*, no
composer). The CLI does the same with `imperium session export <id> [--format md|json]
[--no-subagents] [-o file]` ([02](02-architecture.md)).

### Session search (implemented)

A search box sits at the top of the Sessions panel (*Search sessions*, or *Search this
environment's sessions* in an environment's view). Typing searches what was said and done in
every session, running and archived, with `session.search` (after a short pause in typing; Escape
clears it); while a query is entered its results replace the list. Each result is one session:
its title (with an archive mark when archived), the best excerpt with the matched words marked,
and what matched (*You*, *Reply*, *Tool*, *Subagent brief*, *Subagent report*, or *Title*), when,
how many more matches the session has, and *related* for a match found by meaning rather than by
its words. Choosing a result opens the session scrolled to that point of the conversation, the
entry outlined for a moment: each transcript entry remembers the index of the history event it
started at, and the hit's `event_index` picks the entry holding it. The search call is exported
as `searchSessions` (`apps/desktop/src/sessions/sessionSearch.ts`) for the command palette. How the index
is built and ranked is in [17](17-memory.md#session-search-implemented).

### Attachments and voice input (implemented)

**Attaching files.** The session composer takes files three ways: the paperclip button (a native
file picker in the app, the browser's in a browser), pasting (a screenshot or copied files), and
dropping them on the composer, which shows a dashed ring while files are over it. In the app the
window reports dropped files by path; in a browser the files themselves are uploaded (up to
12 MB each, the most one daemon message carries). Each file is staged with `session.attach` and
shows as a chip above the message box: a thumbnail for images, an icon otherwise, its size, a
spinner while it uploads, and a warning with the reason when the daemon refused it (too large,
unreadable). Its tooltip names the file's path and how the model will receive it. Chips can be
removed before sending; a message can be sent with attachments and no text. The chips go with
the message (`session.send`'s `attachments`, also when it waits in the queue, where a paperclip
and count mark it) and show on the message in the transcript, from the journal, so resumed and
archived sessions show them too.

What the daemon does with a file depends on what it is (read from its first bytes, not its
name) and where it is:

- **Where it lives.** A file inside the session's folders or path grants is referenced where it
  is. Anything else (pasted, uploaded, or dropped from elsewhere) is copied into the session's
  attachments folder, `<data>/attachments/<session id>/`, which the session's tools may read
  (a read-only grant) and which is deleted with the session. Files over
  `sessions.attachment_max_mib` (default 10) are refused. When the message is sent the daemon
  checks each attachment again: it must still be in the attachments folder or inside the
  session's folders and grants (a client cannot point it elsewhere), and its kind and size are
  read again from the file.
- **Images** (PNG, JPEG, GIF, WebP) go to the model as image parts when the session's model has
  vision: its model table row is tagged `vision`, or it is a local llama.cpp model with a
  multimodal projector (`mmproj`) beside it. For a model without vision see
  [images for a model that cannot see them](#images-for-a-model-that-cannot-see-them-implemented):
  another model describes the image, or, when none can, the agent is told it could not be seen
  and given the image's path to use with its tools (the chip still says *path only*).
- **PDFs**: the text is extracted (the `pdf-extract` crate) and included, up to 100,000
  characters with a truncation marker; for a vision model the JPEG page images embedded in the
  PDF (what scanned pages usually are) go along as images, up to eight. For a model without
  vision, a PDF with no text layer has those page images described like an image. Pages that
  are drawn rather than embedded are not rendered to images: there is no PDF renderer in the
  build, so a scan whose pages are not embedded JPEGs is reported as unreadable.
- **Text files** (UTF-8 without NUL bytes) are included up to 100 KiB, cut at a character
  boundary with a marker naming how much was left out and the path to read the rest from.
- **Anything else** is a path the agent reads with its tools.

The model reads the files in an `<attachments>` block before the message's text, read again at
the start of the turn, but only while the path is still the one checked when the message was
sent and is a regular file; a file replaced by a link, a pipe or device, or moved in the
meantime is reported as unreadable instead, and so is an image or PDF that grew past the size
seen at send (text is cut at 100 KiB anyway). Known limits: the check and the read are two
steps, so a swap in between is not caught, and a hard link put at the same path is
indistinguishable from the file. The context breakdown counts that block and the images (estimated at 800
tokens each, calibrated by the reported prompt size) as **Attachments**. In a remote runtime
(Docker container, WSL, SSH) the attachments folder is not reachable by the agent's tools; text,
PDF text, and images still go in the message.

#### Images for a model that cannot see them (implemented)

When the session's model has no vision, an attached image (and the page images of a PDF with
no text layer) is described by a model that has, and the model reads the description in the
`<attachments>` block where the image would have gone:

- **Which model.** `attachments.describe_with` names a row of the model table (its `id` or its
  `<endpoint>:<model>` spec), used as given. Unset, routing picks a row tagged `vision` (a task
  tagged `vision`: `[[routing.rules]]` and `require = ["vision"]` apply, unavailable or backed-up
  local models are skipped), and when the session's model runs on this machine only a local
  row, so a picture is not sent to a hosted model unasked; with no local vision row it is not
  described, and the model is told so. In a hosted session, routing may pick another provider's
  `vision` row, so the image goes to that provider. Naming a hosted row in `describe_with`
  from a local session sends the image off the machine on purpose: the session's notice then
  says "sent to a hosted model". A local llama.cpp model with a projector but no `vision` tag
  is not picked; tag its row or name it in `describe_with`.
- **How it is framed.** The text is wrapped as a machine-generated description (naming the
  model that wrote it), marked as untrusted data about the file and not text from the user or an
  instruction; tags that could close the block (`</attachment>`, `</description>`) inside it
  are defused. The describing call is told to treat text in the image as content to transcribe.
- **Budgets.** One call per attachment (a scanned PDF's pages go in one call), at most
  `attachments.describe_max_tokens` tokens (up to four times that for a multi-page PDF) and
  `attachments.describe_timeout_s` seconds (a local model's loading time counts); the whole
  message waits at most twice that, and a model that timed out or failed is not asked again for
  the message's other images ("describing model unavailable"); at most
  `attachments.describe_max_images` images (attempts count, failed or not) and
  `attachments.describe_max_chars` characters of description per message, later ones noted as
  not described. While the session is stopped, describing waits for it to go on, and a call
  the stop cut short is asked once more after the resume instead of leaving the image unseen.
  A notice ("describing N images…") says so before the first call. A scanned PDF is parsed
  once for the whole message: the text read to decide whether it needs describing is the text
  the model is given. The call counts toward budgets and the usage log as any model call, and a
  model whose budget does not allow it is not called.
- **Cache.** A description is kept for the session by the SHA-256 of the file and the describing
  model, so the same file attached again, or in a later message, is not described again. A
  resumed session keeps the descriptions in its saved conversation; the cache itself is not
  saved.
- **When nothing can describe it** (no vision row, `attachments.describe` off, a failed or slow
  call, a budget, a limit above), the model is told in the block that the image could not be
  described and why and that it has not seen it, and the user gets a notice naming the
  attachment and the reason. An image is never left out without a word.

**Push-to-talk.** Hold the push-to-talk key (Settings → Chat: Ctrl+Space by default, Alt+Space,
Ctrl+Shift+Space, F8, or *Off*; on macOS Cmd counts as Ctrl, and where the system keeps a
combination for itself, such as Spotlight's, pick another) while
focus is in a session, or hold the mic button beside the message box, to record from the
microphone; release to transcribe. A status line above the composer shows a pulsing dot and
the time while recording, then *Transcribing…*. The words go into the message box at the caret
(not sent), so they can be checked first. Esc cancels a recording; a tap shorter than 0.3 s is
discarded. A keyboard press of the mic button (Enter or Space) toggles recording instead.

Recording uses the browser's MediaRecorder (Opus in WebM where supported), the same in the app
and a browser. The audio goes to the daemon (`voice.transcribe`), which sends it to the
OpenAI-compatible `/audio/transcriptions` endpoint configured as `[media.transcription]` (see
[media](09-media-generation.md)); without one the status line says speech-to-text is not set up.

**Local whisper (implemented).** With `[voice] transcription = "local"` (default `hosted`) the
daemon transcribes on this machine instead: it runs whisper.cpp's `whisper-server` as a
supervised child process and sends the recording to its OpenAI-compatible
`/v1/audio/transcriptions` route on a loopback port. Nothing leaves the computer.

- **Setup.** `voice.local.binary` names the `whisper-server` (found on `PATH` when empty) and
  `voice.local.model` a ggml model file such as `ggml-base.en.bin`. With no model, turning on
  `voice.local.download` fetches `voice.local.model_file` from `voice.local.model_repo` the first
  time, through the same Hugging Face path model downloads use, into the models download folder;
  it is off by default so nothing downloads unasked. The app's WebM/Opus recordings are decoded
  by ffmpeg (`voice.local.convert`, on by default), which must be installed.
- **Lifecycle.** The server starts on the first recording (loading the model takes a few
  seconds, shown as *Transcribing…*), is checked until it answers (`/health`, else its start
  page) and abandoned after `voice.local.start_timeout_s`, stays up while recordings come, and
  stops after `voice.local.idle_stop_s` of no use (never during a transcription). One that exits
  on its own, or whose last request failed, is replaced by the next recording. Its output is
  `<data>/logs/whisper.log`.
- **Memory beside the language models.** Before starting, the server's footprint (the model file
  plus a quarter and 256 MiB) is planned against the room left beside the loaded models: on
  the GPU when it fits there and `voice.local.gpu` is `auto`, else on the CPU (`--no-gpu`) in
  RAM, else the recording fails saying how much memory is missing. While it runs, its memory is
  taken from the budget the residency controller plans language-model loads against, and added
  back into the controller's readings of free memory so it is taken off once, not twice. Loading
  a model while the server starts is serialised: the server looks at the room only after any
  model load in flight has finished.
  It never makes a language model leave. When a language model cannot load only because of the
  memory it holds, the server stops as soon as no transcription is running and the model loads;
  the next recording starts it again, on the CPU if the GPU is taken.
- **Failures** (no binary, no model, no room, a server that did not start or answered with an
  error) reach the push-to-talk status line as its error, naming the setting to change or the
  log to read.

Not built: swapping an idle language model out to give whisper the GPU (it falls back to the
CPU instead), streaming partial transcripts while still recording, and a status row for the
server in the Local models panel (`model.residency` events do not carry it).

The microphone needs the operating system's permission. macOS asks the first time (the app's
`Info.plist` carries the usage description); on Windows, WebView2 asks; a refusal shows how to
allow it in the system's privacy settings. No Tauri capability is involved (recording is a web
API), and on Linux it depends on WebKitGTK's media support.

## Setting up models (implemented)

Models already on disk are set up from the app, with no config file edit and no daemon restart.
Each step below writes the user's `config.toml` through the settings table and applies the change
to the running daemon at once ([04](04-models-and-routing.md#setting-up-models-from-the-app)).

- **First run.** While no model is usable (no default, no table row with a configured endpoint,
  no runnable local model), the new-session composer and the Local models panel show three
  cards, each with an icon, one line of explanation and one large action (`--control-h-lg`):
  *Add a model folder* (*Add folder*: the native folder picker; the folder is scanned at once and the
  number of models found is reported), *Download from Hugging Face* (*Download*: opens the Local
  models panel's download form), and *Connect an API endpoint* (*Connect*: a form of `Field`s for a
  name, base URL, API key, and an optional model, which then becomes the default; the key goes to
  the system keychain, `models.add_endpoint`). The cards stack in a narrow column and sit side by
  side in a wide one; failures show as `Notice`s.
- **Layout.** The Local models panel lists the models first, with a small, quiet *Rescan* (scan
  the folders again) and *Configure* above them; then the skipped items and the Hugging Face
  download form; and the *llama.cpp runtime* card at the bottom. It shows no free-memory summary
  (the [Resources panel](#resources-panel-implemented) does). The model tables use the shared
  `.table` and nothing in these panels has a fixed minimum width, so a side column of about 255 px
  (the default layout at 1024 px wide) shows every control inside it: the Models table drops its
  Tags column below 44 rem of panel width and its Where and Tier columns below 36 rem (they move
  to lines under the name), and the Local models table drops the *On this machine* column below 52
  rem (the fit moves under the name) and below 44 rem becomes one block per model, its name and
  facts above its actions.
- **Model folders.** Model folders are set in Settings → *Models*: *Add folder…* (the native
  folder picker; the folder is added to `models.folders` and scanned at once) and the *Model
  folders* list, one path per line, where a folder is removed (nothing in it is deleted). The
  first-run choices above also add one.
- **Runtime.** A *llama.cpp runtime* card runs `runtime.status` when the panel opens: which
  `llama-server` is used and where it comes from (*Your llama-server*, *Found on this computer*
  with how it was installed, such as winget or LM Studio, or *Imperium's build* with its
  release), its path, backend (*CUDA 13*, *Vulkan*, *CPU*, ...), whether the GPU is visible,
  its build number (with the release Imperium uses when it is older), and a verdict (*ready*,
  *check*, *not ready*) with the problem and what to do in words, such as *llama-server can't
  see your GPU — the CUDA runtime is missing or the NVIDIA driver is too old for this build*.
  With no runtime it says Imperium downloads the official build the first time a local model is
  needed (opening the panel counts, so the install usually starts there, with a progress bar
  from `runtime.install_progress`), or, with `runtime.llamacpp.auto_install` off, that it must
  be installed or chosen. *Use detected…* is a choice list of the other `llama-server`
  executables found on this computer, with *Use this*; *Install official build* installs
  llama.cpp's build for this machine (falling back to the next build when one does not run);
  *Choose file…* opens the native file picker for your own; *Stop using my llama-server* drops
  a custom server. When an update is available
  ([llama.cpp updates](04-models-and-routing.md#llamacpp-updates)) the card says so: *Update*
  runs the package manager's upgrade and shows its output (`runtime.update_progress`); *Use
  Imperium's build instead* installs the official build for a folder Imperium does not modify.
  When the server in use changes, the model listing is read again.
- **Choosing a local model without a runtime.** When no `llama-server` is there
  (`models.local` reports the runtime not installed), choosing a llama.cpp model in a session's
  model selector first says *No llama.cpp runtime yet — open Local models → Runtime to install
  one or choose an existing llama-server*, with *Open Local models*, *Use anyway*, and a button
  that keeps the current model.
- **Models.** Each model row shows its id, its repository (`unsloth/Qwen3-8B-GGUF`), *vision*
  when a projector is paired, *default*, quantisation, size, trained context and per-session
  context, and fit on this machine. *Use* makes it the default for new sessions (`models.use`),
  *Test* loads it and sends one short prompt, showing the reply, tokens, tokens per second and
  load time under the row (`models.test`), and the sliders button opens the
  [model configuration dialog](#model-configuration-dialog-implemented) on that model. Models
  that cannot run here say why under the row. Items left out of the scan (image models,
  diffusers pipelines, unreadable files) are listed, with the reason, in a collapsed section.
- **Benchmark.** [Settings → Models](#local-model-benchmark-implemented) can measure each local
  model's load time and speed on this machine.
- **Settings.** `models.default`, `models.folders`, and `runtime.llamacpp.custom_path` changed in
  Settings apply at once too; *Install llama.cpp automatically* (`runtime.llamacpp.auto_install`)
  and *Update a package-manager llama.cpp* (`runtime.llamacpp.update_external`) are there as
  well.

**Stop and resume (implemented):** while the agent or any subagent works, the session header
shows **Stop**. Ctrl+. (⌘. on macOS) does the same, and on a stopped session it resumes. Stop
halts the root and every subagent at their next safe point
([03](03-agent-engine.md#stopping-a-session)): a tool call in flight finishes, a model request
in flight is cancelled and sent again on resume, and local models may free the session's
slots. The status badge reads *Paused*. The header's **Resume** and a banner above the composer
("Stopped. The agent and its subagents are paused where they were; messages you send wait in
the queue.") continue the work. While stopped, the composer still works: messages you send
are queued (with Edit, Steer, and Remove as usual) and run after Resume. The Agents panel
shows running subagents as `paused`. The stop survives a daemon restart; the session waits
for Resume.

**Composer, queue, and steering (implemented):** the composer stays enabled while the agent
works, and sending while a turn runs always queues: the send key (Enter, or Ctrl+Enter, as set
in Settings) and the send button (which reads *Queue* then) put the message in the queue for
after the running turn. Waiting messages are listed just above the composer, dimmed and in
italics while they wait, each marked *Queued* with three buttons: *Edit* (inline; Enter
saves, Escape cancels), *Steer*, and *Remove*. *Steer* (`session.queue` with
`action: "steer"`) moves that message into the running turn: it is marked *Steering…* with an
accent edge until the agent reads it at its next safe point (after its current model response
and tool calls), and can then only be removed, not edited; an edit already open on it closes,
and an edit the daemon refuses is reported. If the turn ends first it runs as the next turn,
ahead of the rest of the queue, whose order is kept
([03](03-agent-engine.md#queueing-and-steering)). The one shortcut that steers directly, without
queueing first, is Ctrl+Shift+Enter in the composer. A steering message also pauses the
subagents the agent is running, which the [Agents panel](#agents-panel-implemented) then shows.
The list follows `queue_changed`, so every window, detached panels included, shows the same
queue, and it empties as messages are delivered. In the transcript a steered message is a user
bubble with an accent edge where the agent read it, labelled *Steered*, or *Steered — N
subagents paused* when it paused any.

**Context breakdown (implemented):** the footer's context meter shows how full the root
agent's context was on its latest model call, as a slim bar (amber from 70%, red from 90%) and
`12.3k / 32.8k` (or just the tokens when the window is unknown; its tooltip gives the
percentage). Pressing it opens the *Context window* section, which shows, for the
selected agent (root first, then each subagent by task id, chosen from a list when there is
more than one), what that agent's latest call read, from its `context_breakdown` event
([03](03-agent-engine.md#context-management)): a bar segmented by category against the window
with the free space last, and a table listing every category by name with its tokens and share
of the window (or of the request when the window is unknown), the free space, and the total.
Colour only repeats what the table says in words: the bar has a text label, and each
segment and swatch is named. Below it, the counting method in words (*Exact, counted with the
model's tokenizer*; *Calibrated to the provider's count*; *Estimated; the provider reported no
usage*) and the prompt tokens served from cache. A negative template overhead is listed but not
drawn. Session totals and recent outputs follow.

## House models on first run (implemented)

While the user has no model at all (no configured hosted endpoint, no local model found in the
model folders, no default), the new-session view shows a `.card` above the model setup, titled at `--fs-lg`, suggesting
Emperor (the root agent and planner, on a base of about 4B) and Consul (its critic and reviewer,
which checks Emperor's plans and delegated work and can reject them, on a base of about 9B),
Imperium's own fine-tuned open-weight models, each with a role blurb and an approximate size
in a `.table` like the model lists (placeholders until the models are published). The catalogue is `apps/desktop/src/models/houseModels.ts`; an entry with no download
source (both, today) shows a *Not yet available: still in training* badge under its name and has no button. An entry
with a Hugging Face source gets a *Download* button that runs the same `models.hf_files` and
`models.download` calls as the Local models panel's form. The ghost × icon button dismisses the card for good (the
`houseModelsDismissed` setting), and it never shows once any model is present. Not built, until
the models are fully trained: a hardware-based pick of size and quantisation, progress and resume
on the card, checksum verification, and making the model the default after the download
([12](12-roadmap.md), item 44).

## Model configuration dialog (implemented)

A modal edits model settings. It uses the Settings window's shell (`Dialog`: the same
sheet, sizing, header with the title and ×, navigation column, and Esc or a click on the backdrop
to close), with a footer for its actions. It opens from *Configure models* in Settings (Models and
Local runtime), from *Configure* in the Local models panel (on that model) and from *Configure* in
the Models panel (on the model table). The navigation column has a *Search models* field and two
groups, *Local models* (the models in `models.folders`) and *Model table*, each model with its
badges; a model with an [alias](10-configuration.md#model-aliases) is listed by it, with its id
beside it and on hover, and the search matches either. The selected model's alias (or id) is the
heading.

Each model's settings are tables, one row per setting, in groups (the rows, their groups and the
wording of their explanations are one declarative table, `models/modelSettingsTable.ts`). A row has the
setting's name with a small *(i)* button, a line saying what it affects (memory, speed, quality,
routing, cost, names, availability, reliability), the value and a *reset* that clears the saved
value. The *(i)* is a keyboard-focusable button with a shared `Tooltip`: hovering it (after 400 ms)
or focusing it from the keyboard opens a tooltip that says in plain words what the setting does and
what changing it trades off (it is the button's `aria-describedby` while it shows, so a screen
reader reads it too), and Esc closes the tooltip without closing the dialog. A touch tap (or a click
where hover is unavailable) toggles the same help inline under the setting's name instead, with
`aria-expanded` on the button; Esc closes the tooltip only while the button has focus. The settings tables are the shared `.table`; the tier, cache-type, flash-attention and
new-default pickers are `Select`s (no native select is left in the dialog); in-body warnings and
the dry run's validation message are `Notice`s, and a failed save shows as a `Notice` between the
header and the settings. A local model's groups:

- *Identity and availability*: **Use this model** (a switch, below), *Display name* (the alias
  shown instead of the id everywhere a model is named, with the id as the placeholder; empty shows
  the id again, or a table row's alias from config.toml) and *Model card* (what the model is good at
  and notes, which agents read when they pick a model to delegate to
  ([model cards](04-models-and-routing.md#model-awareness-and-model-cards-implemented)); a *From
  the model's metadata* fold shows what its header and model card say, with *copy into the card*
  while the card is empty);
- *Context and memory*: max context per agent (a number of tokens or *auto*, offered in the field's
  list; the placeholder shows the sized ceiling, such as *auto (131072)*), KV pool tokens (empty:
  starts small and grows on demand; a number fixes it), slots, K and V cache types (`q8_0` about
  halves the KV memory), flash attention and max GPU layers;
- *Speed: speculative decoding*: *Draft model* (the field lists the other installed llama.cpp
  models; with a draft set, *Draft max tokens*, *Draft min tokens* (0 or more, at most the maximum
  in effect) and *Draft GPU layers* (empty is llama.cpp's `auto`) become editable);
- *Limits*: max wait and max output tokens (0 is uncapped); *Advanced*: extra arguments (one per
  line).

Each field is optional and shows the effective value as its placeholder. For a vLLM model the
cache types, flash attention, GPU layers and draft rows are shown dimmed as not used. A
model-table row has *Identity and availability* (the same three rows), *Routing* (tier and tags)
and *Context and cost* (context window and the two prices, both or neither); the form edits
existing rows only and does not add or remove them.

**Estimated memory.** Above its settings a local model has an *Estimated memory* table, marked
*≈ estimate*: VRAM, RAM, the total needed, the model weights, the KV cache and where it would run
(all layers on the GPU, *22 of 36 layers on the GPU, the rest on the CPU*, CPU only, or too large),
each with its own *(i)*. The figures are the daemon's planner's for the settings being edited, not a
second formula in the app: the debounced (500 ms) `models.set_config` dry run returns
`required_bytes`, `kv_pool_bytes`, `weights_bytes`, `vram_bytes`, `ram_bytes`, `gpu_layers` and
`placement` for the model under the effective settings (context, pool, K/V cache types, GPU layers,
draft model; calibrated by the measured memory of earlier loads), and the table follows them,
greyed with *updating…* while the answer is on its way. The total is weights + KV pool + runtime
overhead (+ a draft model); the KV pool is its tokens times the bytes per token, which the cache
types scale, worked out for the pool at its ceiling (`effective.pool_max`: the fixed pool, or the most an
empty pool may grow to, which is the max context times the slots, cut to what fits), because that is
the most the model may reserve; a pool that starts small and grows with use does not make the figures
creep up. The KV cache row says so (*KV cache at max context 32,768*, or *at the full pool of
131,072 tokens* when the slots share more than one context; with no figure for the model's KV size
per token it names no size). The planner then fills the GPU with as many layers as fit and puts the
rest, with their share of the pool, in RAM (all of it in RAM on the CPU lane). The machine state it
assumes is an empty one: the memory measured when the daemon started, the same the context ceiling
and the pool were sized against, so nothing else loaded counts and a drift of the live readings
cannot show a pool that fits as split between the GPU and RAM. The *Local models* row's badge is
different: it shows what loading the model now needs, at the pool it has now (the smaller, starting
one until calls grow it), against the live memory, while the dialog shows the most it may reserve.
The figures are estimates, and the real ones also depend on the driver. A table row that names a local
model (`llamacpp:` or `vllm:`) shows that model's estimate; a hosted row says it *runs remotely*.

**Use this model.** The switch turns a model off without forgetting its settings. Off, the model
is greyed in the dialog's navigation (with an *off* badge) and its tables, dimmed with an *off*
badge in the Models and Local models panels, left out of the session model selector (except for a session already on it, whose selector keeps
showing it by its name with *(off)* and dimmed until another model is chosen), the new-session
chooser and the command palette, never chosen by routing or by an escalation retry, not offered to
agents (`list_models`) or the OpenAI-compatible API, and cannot start a session or be made the default
model; turning it on again restores all of that. A local model's switch is saved as its spec in
`models.disabled`; a model-table row's is its own `enabled`, and a row whose model is off in the
local list is off too, with its switch locked and a note saying so (see
[disabled models](04-models-and-routing.md#disabled-models-implemented)). The daemon refuses to
switch off the default model, so switching it off shows a *choose a model* list of the other models
that are on: *Apply* or *Save* then makes that one the default (`models.use`) and switches the old
one off; with no choice made, saving shows "choose another default model before switching it off". If that save fails after the default was changed, the message says the default was already changed. A default that is off counts as none in the new-session chooser, with a *Your default model is off; pick one* note.

*Apply* (or Ctrl+S) saves without closing: the dialog shows "Saved" for a moment and the values now
in effect (such as a cut pool or the layers on the GPU), and closing it afterwards keeps what was
applied; *Save* saves and closes. Both are disabled while nothing has changed. The summary says how
much context one agent gets, and warns when a fixed KV pool was cut to keep the layers on the GPU
or when the weights do not fit on the GPU, says which draft model the model drafts with (its memory
is in the figures), and shows the notice when a configured draft is missing, incompatible or does
not fit and the model runs without it. The footer shows the file the settings are saved to,
*Cancel* (or *Close* once nothing is pending), *Apply* and *Save*. *Save* sends only the models that
differ from what is saved, each as a whole-settings update, then closes and refreshes the panels; a
failure keeps the dialog open with the message. *Reset model* clears all of a model's saved
settings. A loaded model's badge reads `loaded · <in use>/<slots> busy · pool <reserved>/<pool>`.

A model's alias (`models/modelNames.ts`, from `models.list` and `models.local`) is what the interface
names it by: the session model list and the command palette's *Use …* entries (the id in the
description, and the palette's search matches either), the Models and Local models panels, the
Resources panel's loaded models, the session list's hover, the context breakdown, the telemetry table,
the Agents panel and task lines, and notices such as *Switched model from … to …*. The real id or
spec is the secondary text or hover. Ids and specs sent to the daemon and stored in settings are
unchanged. A notice already shown keeps the name it was written with.
