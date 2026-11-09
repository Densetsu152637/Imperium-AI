# 07 — Desktop interface

## Implemented in phase 2

- The Tauri 2 shell (`apps/desktop`) reads the daemon's endpoint file through a native command
  and the React interface connects over the WebSocket with the subprotocol token. In a plain
  browser (development preview) it asks for the URL and token instead.
- The default view below, built from columns of spanning or stacked panels with draggable
  dividers; sizes are remembered, and stacked panels fold to their header
  ([folding](#columns-spanning-and-stacking)).
- Session panel: the title and status at the top, conversation with assistant replies rendered
  as Markdown (GitHub-flavoured, raw HTML disabled; an option shows the raw text), inline
  permission prompts (allow, always this session, always allow by a rule, deny; see
  [permissions](#permissions-implemented)), a floating composer, and under it a
  footer with the model selector and the context meter
  ([session footer](#environments-and-sessions-implemented)).
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

### Agents panel (implemented)

The Agents panel lists the subagents of the selected session: each row shows the task id, its
status (`running`, `paused`, `finished`, or `failed`), model, cost, brief, the messages it was given and
its tool calls, and its latest report. Subagents keep their state when they finish
([agent state](03-agent-engine.md#agent-state)), so a finished or failed row's ⋯ menu has
**Continue with…**: it opens a follow-up box under the row (Ctrl+Enter sends, Escape
cancels), and the follow-up is sent with `session.agent_resume`. The subagent works again with
its earlier conversation: its row goes back to `running`, the transcript notes
"Continued t1 on …", and its new report replaces the old one when it finishes (the cost shown
adds up across runs). While it runs, the menu item is disabled; archived sessions show no menu.
Subagents a daemon restart cut off are shown as failed with a report saying so, and can be
continued the same way. A subagent paused by a steering message
([below](#environments-and-sessions-implemented)) shows a `paused` badge, its progress as its
report, and **Resume** (continues from its saved conversation), **Re-brief…** (the follow-up
box), and **Cancel** (`session.agent_cancel`) until the root agent or the user decides.

### Resources panel (implemented)

The Resources panel (in Settings → Panels, closable and detachable like the others) shows what
the machine and the sessions are using: CPU, RAM, and per GPU its utilisation and VRAM as bars
with a two-minute sparkline each (hover a bar for per-core figures or swap), a table of the
local models loaded, in use or idle, with placement (or the CPU lane), the context per session
and the KV pool's reserved and total tokens, memory, busy slots and queued calls (or "held by a
turn" while an agent is mid-turn on it) or the idle-unload countdown, the sessions on each, and
an Unload button for idle ones, and a table of the hosted
endpoints sessions use with requests, tokens, latency, and errors over the last 15 minutes.
The panel subscribes to `system.resources_changed` only while it is open and its window is
visible, so the daemon samples nothing otherwise
([04](04-models-and-routing.md#active-models-and-resource-monitor)).

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
- **Sessions panel** (bottom left): sessions in the selected environment, with status, cost,
  and pending approvals.
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
    Other panels cannot be stacked above or below it.
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
  *Expand all* for the column (dim until the header is hovered or the button has focus). The
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

**Theme (implemented):** Settings → Appearance → Theme picks System (the default, following the
operating system and its changes), Light, or Dark. `settings.ts` sets `data-theme` on the root
element and `styles.css` keys the light tokens on `:root[data-theme="light"]`; the command
palette's *Switch to light/dark theme* flips it. Mod panels pick up the tokens when they open.

## Command palette (implemented)

**Ctrl+K** (Cmd+K on macOS) opens the command palette in the main window and in detached panel
windows; pressing it again closes it. It is a modal dialog near the top of the window with one
search box over:

- **Actions:** new session, new session in each environment, new environment, Open Settings,
  Open Connectors, Configure models, switch to the light or dark theme, check for updates, and
  for the open session: compact the conversation, archive or unarchive, pin or unpin.
- **Sessions** by title (with the environment's name), and **Archived sessions** in a group of
  their own; choosing one opens it.
- **Environments:** show one (its sessions and new sessions there), or edit it.
- **Models:** switch the open session's model to any usable model-table row or local model
  (`session.control` `set_model`, as the model selector does).
- **Panels:** open a closed panel, close or detach a docked one, show or dock a detached one
  (the Usage and Local models panels among them; mod panels too).
- **Settings:** every section by name (*Settings: Budgets*, *Settings: Keyboard*, …), opened
  directly.

Matching is fuzzy: every typed word must match the title, a subtitle (a session's
environment, a model's details), or a command's keywords, as a substring (best at a word start)
or as letters in order. With nothing typed, the last few commands run lead under **Recent**;
while typing, recently run commands are lifted above equal matches. The last 20 are kept in the
layout document. Arrow keys and Page Up/Down move, Enter runs, Tab and Shift+Tab jump between
groups, Escape (or a click outside) closes, and the mouse works too. Commands with a shortcut
show it (Open Settings: Ctrl+,).

**Registry:** `src/commands/registry.ts` holds named command sources; a feature registers one
with `useRegisterCommands(name, build)`, and the palette calls each source when it opens, so
commands always run the feature's current handlers. The app's own commands
(`commands/appCommands.ts`) are built from the handlers `App.tsx` already uses for the same
buttons. Ranking and grouping are pure functions in `commands/rank.ts`, tested in
`test/rank.test.ts`. A panel window's palette lists the same commands but sends the chosen one's
id to the main window (a `command` panel-window event), which runs it, records it as recent, and
comes forward; theme changes leave the panel window in front. The shortcut is listed under
Settings → Keyboard; shortcuts are not rebindable yet.

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
  (theme, accent colour, interface, message, and code scale, line length, density, message font,
  reduce motion), Chat (send key, Markdown, code wrapping, following new output),
  Notifications (sounds, a title badge, and system notifications; see
  [notifications](#notifications-implemented)), Panels (show or
  hide each panel, reset the layout), Keyboard (the shortcuts, the command palette's among them), Daemon (start, stop, restart, log, keep
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
- **Permissions** (under Agent and daemon): the global path grants and permission rules; see
  [permissions](#permissions-implemented).
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
settings)` in `apps/desktop/src/notify.ts`, which applies these settings; budget alerts use it
([budgets](#budgets-and-usage-implemented)). Which events notify is decided in `apps/desktop/src/attention.ts` and covered by
`apps/desktop/test/attention.test.ts`.

## Budgets and usage (implemented)

**Settings → Budgets** edits the daemon's [budgets](10-configuration.md#budgets)
(`budgets.get` / `budgets.set`, saved to `[budgets]` in the user config): when weekly budgets
start, the day monthly ones reset, the default alert thresholds, and one card per budget with an
optional name, its scope (*All usage*, an environment picked from the list, an endpoint, or a
model, the last two with suggestions from the model table), its window (daily, weekly, monthly),
a token limit, a cost limit in US dollars (either may be empty), what happens when it is used up
(*Warn only*, *Ask first*, *Block*), and its own alert thresholds (empty: the defaults). Each
saved budget shows where it stands in its current window. *Save* applies the budgets at once.

The **Usage** panel (in Settings → Panels, closable and detachable like the others) is the
dashboard: every budget's current window as a bar with its share, tokens and cost against the
limits, when it resets, and its action; a bar chart of the last 30 days, by tokens or cost, for
all usage or, after clicking a budget, that budget's scope; and the top five environments,
endpoints, and models over the same days. It follows `budgets.changed`, which the daemon sends
after each model call.

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
*Add folder* (the native folder picker) and *×* to remove one. Its *Permissions* section holds the
environment's path grants and rules ([permissions](#permissions-implemented)). A new environment is created with
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

**New session (implemented):** *New session* in the Sessions panel's header (disabled while
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
deleted elsewhere (`session.deleted`) leaves the list. The session's top bar shows only its
icon, title, and status badge (with *Resume now* while paused); everything else about the
session sits in the footer under the composer.

**Session footer (implemented):** a compact row under the composer, as in chat apps: the model
selector at the left, then the session's cost once it has one, an options button, and the
context meter at the right. The options button opens *Display* (*Render Markdown*, and the
project and isolation tier) and *Usage limits* (how to wait when usage runs out:
`session.control` `set_resume_policy`) above the composer; the context meter opens the
[context breakdown](#environments-and-sessions-implemented), session totals, and recent outputs
there. Only one of them is open at a time; each closes with its button again or the panel's
close button.

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
the context it was trained for and the per-session context it is started with when different,
its size on disk, and how it fits this machine, such as *Q4_K_M · 256k ctx (8k per session) ·
2.3 GB · GPU + RAM · llama.cpp*. A local model that cannot run here (a vLLM folder off Linux) is
listed but cannot be chosen. The new-session composer's model list is the same.

## Setting up models (implemented)

Models already on disk are set up from the app, with no config file edit and no daemon restart.
Each step below writes the user's `config.toml` through the settings table and applies the change
to the running daemon at once ([04](04-models-and-routing.md#setting-up-models-from-the-app)).

- **First run.** While no model is usable (no default, no table row with a configured endpoint,
  no runnable local model), the new-session composer and the Local models panel show three
  choices: *Add a model folder* (the native folder picker; the folder is scanned at once and the
  number of models found is reported), *Download from Hugging Face* (opens the Local models
  panel's download form), and *Connect an API endpoint* (a name, base URL, API key, and an
  optional model, which then becomes the default; the key goes to the system keychain,
  `models.add_endpoint`).
- **Model folders.** The Local models panel lists `models.folders`, each with a remove button
  (*Stop scanning this folder*; nothing in it is deleted), the download folder when it is separate,
  and *Add folder*. *Rescan* scans again.
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

## Model configuration dialog (implemented)

A modal (a native `<dialog>`, so Esc and the backdrop close it) edits model settings. It opens
from *Models…* in the settings menu, from *Configure* in the Local models panel (Local tab) and
from *Configure* in the Models panel (Model table tab). The Local tab lists the models in
`models.folders`; each form field is optional, shows the effective value as its placeholder and
has a *reset* that clears the saved value. Fields: context per session, KV pool tokens, slots, K
and V cache types, flash attention, max GPU layers, max wait, max output tokens (0 is uncapped)
and extra arguments (one per line). For a vLLM model the cache types, flash attention and GPU
layers are disabled. A debounced (500 ms) `models.set_config` dry run shows the memory the model
would need, its KV pool and its placement, or the server's error. The Model table tab edits
existing rows only (enabled, tier, tags, context window, prices; both prices or neither); it
does not add or remove rows. *Save* sends only the models that differ from what is saved, each
as a whole-settings update, then closes and refreshes the panels; a failure keeps the dialog
open with the message. *Reset model* clears all of a model's saved settings. A loaded model's
badge reads `loaded · <in use>/<slots> busy · pool <reserved>/<pool>`, and a disabled table row
is dimmed with an *off* badge.
