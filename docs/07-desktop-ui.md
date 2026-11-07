# 07 — Desktop interface

## Implemented in phase 2

- The Tauri 2 shell (`apps/desktop`) reads the daemon's endpoint file through a native command
  and the React interface connects over the WebSocket with the subprotocol token. In a plain
  browser (development preview) it asks for the URL and token instead.
- The default view below, built from columns of spanning or stacked panels with draggable
  dividers; sizes are remembered.
- Session panel: environment header (project, model, isolation tier, cost), conversation with
  assistant replies rendered as Markdown (GitHub-flavoured, raw HTML disabled; a toggle shows
  the raw text), inline permission prompts (allow, always this session, deny), composer, and
  context and outputs sections.
- Telemetry panel: per-call tokens, time to first token, tokens per second, and cost, with
  session totals.
- Display settings: UI scale, prose and code text scale, density, and line length. They are
  stored per viewer in local storage for now and move into the daemon's configuration with
  the settings UI.
- Light and dark themes follow the system.

Not yet: dragging panels and modules between columns, the edit-layout mode, detachable windows,
project layout overrides, mods, and themes beyond light and dark.

## Shell

Tauri 2 with a React + TypeScript + Vite frontend. The Tauri Rust side is thin: it launches or
attaches to `imperiumd`, handles native menus, notifications, file dialogs, deep links, and
the updater. All application logic goes through the daemon protocol, so the web UI could also
be served to a browser for remote use.

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
│ Projects           │ Session: <name>                                      │
│ (environments)     │ ┌──────────────────────────────────────────────────┐ │
│                    │ │ Environment · model · isolation tier · budget    │ │
│ stackable          │ ├──────────────────────────────────────────────────┤ │
├────────────────────┤ │ Conversation and plan                            │ │
│ Sessions           │ │                                                  │ │
│ (of the selected   │ ├────────────────┬─────────────────┬───────────────┤ │
│  project)          │ │ Context        │ Outputs         │ Worktrees     │ │
│                    │ │ breakdown      │ (artefacts)     │               │ │
│ stackable          │ └────────────────┴─────────────────┴───────────────┘ │
│                    │ spans full height                                    │
└────────────────────┴──────────────────────────────────────────────────────┘
```

- **Projects panel** (top left): projects / environments, their location (local, container,
  SSH host), project-agent status, and schedules.
- **Sessions panel** (bottom left): sessions in the selected project, with status, cost, and
  pending approvals.
- **Main session panel** (middle, full height): the selected session. It is built from
  sections — environment header, conversation and plan, context breakdown (tokens by system
  prompt, policy, tools, files, and history against the window size), outputs, and worktrees —
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

## Accessibility

Full keyboard navigation, visible focus, screen-reader labels, reduced-motion support, and
WCAG AA contrast for shipped themes.
