# 07 — Desktop interface

## Shell

Tauri 2 with a React + TypeScript + Vite frontend. The Tauri Rust side is thin: it launches or
attaches to `imperiumd`, handles native menus, notifications, file dialogs, deep links, and
the updater. All application logic goes through the daemon protocol, so the web UI could also
be served to a browser for remote use.

## Layout

- **Tiling workspace:** panes split horizontally and vertically, tabbed, resizable, detachable
  into separate windows. Layouts are saved per workspace and shareable as presets.
- Default panes: sessions, conversation, agent tree, task inspector, files, terminal, browser,
  models and hardware, telemetry, settings. Mods can add panes ([06](06-extensibility.md#ui-mods)).
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
