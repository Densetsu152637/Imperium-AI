# 07 — Desktop interface

## Implemented in phase 2

- The Tauri 2 shell (`apps/desktop`) reads the daemon's endpoint file through a native command
  and the React interface connects over the WebSocket with the subprotocol token. In a plain
  browser (development preview) it asks for the URL and token instead.
- The default view below, built from columns of spanning or stacked panels with draggable
  dividers; sizes are remembered.
- Session panel: environment header (project, model, isolation tier, cost), conversation with
  assistant replies rendered as Markdown (GitHub-flavoured, raw HTML disabled; a toggle shows
  the raw text), inline permission prompts (allow, always this session, deny), a floating
  composer, and context and outputs sections folded into the header.
- Telemetry panel: per-call tokens, time to first token, tokens per second, and cost, with
  session totals.
- Display settings: UI scale, prose and code text scale, density, and line length. They are
  stored per viewer in local storage for now and move into the daemon's configuration with
  the settings UI.
- Light and dark themes follow the system: metallic black and graphite with a petrol-teal
  accent and silver hairline trims by default, and a matching brushed-silver light theme. Side
  columns are brushed-metal rails; the conversation is a centred column with the user's turns
  as pills on the right.

- Detachable panels: every panel, built-in or from a mod, has a Detach button (⧉) that moves it
  to a window of its own. A detached window shows only that panel under a slim header with the
  panel's actions, a Pin toggle (always on top), and Dock. Many can be open at once; each keeps
  its position, size, and pin in the saved layout, and all reopen at the next start (moved onto
  a visible monitor if theirs is gone). Closing one with the OS button docks it; quitting from
  the main window leaves them detached. Only the main window writes the layout, so detached
  windows report pinning, moving, and docking to it; documents are synced between windows, and a
  detached window follows the main window's selected project and session. In a plain browser
  the window is a popup and cannot be pinned.

**Model download suggestions (implemented):** a `model_suggestion` event shows as a card in the
conversation, styled like a permission prompt: the repository, quantisation, size, how it fits
this machine (on the GPU, split with system memory, or on the CPU), and why it is suggested,
with **Download**, **Not now**, and **Don't ask again** (which turns off *Suggest model
downloads* in Settings). Downloading shows a progress bar from `models.download_progress`;
when it is done the card offers **Switch to it** (`session.control` `set_model`). Nothing is
downloaded until Download is clicked. The answers go through `session.suggestion_respond`
([04](04-models-and-routing.md#model-download-suggestions)).

Not yet: dragging panels and modules between columns, the edit-layout mode, project layout
overrides, mods, and themes beyond light and dark.

## Shell

Tauri 2 with a React + TypeScript + Vite frontend. The Tauri Rust side is thin: it attaches to a
running `imperium-core` or starts the one installed beside the app (see
[platform bundles](11-ci-cd.md#platform-bundles)), handles native menus, notifications, file dialogs, deep links, and
the updater ([desktop updates](11-ci-cd.md#desktop-updates)). All application logic goes through the daemon protocol, so the web UI could also
be served to a browser for remote use.

### The bundled daemon belongs to the app (implemented)

The app starts the bundled `imperium-core` as its own child process, so Windows Task Manager
shows it under Imperium rather than as a separate background process. When the app opens and
finds a bundled daemon it did not start (left by an earlier run, or the previous version's
`imperiumd`), it asks it to stop (`system.shutdown`) and starts it again as its child. A daemon
started some other way (from a terminal, or installed elsewhere) is used as it is. In a
development build the "bundled" daemon is the `imperium-core` next to the app in `target/`, so a
daemon started by hand from there is restarted too; turn on keep running to keep it.

**Lifetime:** by default the daemon exits with the app (**Settings → Daemon → Keep running after
Imperium closes** is off). Sessions are saved as they go and resume when the app starts the
daemon again. On Windows the child is also placed in a job object with kill-on-close, so even a
crash or an app ended from Task Manager never leaves it orphaned. With keep running on, the job
no longer ends it: the daemon outlives the app (schedules, project agents, and auto-fix keep
working) and then shows as its own process; when the app opens again it uses that daemon as it
is. The choice can be changed while the daemon runs. (On macOS and Linux the app stops the daemon
as it quits; there is no job object.) Settings → Daemon says whether the running daemon was
started by Imperium, left from an earlier run, or started outside the app.

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
│ Environments       │ Session: <name>                                      │
│ (and folders)      │ ┌──────────────────────────────────────────────────┐ │
│                    │ │ Environment · model · isolation tier · budget    │ │
│ stackable          │ ├──────────────────────────────────────────────────┤ │
├────────────────────┤ │ Conversation and plan                            │ │
│ Sessions           │ │                                                  │ │
│ (of the selected   │ ├────────────────┬─────────────────┬───────────────┤ │
│  environment)      │ │ Context        │ Outputs         │ Worktrees     │ │
│                    │ │ breakdown      │ (artefacts)     │               │ │
│ stackable          │ └────────────────┴─────────────────┴───────────────┘ │
│                    │ spans full height                                    │
└────────────────────┴──────────────────────────────────────────────────────┘
```

- **Environments panel** (top left): environments and their folders; planned: their location
  (local, container, SSH host), project-agent status, and schedules.
- **Sessions panel** (bottom left): sessions in the selected environment, with status, cost,
  and pending approvals.
- **Main session panel** (middle, full height): the selected session. It is built from
  sections — environment header, conversation and plan, context breakdown (tokens by system
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
    Other panels cannot be stacked above or below it.
  - `stack` — shares its column with other stackable panels, split by draggable dividers,
    collapsible to a title bar, and tabbable when space is short (default for projects and
    sessions).
  The user can override a panel's default in its settings.
- Columns and stack dividers are resizable; sizes are stored as proportions so layouts adapt
  across monitors.

### Modules inside panels

- **Mods live in panels.** A UI mod contributes **modules** (self-contained views such as a
  telemetry chart, a pet, or a cost meter) that can be placed into a panel's section areas or
  slots, or opened as panels of their own.
- Built-in sections (environment header, context breakdown, outputs, worktrees, ...) are
  modules too, so users can reorder, remove, or swap them with mod-provided modules.

### Rearranging

- Everything is drag-and-drop: drag a panel's title bar to another column, into a stack, or
  out into a new window; drag a module between panels. Drop targets preview the result.
- An **edit layout** mode shows all drop zones, panel boundaries, and hidden modules; outside
  it, layouts are locked to prevent accidental changes.
- **Layouts are global.** One layout applies to every project, and changes to it apply
  everywhere.
- **Project overrides are explicit.** A project uses its own layout only after the user
  chooses *Save as project layout* for it; nothing creates one automatically. While an override
  is active the Projects panel and window title show a marker, and *Use global layout*
  removes it. Overrides are stored in user config, keyed by project (or in
  `.imperium/layout.json` when the user chooses to share it with the project).
- Layouts can be exported and shared as presets (a preset includes panel instances, module
  placement, and settings, and lists the mods it needs). "Reset layout" restores the default
  view.
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

## Theming

Design tokens (colour, type, spacing, radius, motion) are CSS custom properties. Themes are
token sets; light, dark, and high-contrast ship by default. Fonts are user-selectable,
including ligatures for code.

**Icons (implemented):** every icon is a [Lucide](https://lucide.dev) line icon from
`lucide-react` (pinned in `apps/desktop/package.json`), the same thin, rounded style as
ChatGPT's and Claude's apps. One stroke width is set for all of them in `main.tsx`
(`LucideProvider`), and their size follows the text around them (`.lucide` in `styles.css`),
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

## Accessibility

Full keyboard navigation, visible focus, screen-reader labels, reduced-motion support, and
WCAG AA contrast for shipped themes.

## Connectors (implemented)

**Connectors** in the title bar (or Settings → Connectors) opens a window like Claude's and
ChatGPT's connector directories, with a search box and two tabs:

- **Installed:** MCP servers, each with its icon, command or URL, and status (*Not tested*,
  *Connected · N tools* with the tool names, *Error* with the reason, or *Off*), and buttons
  to test the connection, configure, remove (optionally deleting its saved secrets), and a
  switch to turn it off. Missing keychain secrets are flagged. Below them, plugins (skills,
  commands, hooks, and servers they bring, with an on/off switch) and skills.
- **Browse:** a card grid of the connector directory, filtered by category chips and search,
  each card with an icon, publisher, description, category, local or remote, and *Add* (or
  *Added*). *Add* opens a form for the connector's settings: a name, then its fields (folders,
  paths, URLs, tokens). Secret fields are password inputs saved to the system keychain and
  never shown again. A *Custom connector* card adds any server by command (arguments,
  environment, secret variables) or URL (bearer token, headers), with the tools that run
  without asking and a timeout; *Configure* opens the same form for an installed server, where
  saved secrets are kept unless replaced.

Adding or saving a connector tests it straight away. The window talks to the daemon's `mcp.*`
methods and the directory comes from the daemon; see
[managing MCP servers from the app](05-tools-and-isolation.md#connectors-managing-mcp-servers-from-the-app-implemented).

## Settings and persistence (implemented)

**Settings** (title bar, or Ctrl+,) opens a window with sections on the left and search across
every setting:

- **Imperium:** General (reopen where you left off, start imperium-core with the app), Appearance
  (accent colour, interface, message, and code scale, line length, density, message font,
  reduce motion), Chat (send key, Markdown, code wrapping, following new output),
  Notifications (sounds for approvals and finished sessions, a title badge), Panels (show or
  hide each panel, reset the layout), Keyboard, Daemon (start, stop, restart, log, keep
  running), Updates (installed version with **Check for updates** and **Check and install**,
  channel, how often to check, and checking when the computer starts; see
  [desktop updates](11-ci-cd.md#desktop-updates)), and Connectors (opens the Connectors
  window, with the MCP and plugin settings).
- **Accent colours:** twelve, shown as a grid of swatches in hue order (cyan, teal, green, lime,
  amber, orange, rose, magenta, violet, indigo, blue, silver); the chosen one is ringed.
- **Layout:** every section uses the same rows: the setting's title and description
  left-justified on the left, its control aligned right. A setting that depends on the one above
  it (such as checking at startup under the update schedule), and status lines or logs under a
  setting, are indented one step with a rule on the left.
- **Agent and daemon:** every setting the daemon exposes through `config.describe`, grouped by
  section (models, local runtime, memory, workflow, web, media, isolation, GitHub, pets, phone,
  resuming, network). Changes go to the user config file through `config.set` and apply when
  the daemon restarts; the window offers the restart.
- **Data and storage** shows where everything is kept and resets app settings or the layout.

App settings, the layout (panel sizes, closed panels, the selected environment and session),
and the last-known sessions are files in `desktop/` inside Imperium's config folder, beside the
daemon's `config.toml`, so they persist across updates and reinstalls. The workspace is always
shown: while the daemon is away, a bar above it offers to start or reach it and the lists show
the sessions as last seen.

## Environments and sessions (implemented)

The Environments panel lists the daemon's
[environments](14-projects-and-automation.md#environments-implemented), one row each: the
environment's icon and name, a pin mark when pinned, and a *⋯* menu on the right with *Edit*,
*Pin*/*Unpin*, and *Delete*. Pinned environments are listed first. Clicking a row selects the
environment (new sessions start there) and shows its sessions; double-clicking opens it for
editing.

*New environment* and *Edit* open the **environment window**, a sheet over the workspace like
Settings (Escape or a click outside closes it): the name; the icon, picked from a searchable grid
of every icon the app uses (`icons.ts`, the shared registry — Settings sections, Connectors, the
titlebar, and project glyphs; older saved names still resolve); the tile colour, picked from a
row of rounded-square swatches; and the folders, the first marked as the working directory, with
*Add folder* (the native folder picker) and *×* to remove one. A new environment is created with
all of it at once; for an existing one, folder changes apply at once and the name, icon and
colour with *Save*. *Delete environment* asks once more (folders on disk are untouched); *Delete*
in a row's menu opens the window at that question. Icons and colours are stored with the
environment in the daemon. The first time the app connects after environments arrived, every
project folder it knew (opened folders, the selected project, folders of running sessions)
becomes an environment of its own, keeping the icon it had.

**Runtime and persistent agent (implemented):** the environment window (create and edit) has a
*Runtime* section (`EnvironmentRuntimeFields`): a select for *This machine*, *Docker*, *WSL*,
or *SSH* with that runtime's fields (the image and an optional running container, the
distribution, or the host), *Test*, which calls `environments.test_runtime` for the saved
runtime and shows its answer in green or red (for example *Docker is running but image rust:1
is not present; get it with: docker pull rust:1*), and the *Persistent agent* switch for the
environment's [persistent agent](14-projects-and-automation.md#persistent-agent-implemented).
Both apply with *Create* or *Save*. For a remote runtime the folders are paths inside it
([runtimes](14-projects-and-automation.md#runtimes-implemented)), typed in with *Add path*
instead of the folder picker.

*New session* (in the Sessions panel's header) starts a session in the selected environment;
it is disabled, with the reason as its tooltip, while disconnected, with no environment
selected, or when the environment has no folders. The model list beside it picks the session's
model (*Default model* uses `models.default`); when no default is configured, starting with
*Default model* shows the daemon's explanation above the list instead of failing silently.
*Start in a worktree* beside it starts the session on new git worktrees of the environment's
repositories ([worktrees](14-projects-and-automation.md#worktrees)).

The Sessions panel lists every session across environments, each with its environment's icon
to the left of its title and a status badge (idle, working, needs attention, paused with its
resume time, watching with its pull request, merged), and a *⋯* menu with *Pin*/*Unpin*,
*Archive*, and *Show* its environment. Archived sessions (archived by hand, or on their own once
their pull request closes or after the inactivity set in Settings → Sessions; see
[archiving sessions](14-projects-and-automation.md#archiving-sessions)) leave the list and are
kept in a collapsed *Archived* group at the bottom, most recently archived first, with
*Unarchive* in their menu. Opening one shows its history with a note that it is archived and an
*Unarchive* button in place of the composer. A sort control above the list orders it *Recent* (the default: most
recently active first), *By environment* (grouped under environment headings, pinned
environments first), or *By status* (groups *Waiting for approval*, *Active* — working or
watching —, *Paused*, *Idle*, *Merged*); pinned sessions stay on top within each group, and the
choice is kept in the local layout. Pins on sessions and environments are kept by the daemon
(`session.control` `set_pinned`, `environments.update` `pinned`), so every client agrees and
they survive restarts; the daemon announces a changed session as `session.updated`. Clicking an
environment (or *Show* in a session's menu) opens that environment's view, which lists only its
sessions, with *All sessions* to return; clicking the row opens the session and selects its
environment. An environment's persistent agent is listed above everything else in its group,
titled *<environment> agent* with a *Persistent* badge; a session on worktrees has a *Worktree*
badge whose tooltip lists each worktree and branch. Renaming and stopping sessions are not
offered yet; deleting one is `session.delete` (`imperium session delete`), and a session
deleted elsewhere (`session.deleted`) leaves the list. The session header shows the status and the *Usage limits* controls (how to wait
when usage runs out, and *Resume now* while paused).

**Model picker (implemented):** among the header's environment pills, the session's model is a
picker listing the model table (`models.list`; rows whose endpoint is not configured are shown
but cannot be chosen, and a model outside the table is listed as is). Choosing one switches the
session with `session.control` `set_model`; it is disabled while a turn runs, since switches
apply between turns ([03](03-agent-engine.md#switching-models-and-compaction)). When the new
model's context window is smaller than the conversation needs, the daemon compacts it first:
the picker notes `compacted 30.1k → 9.2k tokens`, and the transcript gets a notice naming the
target, the token counts, how many messages were summarised, and by which model, followed by
*Switched model from x to y*. A refused switch (nothing fits, a missing key) shows its error
and leaves the model as it was. Breakdowns of a compacted conversation list the summary as its
own category, *Conversation summary*.

**Composer, queue, and steering (implemented):** the composer stays enabled while the agent
works. Its send key (Enter, or Ctrl+Enter, as set in Settings) queues the message for after the
running turn (the send button reads *Queue* then), and a *Steer* button beside it, or
Ctrl+Shift+Enter, adds the message to the running turn instead
([03](03-agent-engine.md#queueing-and-steering)). Waiting messages are listed just above the
composer, marked *Queued* or *Steering*, each with *Edit* (inline; Enter saves, Escape
cancels) and *Remove*; the list follows `queue_changed`, so it empties as messages are
delivered. In the transcript a steering message is a user bubble with an accent edge and a
*Steering* label, at the point in the turn where the agent read it.

**Context breakdown (implemented):** the header's *Context and outputs* summary shows how full
the root agent's context was on its latest model call, such as `12.3k / 32.8k tokens` (or just
the tokens when the window is unknown). Opened, its *Context window* section shows, for the
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
