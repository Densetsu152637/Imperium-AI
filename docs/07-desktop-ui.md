# 07 — Desktop interface

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

- **Tiling workspace:** panels split horizontally and vertically, tabbed, resizable, and
  detachable into separate windows. Layouts are saved per project and shareable as presets
  (a preset includes panel instances and their settings, and lists the mods it needs).
- Mods contribute new panels and slot content through [UI mods](06-extensibility.md#ui-mods).
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
