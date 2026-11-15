# Plan: desktop interface polish

Revision: 2026-11-16, measured on `experimental` at `feddfc9e` (last updated). Roadmap
[item 47](../12-roadmap.md). Status: planned; no stage started.

The user asked for the desktop app to have "as much polish as the claude interface", with UI
components that are "simple but well stylised". This plan turns that into a token system, a
small set of shared components, and fifteen pull requests that restyle the app surface by
surface. The quality bar is Claude's interface: calm, simple components, careful type and
spacing, restrained colour, smooth states. The look stays Imperium's own (see
[identity](#identity-not-a-copy)).

## Goals

Each goal is boolean and has a check. "Guard" means the tests added in
[stage 1](#stage-1-stylesheet-split-and-guard-tests).

1. **One token system.** Every colour, font size, radius, shadow, duration and z-index in the
   app's CSS comes from `styles/tokens.css`. Check: the style guard passes with every file's
   budget at zero, and its budget header is removed in [stage 15](#stage-15-motion-and-accessibility-pass).
2. **Shared components.** Buttons, icon buttons, fields, selects, switches, tabs, menus,
   dialogs, tables, cards, badges, empty, loading and notice states each have one
   implementation, used everywhere. Check: no native `<select>` in `apps/desktop/src` (guard);
   `rg 'className="(empty|error)"' apps/desktop/src` returns nothing; reviewers find no second
   implementation of a listed component.
3. **Known defects fixed.** The eight defects in [known defects](#known-defects) are gone.
   Check: each has a named check in the stage that fixes it.
4. **Works at every width.** The app is usable at 1280, 1024 and 420 px wide, in light and
   dark, by keyboard alone. Check: the [visual checklist](#visual-checklist) passes for every
   stage.
5. **Readable and accessible.** Body and secondary text meet WCAG AA (4.5:1), the focus ring
   and control borders meet 3:1, in both themes and for every accent. Check: the contrast test
   from [stage 2](#stage-2-tokens-and-base-elements).
6. **No behaviour change.** No feature, protocol or Rust change. Check: the diff of every
   stage touches only the paths it owns, and `bash scripts/check-web.sh` passes.

## Scope and non-goals

In scope: `apps/desktop/src` (TSX and CSS), `apps/desktop/public/boot.css`, the desktop tests in
`apps/desktop/test`, the mod frame (`apps/desktop/src/mods/ModFrame.tsx`) and the bundled
nursery mod's view code (`mods/imperium-nursery/src`), and the desktop UI document
([07](../07-desktop-ui.md)).

Non-goals:

- **No feature changes.** No new panels, settings, commands or flows. The one new interaction is
  how the window behaves below 600 px wide ([stage 14](#stage-14-narrow-layout)), which today
  has no layout at all. A toast system is not added: nothing in the app needs one today.
- **No protocol changes.** `PROTOCOL_VERSION` and `packages/protocol` stay as they are.
- **No Rust changes.** Nothing in `apps/desktop/src-tauri` or `crates/` changes; no native menu
  accelerators (that is why the Ctrl+, fix is in the web view).
- **No UI framework.** Plain React and CSS custom properties, as today (see
  [decision D1](#decisions)). The only possible new dependency is a bundled font
  ([question Q3](#open-questions-for-the-user)).
- **No change to the mod theme contract.** Mods receive `--bg`, `--panel`, `--fg`, `--muted`,
  `--border`, `--accent`, `--ok`, `--danger`, `--space` and `--radius` (`THEME_TOKENS` in
  `ModFrame.tsx`; [06](../06-extensibility.md#ui-mods) says theme tokens are copied into the
  frame); those names stay and keep their meaning. Stage 13 may add names, never remove one.

## Assumptions

- The visual review that found the defects ran at the default UI scale (100 %), comfortable
  density, on Windows. Pixel values below are at that scale: `html` is 15 px, so 1 rem = 15 px.
- Implementers can run the app with a daemon and take screenshots; this plan was written from
  the code only (no browser was used), so the defect causes marked *suspected* need confirming
  first.
- Stages land one pull request each against `experimental`, in the order below, with the
  parallel windows noted.

## Relevant paths

| Path | What it is |
| --- | --- |
| `apps/desktop/src/styles.css` | 7,176 lines; every rule in the app except the model dialog's |
| `apps/desktop/src/models/modelConfig.css` | 255 lines; the model configuration dialog |
| `apps/desktop/src/components/` | Today's shared pieces: `Select`, `Switch`, `RowMenu`, `SettingsModal`, `SettingsRow` |
| `apps/desktop/src/settings/settings.ts` | Writes theme, accent, scales and fonts to the root element |
| `apps/desktop/src/mods/ModFrame.tsx` | Mod frames and the theme tokens passed into them |
| `apps/desktop/src/layout/Columns.tsx` | Columns and stacks, sized as proportions with no minimum |
| `apps/desktop/test/cssBalance.test.ts` | Existing guard: balanced braces in every `.css` under `src` |
| `mods/imperium-nursery/src/view.ts`, `panels.ts`, `space.ts` | The nursery's room, controls and placement |

## Related docs

- [07 Desktop interface](../07-desktop-ui.md): Theming, Quiet interface, Scaling, Accessibility,
  Source layout. On a conflict this plan wins while it is open; each stage updates the section of
  07 it changes, and stage 15 folds the rest in.
- [06 Extensibility](../06-extensibility.md): the mod theme tokens. 06 wins on the mod contract.
- [Cleanup plan](cleanup.md): the format and the "no compatibility shims" rule this plan follows.

## Audit

Figures come from a scripted count over `styles.css` and `modelConfig.css` (comments removed;
token definitions excluded where noted) and over every `.tsx` file in `apps/desktop/src`.
Measure again when stage 1 starts.

### Tokens today

`:root` defines 74 custom properties: surfaces (`--bg`, `--surface`, `--surface-2`, `--surface-3`,
`--bubble`, `--code-bg`), metal gradients (`--metal`, `--metal-bar`, `--metal-card`,
`--chrome-text`), borders, text, one accent with strong, soft and ink variants, `--silver`,
`--danger`, `--ok`, `--warn`, fifteen context-chart colours, two fonts, `--space`, four radii,
control heights and paddings, two icon sizes, three type steps (`--fs-xs`, `--fs-sm`,
`--fs-md`), one easing and two durations. Light values override them under
`:root[data-theme="light"]`. The scales the user controls (`--ui-scale`, `--prose-scale`,
`--code-scale`, `--space-scale`) are a good base and stay.

The tokens exist, but most rules do not use them:

| Property | Declarations | Distinct values | Through a token |
| --- | --- | --- | --- |
| Colour literals (whole file) | n/a | 71 hex, 30 `rgb()`, 26 `color-mix()` | 16 hex and 17 `rgb()` sit outside token definitions |
| `font-size` | 222 | 35 (0.68 rem to 1.35 em) | 11 of 222 use `--fs-*` |
| `font-weight` | 52 | 5 (400, 500, 600, 700, normal) | none |
| `letter-spacing` | n/a | 8 | none |
| `border-radius` | 112 | 19 (1 px to 999 px, `50%`, three `calc()` multiples) | 62 of 112 |
| `box-shadow` | 67 | 21 | 12 are `none`, 10 are literal `rgb()` shadows, the rest mix tokens |
| `padding`, `margin`, `gap` | 675 | 73 value tokens (0.05 rem to 1.5 rem, and `calc(var(--space) * n)` with 10 multipliers) | 170 `calc(var(--space)…)` |
| Raw `px` lengths | 237 | about 17 | n/a |
| Control heights | 76 `height`/`min-height` in rem | 35 | a handful use `--control-h` |
| `opacity` | n/a | 11 (0.35 to 1) | none |
| `transition` | 26 | 19 | most use `--fast`/`--slow`; 4 carry a literal 120 ms (some as fallbacks) |
| `z-index` | 13 | 9 (1 to 2000) | none |

Other findings:

- **Rules repeat.** 54 selectors are defined more than once (`.settings-nav-item` at lines 4461
  and 5190, `.turn-summary` and `.error` three times each, `button`, `.prose`, the
  `.message.assistant` headings), because the file grew by appending: late blocks such as
  "Settings polish" (line 5189) override earlier ones. Session rules alone sit in at least six
  separate regions of the file.
- **Four custom properties are read but never defined in CSS:** `--surface-1` (a bug; it falls
  back to `transparent`), plus `--branch-depth`, `--swatch` and `--font-prose`, which are set from
  TSX and are fine.
- **Metal look.** 31 rules use the metal gradients and `--chrome-text`; together with
  `--highlight` (an inset top light on every button) they give the "brushed graphite" look that
  makes surfaces busier than the reference.
- **The font is not shipped.** `--font-ui` starts with Inter, but no font file is bundled, so
  the app renders in Inter only where the user installed it, and in Segoe UI, San Francisco or
  Roboto elsewhere. Spacing tuned on one machine is off on another.
- **The light accent is overridden (confirmed in code).** `applySettings` writes the chosen
  accent's dark value to the root element's inline style, which beats
  `:root[data-theme="light"] { --accent: #005a72 }`. In light mode the default accent is
  therefore `#1fb2c8` with white ink: 2.54:1 on primary buttons and 2.33:1 as text on the
  background, both below AA.
- **Focus is drawn three ways:** `:focus-visible` outlines (30 rules), a 3 px `--accent-soft`
  halo (`box-shadow`, 10 rules), and `outline: none` with no replacement (11 rules, some
  intended).
- **Responsive rules are few:** two `@media (max-width: 720px)` blocks (Settings and
  Connectors), seven `@container` width queries, five `(hover: none)` and one `(forced-colors: active)`.
  Columns have no minimum width.

### Components today

There is no component library; most markup is ad hoc, styled by global element rules and
classes.

| Pattern | Today | Problem |
| --- | --- | --- |
| Button | 246 `<button>` in 67 files; global `button` rule plus `.primary` (44), `.ghost` (87), `.small` (31), `.icon-only` (40), `.danger` (10) | Works, but sizes and paddings are overridden per surface; icon-only buttons rely on native `title` tooltips |
| Tooltip | 195 `title=` attributes in 52 files; `info-tip` in `models/ConfigParts.tsx` | Native tooltips: slow, unstyled, not shown on focus |
| Input, textarea | 67 `<input>`, 14 `<textarea>`; the "Fields" block (line 4969) plus per-surface overrides (`.settings-row-control input[type="number"]` is defined three times) | No shared label, hint and error layout |
| Select | `components/Select.tsx` (28 uses in 17 files) and 6 native `<select>` left (`BenchmarkSection`, `LocalForm`, `ModelConfigDialog`, `TableForm`, `ReviewPanel` twice) | Native selects ignore the theme (the same cause as the nursery's dark select) |
| Switch | `components/Switch.tsx` (13 uses) | Fine; restyle only |
| Menu | `components/RowMenu.tsx` (10 uses) | Fine; the base for a general menu |
| Tabs | Hand-built `role="tablist"` twice (Connectors views, Browser tabs) | Two implementations |
| Dialog | `components/SettingsModal.tsx` (Settings, environments, model configuration); Connectors builds its own on the same `.settings-backdrop`; the command palette and the tour have their own | Three shells, one focus-trap behaviour |
| Table | 11 `<table>` in 8 files, each with its own class (`.model-table`, `.bench-table`, `.context-table`, `.local-model-table`, `.telemetry`, `.models`, `.shortcuts`) | Seven table styles |
| Card | `.connector-card`, `.budget-card`, `.tour-card`, `.house-models`, model suggestion card | Five card styles |
| Badge | `.badge` (22 uses, with `running`, `finished`, `failed` defined twice), `StatusBadge`, `folder-badge`, `source-badge` | Repeated variants |
| Empty state | `className="empty"` 29 times in 10 files, plus `connectors-empty`, `settings-nav-empty`, `command-palette-empty`, `tile-grid-empty` | Plain muted text; no shared layout |
| Loading | "Loading…" paragraphs in 10 files; spinners in `ToolState`, `WaitLine`, `AttachmentChips`, `FormParts`, `InstalledView` | No shared loading state |
| Error notice | `className="error"` 50 times | Bare red text; no icon, no consistent placement |
| Inline styles | 13 `style={{…}}` in 12 files | Mostly legitimate (sizes from state); keep those |

### Known defects

From the recent visual review, with the cause found in code (*suspected* where it needs the
running app to confirm):

| # | Defect | Cause | Fixed in |
| --- | --- | --- | --- |
| K1 | Settings navigation items and icons look ragged | `.settings-nav-item` is styled twice (line 4461: block padding, 0.88 rem; line 5190: flex, gap 0.6 rem, 1 rem icons), items with badges use a column layout (`.settings-nav-item-text`), and group labels use a different inset (0.2 rem 0.6 rem) from items; nothing fixes the row height | [Stage 3](#stage-3-shared-components) (the dialog navigation) |
| K2 | The nursery panel in the sidebar is about 140 px wide, shows both scrollbars, and has a dark select in the light theme | The frame sets `color-scheme: light dark` (`ModFrame.tsx` line 119), so native controls follow the operating system, not the app theme; the frame body has 8 px padding and the room a 160 px minimum height plus a computed `roomHeight`, so the room overflows the frame both ways at that width | [Stage 13](#stage-13-nursery-and-mod-frames) |
| K3 | Right-hand panels clip inputs at 1024 px | *Suspected:* fixed minimum widths on controls (`min-width: 10rem` on `.models-toolbar input`, `.local-model-table .model-name`, `.hf-download select` and `.row-menu`; `9rem` on `.new-session-subfolder`) inside columns that shrink proportionally with `overflow: hidden`; which panels the review saw needs confirming | [Stage 8](#stage-8-model-configuration-and-model-panels) and [stage 10](#stage-10-side-panels) (panel content), [stage 14](#stage-14-narrow-layout) (column minimums) |
| K4 | No narrow layout (600 px and below); three columns crush | Column widths are proportions (`Columns.tsx`, `flex-grow` share with `flex-basis: 0`) with no minimum and no breakpoint | [Stage 14](#stage-14-narrow-layout) |
| K5 | Ctrl+, works only after clicking the page | The handler is a `window` keydown listener (`shared/useWindows.ts`). *Suspected:* the web view has no keyboard focus at launch until clicked, and keys pressed while a sandboxed mod frame has focus never reach the parent window | [Stage 5](#stage-5-shell-sidebar-and-navigation) (launch focus), [stage 13](#stage-13-nursery-and-mod-frames) (keys from mod frames) |
| K6 | Shortcut text sits tight against its Change button | `.shortcuts td` has no column gap or fixed control column | [Stage 6](#stage-6-settings) |
| K7 | Pets and labels overlap in the nursery room | *Suspected:* placement in `space.ts` spaces pets by sprite size, while each pet's box is `LABEL_W` wide plus a 14 px label line under it | [Stage 13](#stage-13-nursery-and-mod-frames) |
| K8 | Light theme accent fails contrast | See [tokens today](#tokens-today): the dark accent is written inline over the light one | [Stage 2](#stage-2-tokens-and-base-elements) |

## Design direction

### Principles

1. **One accent.** The accent marks the primary action, the current selection, focus and links,
   and nothing else. Status uses the semantic colours; everything else is neutral.
2. **Quiet borders.** Hairline borders at low contrast separate regions; elevation (a shadow)
   is reserved for things that float: menus, popovers, dialogs. No gradients on surfaces, no
   inset highlights on buttons.
3. **Generous whitespace, few sizes.** Spacing comes from one 4 px-based scale; text from six
   sizes and three weights. When in doubt, more space and a smaller, lighter label.
4. **Consistent rows.** Every list row (sessions, environments, settings navigation, menus,
   model lists) has the same height for its density; controls in a row align to its centre.
5. **Icons follow text.** Lucide line icons at 16 px beside 13 px text, 14 px in dense rows,
   20 px only in empty states and the title bar; the stroke width stays the single value set in
   `main.tsx`.
6. **Every view has its three states.** Empty, loading and error each use the shared
   component, with a one-line explanation and at most one action.
7. **Motion explains, never decorates.** Things that appear fade and rise a few pixels; nothing
   bounces; reduced motion removes movement and keeps fades instant.

### Identity, not a copy

Imperium keeps its own look: cool graphite neutrals in dark mode, cool near-white in light,
and its cyan accent. It does not use Anthropic's typefaces, logo, warm ivory backgrounds,
terracotta accent or serif headings. What is borrowed is the discipline: few sizes, calm
colour, roomy spacing, soft states.

### Tokens

All tokens live in `apps/desktop/src/styles/tokens.css`. Lengths are in rem so `--ui-scale`
zooms them; spacing, row and control heights also multiply by `--space-scale` (density). Pixel
values in comments are at 100 %.

**Type scale.** Six sizes, three weights, three line heights. Prose and code keep their own
user scales.

```css
--font-ui: "Inter Variable", "Segoe UI Variable Text", "Segoe UI", system-ui, -apple-system, sans-serif;
--font-code: "Cascadia Code", "JetBrains Mono", ui-monospace, Menlo, Consolas, monospace;
--fs-xs: 0.75rem;     /* 11.25px  badges, captions, keyboard hints */
--fs-sm: 0.8125rem;   /* 12.2px   secondary text, meta, table cells */
--fs-md: 0.875rem;    /* 13.1px   controls, rows, labels: the UI default */
--fs-base: 1rem;      /* 15px     transcript prose (times --prose-scale) */
--fs-lg: 1.125rem;    /* 16.9px   section and panel titles */
--fs-xl: 1.375rem;    /* 20.6px   dialog titles, empty-state headings */
--fw-regular: 400;
--fw-medium: 500;
--fw-semibold: 600;
--lh-tight: 1.25;     /* titles, badges */
--lh-ui: 1.4;         /* controls, rows */
--lh-prose: 1.6;      /* transcript */
--tracking-caps: 0.06em; /* the only letter-spacing: small uppercase group labels */
```

Today's 35 sizes map as: 0.68 to 0.78 rem → `--fs-xs`; 0.8 to 0.83 → `--fs-sm`; 0.85 to 0.92 →
`--fs-md`; 0.95 to 1.05 → `--fs-base`; 1.1 to 1.25 → `--fs-lg`; 1.3 and up → `--fs-xl`. Sizes
in `em` stay only for icons and inline code that must follow their parent.

**Spacing scale.** Multiples of 4 px; `--space` stays as an alias of `--sp-2` for mods.

```css
--sp-1: calc(0.25rem * var(--space-scale));  /* 4px  icon-to-label inside a chip */
--sp-2: calc(0.5rem * var(--space-scale));   /* 8px  between controls in a row */
--sp-3: calc(0.75rem * var(--space-scale));  /* 12px control padding, row padding */
--sp-4: calc(1rem * var(--space-scale));     /* 16px panel padding, between groups */
--sp-5: calc(1.5rem * var(--space-scale));   /* 24px dialog padding, between sections */
--sp-6: calc(2rem * var(--space-scale));     /* 32px empty-state padding */
--sp-8: calc(3rem * var(--space-scale));     /* 48px page margins in wide dialogs */
--space: var(--sp-2);
```

**Sizes.** Controls and rows share heights so a row and its controls line up.

```css
--control-h-sm: calc(1.625rem * var(--space-scale)); /* 24px: row tools, badges with actions */
--control-h: calc(2rem * var(--space-scale));        /* 30px: buttons, inputs, selects */
--control-h-lg: calc(2.5rem * var(--space-scale));   /* 37.5px: composer send, first-run choices */
--row-h: calc(2rem * var(--space-scale));            /* list rows, menu items, navigation items */
--icon-sm: 0.875rem;  /* 13px */
--icon: 1.0625rem;    /* 16px */
--icon-lg: 1.25rem;   /* 19px */
```

**Radii.** Four steps and a pill; `--radius` keeps its name for mods.

```css
--radius-xs: 4px;     /* badges, keyboard keys, inline code */
--radius-sm: 6px;     /* buttons, inputs, selects, menu items */
--radius: 8px;        /* cards, rows, menus, popovers, message bubbles */
--radius-lg: 12px;    /* dialogs, the composer */
--radius-full: 999px; /* pills, switches, avatars */
```

**Elevation.** Three levels, used only by floating things. Dark mode also lightens the surface
one step per level, because shadows barely show on dark backgrounds.

```css
--elev-1: 0 1px 2px rgb(16 24 40 / 0.06);                                    /* light: raised card */
--elev-2: 0 4px 12px rgb(16 24 40 / 0.08), 0 1px 3px rgb(16 24 40 / 0.06);   /* menus, popovers */
--elev-3: 0 16px 40px rgb(16 24 40 / 0.14), 0 2px 8px rgb(16 24 40 / 0.06);  /* dialogs */
/* dark: same geometry, alpha 0.30 / 0.40 + 0.25 / 0.55 + 0.30 */
--scrim: rgb(10 12 15 / 0.45); /* dark: rgb(0 0 0 / 0.55); no backdrop blur */
--z-raised: 1; --z-sticky: 10; --z-overlay: 1000; --z-popover: 1100; --z-tour: 2000;
```

**Motion.**

```css
--dur-fast: 120ms;   /* hover, press, colour */
--dur-base: 180ms;   /* menus, popovers, folding a panel */
--dur-slow: 260ms;   /* dialogs, sheets, the narrow sidebar */
--ease-standard: cubic-bezier(0.2, 0, 0, 1);
--ease-exit: cubic-bezier(0.4, 0, 1, 1);
/* aliases kept: --fast: var(--dur-fast); --slow: var(--dur-base); --ease: var(--ease-standard) */
```

Entering: fade plus a 4 px rise over `--dur-base`; leaving: fade over `--dur-fast` with
`--ease-exit`. Under reduced motion (the operating system's or the app's `reduce-motion`
class) movement is removed and durations drop to zero, as the existing rule does.

**Colours.** Semantic names; components never use a raw value. Contrast ratios were computed
for these values (WCAG relative luminance) and are checked again by the stage 2 test.

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `#0e1013` | `#f6f7f8` | Window background, behind panels |
| `--surface` (`--panel` alias) | `#14171a` | `#ffffff` | Panels, dialogs |
| `--surface-raised` | `#1a1d21` | `#ffffff` | Cards, buttons, menus |
| `--surface-sunken` | `#0b0d0f` | `#f1f2f4` | Inputs, code blocks, the composer well |
| `--surface-hover` | `rgb(255 255 255 / 0.04)` | `rgb(17 24 39 / 0.04)` | Row and ghost-button hover |
| `--surface-active` | `rgb(255 255 255 / 0.07)` | `rgb(17 24 39 / 0.07)` | Pressed, selected rows |
| `--border` | `rgb(255 255 255 / 0.08)` | `rgb(17 24 39 / 0.09)` | Dividers, cards, inputs at rest |
| `--border-strong` | `rgb(255 255 255 / 0.14)` | `rgb(17 24 39 / 0.16)` | Hovered inputs, table heads |
| `--text` (`--fg` alias) | `#e7e9ec` (15.7:1 on bg) | `#16181b` (16.6:1) | Body text |
| `--text-muted` (`--muted` alias) | `#a0a6ae` (7.8:1) | `#545a63` (6.5:1) | Secondary text, labels |
| `--text-subtle` | `#7f8690` (5.2:1) | `#646a73` (about 5:1) | Timestamps, placeholders; never on `--surface-sunken` in light |
| `--accent` | `#1fb2c8` | `#0a7186` | Primary fills, selection, links |
| `--accent-hover` | `#45c6d8` | `#086477` | Primary hover |
| `--accent-soft` | accent at 14 % | accent at 10 % | Selected rows, active navigation |
| `--accent-ink` | `#03161a` (7.3:1 on accent) | `#ffffff` (5.7:1 on accent) | Text on accent fills |
| `--accent-text` | `#5fd0e0` (10.5:1) | `#0a7186` (5.3:1) | Accent used as text or icon |
| `--focus` | `#5fd0e0` | `#0a7186` | Focus ring (both above 3:1) |
| `--danger` / `--danger-soft` | `#f2766b` / 14 % | `#b42318` / 10 % | Errors, destructive actions |
| `--ok` / `--ok-soft` | `#58c98a` / 14 % | `#18794e` / 10 % | Success, running |
| `--warn` / `--warn-soft` | `#e3b25a` / 14 % | `#8a5a00` / 10 % | Warnings, budgets |

The context-chart colours (`--ctx-*`) stay as a separate data-visualisation set, exempt from
the one-accent rule. `--silver`, `--bubble`, `--code-bg` and `--ink` become aliases of the
tokens above and are removed once nothing reads them.

**Accents per theme.** Each of the twelve accents in `settings.ts` gets a dark and a light
value (light values darkened until white ink reaches 4.5:1). `applySettings` sets
`data-accent` on the root instead of writing colours inline, and `tokens.css` defines each
accent under `:root[data-theme…][data-accent…]`, so the light theme's values win (K8).

**Focus ring.** One rule for every focusable element:
`outline: 2px solid var(--focus); outline-offset: 2px` on `:focus-visible`, offset 0 inside
inputs (where the border also takes `--focus`). The `--accent-soft` halos go. Outlines show in
forced-colours mode, which halos do not.

## Shared components

Plain React components in `apps/desktop/src/components/` with their CSS in
`styles/primitives.css`. Each wraps native elements, keeps WAI-ARIA patterns the app already
uses, and takes `className` for layout only (never colour or size).

| Component | Variants and API | Where it is used today | Migration |
| --- | --- | --- | --- |
| Button | Stays a native `<button>` styled by element and class: `primary`, `ghost`, `danger`, default (secondary); sizes `small`, default, `large` | 246 buttons in 67 files | The class API already exists; stage 2 restyles it, surface stages remove per-surface overrides |
| IconButton | `<IconButton icon label tooltip? size?>`; `label` is required and becomes `aria-label` and the tooltip | 40 `.icon-only`, panel and title-bar tools, row tools | Surface stages replace `.icon-only` markup |
| Tooltip | Shown on hover after 400 ms and at once on keyboard focus; `role="tooltip"` with `aria-describedby`; positioned by `shared/floating.ts` | 195 native `title=`; `info-tip` in `ConfigParts.tsx` | Used by IconButton and `info-tip`; other `title=` attributes stay (truncated text) |
| Field | `<Field label hint error>` around an input, textarea or Select; Input and Textarea are native elements styled globally | 67 inputs, 14 textareas, `FieldRow`, `FormParts` | Surface stages wrap labelled controls; `FormParts` and `FieldRow` become thin users |
| Select | Existing `components/Select.tsx`, restyled, plus `size="small"` | 28 uses; 6 native selects | The native ones move to Select in their surface stage; the guard then forbids `<select` |
| Switch | Existing `components/Switch.tsx`, restyled | 13 uses | None |
| Checkbox | Native checkbox with `accent-color: var(--accent)` and the focus ring; no component | Permission and connector forms | CSS only |
| Tabs | `<Tabs items value onChange>` following the WAI-ARIA tabs pattern (arrow keys, Home/End) | Connectors views, Browser tabs | Both move to it in their stages |
| Menu | `RowMenu` generalised: a trigger plus items with icon, label, shortcut, separator and danger item | `RowMenu` (10 uses), the Notifications *Play* menu | `RowMenu` is renamed and its callers updated in stage 3 |
| Dialog | `SettingsModal` generalised: `Dialog` (header, body, footer) with an optional `DialogNav` column; focus trap, Esc, scrim click; full-screen sheet below 600 px | Settings, environments, model configuration (`SettingsModal`); Connectors (own shell); command palette | Stage 3 renames it and updates its three importers; Connectors moves in stage 7, the palette in stage 5. The tour keeps its own card (it is not modal) |
| Table | CSS class `.table` (header, row hover, numeric columns right-aligned with tabular figures, compact variant); no component | 11 tables, 7 classes | Surface stages swap their table class |
| Card | `.card` CSS class: surface-raised, border, `--radius`, `--sp-4` padding; `.card.interactive` for clickable cards | Connector, budget, tour, house-models and suggestion cards | Surface stages |
| Badge | `<Badge tone>`: neutral, accent, ok, warn, danger; text at `--fs-xs` on the soft colour | `.badge` (22), `StatusBadge`, folder and source badges | Stage 3 builds it and moves `StatusBadge`; surfaces move the rest |
| EmptyState | `<EmptyState icon title body? action?>`, centred, `--sp-6` padding | 29 `.empty` and four other empty classes | Surface stages |
| LoadingState | `<LoadingState label>`: a spinner shown after 300 ms (no flash for fast loads), with text; `inline` variant for rows | "Loading…" in 10 files; five spinners | Surface stages; the spinners share one `Spinner` |
| Notice | `<Notice tone title? action?>` for inline errors, warnings and successes, with an icon | 50 `.error` paragraphs, the preset notice in `App.tsx`, `ConnectionBanner` | Surface stages |
| Kbd | `<Kbd>` for a key chord, at `--fs-xs` on `--surface-sunken` | Shortcuts table, command palette hints, tooltips | Stages 5 and 6 |

Not added: **Toast** (no part of the app shows transient messages today; adding them would be
a feature) and **Skeleton** (lists load from the local daemon fast enough that a delayed
spinner is calmer than placeholder shapes; revisit if a list is measured loading for more than
a second).

## Visual checklist

Every stage's pull request carries screenshots of the surfaces it changed, and the reviewer
checks each item below. A stage adds its own items.

- [ ] Window 1280 × 800 and 1024 × 768: nothing clips, overlaps or wraps badly.
- [ ] Window 420 × 800 (from stage 14 on; before it, the surface's own column at 320 px wide).
- [ ] Light and dark theme, default accent and one other accent.
- [ ] Keyboard only: every control in the changed surface is reachable with Tab, shows the
      focus ring, and works with Enter, Space, arrows and Esc as before.
- [ ] Density compact and spacious, UI scale 150 %: rows stay aligned, nothing overflows.
- [ ] Empty, loading and error states of the surface (force them where needed).
- [ ] Reduced motion on: nothing moves; fades are instant.

## Guard tests

Cheap, pure Node tests in `apps/desktop/test`, run by `pnpm --filter @imperium/desktop test`
and in CI by `scripts/check-web.sh`:

1. **`cssBalance.test.ts`** (exists): keeps covering every `.css` under `src`, including the new
   `styles/` folder, unchanged.
2. **`styleTokens.test.ts`** (stage 1): for every `.css` file except `styles/tokens.css`, counts
   colour literals (hex, `rgb()`, `hsl()`, named colours other than `transparent`,
   `currentColor` and `inherit`), `font-size` values that are not `var(--fs-*)`, `inherit`, or an
   `em` or `%` value, `font-weight` numbers, `border-radius` and `box-shadow` values without a
   token, literal durations in `transition` and `animation`, and literal `z-index` values. Each
   file declares its allowance in a first-line comment, `/* style-budget: colour=3 font-size=12
   … */`; the test fails if a count exceeds the budget **or falls below it** (so every
   improvement lowers the budget in the same pull request). Keeping the budget in the file
   itself means parallel stages never edit a shared baseline. Stage 15 removes the comments
   and the test then requires zero everywhere.
3. **Undefined tokens** (same file): every `var(--name)` read in CSS is defined in CSS, or set
   from TSX and listed (`--branch-depth`, `--swatch`, `--font-prose`, `--tile`), or has a
   fallback that is itself a token. Catches the `--surface-1` kind of bug.
4. **No native select** (same file, from stage 13's merge, the last native select's stage): no
   `<select` in `apps/desktop/src/**/*.tsx`.
5. **`tokenContrast.test.ts`** (stage 2): parses `styles/tokens.css`, and for both themes and
   all twelve accents checks text, muted and subtle text on `--bg` and `--surface` (4.5:1),
   accent ink on accent (4.5:1), and the focus ring and `--border-strong` on `--bg` (3:1).
6. **Icon buttons are named** (stage 3): `IconButton`'s `label` prop is required by its type, so
   `pnpm typecheck` is the guard; no runtime test.

## Stages

Stages 1 to 3 run in order. After stage 3 merges, stages 4 to 13 own disjoint files and any two
(or more) can run in parallel; stage 14 needs stage 5, and stage 15 runs last. Every stage:
updates its files' style budgets, passes `pnpm --filter @imperium/desktop test` and
`pnpm --filter @imperium/desktop typecheck` locally and `bash scripts/check-web.sh` in CI,
passes the [visual checklist](#visual-checklist), and touches no path outside its owned paths
(a needed change elsewhere goes in its handoff notes for the owning stage).

"Owned CSS" means the file in `apps/desktop/src/styles/` created by stage 1 for that surface.

### Stage 1: stylesheet split and guard tests

- **Objective:** split `styles.css` into one file per surface with no visible change, and add
  the guard tests at today's counts.
- **Depends on:** none.
- **Owned paths:** `apps/desktop/src/styles.css` (removed), `apps/desktop/src/styles/` (new:
  `tokens.css`, `base.css`, `primitives.css`, `shell.css`, `sidebar.css`, `palette.css`,
  `session.css`, `settings.css`, `connectors.css`, `models.css`, `environments.css`,
  `panels.css`, `tour.css`, `house-models.css`, `mods.css`, `index.css` importing them in
  order), `apps/desktop/src/main.tsx` (the import), `apps/desktop/test/styleTokens.test.ts`,
  docs [07](../07-desktop-ui.md) Source layout.
- **What visibly changes:** nothing.
- **How:** move rules, do not edit them. First merge each repeated selector into its last
  occurrence (where it already wins). Then move each rule to its surface file. A rule moved
  past another rule is a risk only when both have the same specificity and set a property in
  common; a scratch check (kept out of the repository; its output goes in the pull request)
  lists every such pair whose order changed, and each pair is either shown not to match the
  same element or kept in order by placing the rule in the file that preserves it.
  `modelConfig.css` stays where it is (it is imported by its dialog).
- **Acceptance checks:** the app's computed styles are unchanged: before and after screenshots
  of each surface in both themes are pixel-identical (or the differences are listed and
  explained); the order-check output lists no unresolved pair; `styleTokens.test.ts` passes with
  budgets equal to today's counts; `cssBalance.test.ts` passes.
- **Risks:** cascade order changes (mitigated above); a missed rule (the screenshot diff catches
  it).
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** keep each file's sections in their original relative order. Put rules used
  by several surfaces (`.muted`, `.error`, `.empty`, `.badge`) in `primitives.css`.

### Stage 2: tokens and base elements

- **Objective:** the token system above, applied to element rules, with today's token names kept
  as aliases.
- **Depends on:** stage 1.
- **Owned paths:** `styles/tokens.css`, `styles/base.css`, `apps/desktop/src/settings/settings.ts`
  (accent per theme, `data-accent`), `apps/desktop/public/boot.css` (loading screen colours),
  `apps/desktop/test/tokenContrast.test.ts`, the font files and `package.json` if
  [Q3](#open-questions-for-the-user) chooses a bundled font, docs 07 Theming.
- **What visibly changes:** everywhere at once: flat surfaces instead of metal gradients
  (except the title-bar wordmark, per [Q1](#open-questions-for-the-user)), quieter borders, the
  new neutrals, one focus ring, restyled buttons, inputs, checkboxes and scrollbars, and a light
  theme whose accent passes contrast (K8).
- **Acceptance checks:** `tokenContrast.test.ts` passes for both themes and all twelve accents;
  `rg "metal|chrome-text" apps/desktop/src/styles` finds only the wordmark rule; the theme
  switch in the command palette still flips every surface, and the loading screen matches the
  theme before first paint.
- **Risks:** this is the largest visible change and lands before surfaces are tidied, so some
  surface overrides will look off until their stage. Accept it, or hold stages 2 to 13 on a
  long-lived integration branch and merge to `experimental` together (see
  [Q5](#open-questions-for-the-user)). Mods see new token values under the same names.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** keep `--fast`, `--slow`, `--ease`, `--space`, `--radius`, `--panel`, `--fg`,
  `--muted` as aliases. Stage-specific checks: Settings → Appearance accent swatches, theme
  System following the operating system.

### Stage 3: shared components

- **Objective:** build the [shared components](#shared-components) and move their existing
  implementations onto them.
- **Depends on:** stage 2.
- **Owned paths:** `apps/desktop/src/components/` (new: `IconButton.tsx`, `Tooltip.tsx`,
  `Field.tsx`, `Tabs.tsx`, `Menu.tsx` (from `RowMenu.tsx`), `Dialog.tsx` (from
  `SettingsModal.tsx`), `Badge.tsx`, `EmptyState.tsx`, `LoadingState.tsx`, `Notice.tsx`,
  `Kbd.tsx`; restyled `Select.tsx`, `Switch.tsx`), `apps/desktop/src/shared/floating.ts`
  (positioning taken from `Select.tsx`), `apps/desktop/src/panels/StatusBadge.tsx`,
  `styles/primitives.css`, and only the import lines and renamed tags of `RowMenu` and
  `SettingsModal` callers.
- **What visibly changes:** dialogs (Settings, environments, model configuration) get the new
  shell and a navigation column with fixed row height, one inset for group labels and items, and
  icons in a fixed 16 px column, which fixes K1; menus and selects share one popover look;
  badges and switches restyle.
- **Acceptance checks:** K1: every Settings navigation item is `--row-h` tall with its icon and
  label on the same baselines, checked by screenshot at 1280 and 1024 in both themes; the
  dialog traps focus, closes on Esc and scrim click, and returns focus to its trigger; Tabs and
  Menu pass their keyboard patterns (a pure test for their key handling, as `Select` has);
  `rg "RowMenu|SettingsModal" apps/desktop/src` returns nothing.
- **Risks:** renaming touches callers' import lines in files later stages own; that is why this
  stage runs before them.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** components accept `className` for layout only. The Dialog's narrow sheet
  behaviour is here so stage 14 needs no dialog changes.

### Stage 4: session panel, composer and transcript

- **Objective:** the main session column at the reference's quality: a centred transcript at
  the readable measure, quiet tool calls, a calm composer.
- **Depends on:** stage 3.
- **Owned paths:** `panels/SessionPanel.tsx`, `Composer.tsx`, `AgentView.tsx`,
  `ContextBreakdown.tsx`, `ComparePanel.tsx`, `NewSessionPanel.tsx`; `components/ToolCall.tsx`,
  `ToolGroup.tsx`, `ToolState.tsx`, `WaitLine.tsx`, `JumpToLatest.tsx`, `AttachmentChips.tsx`,
  `ModelName.tsx`; `styles/session.css`.
- **What visibly changes:** user messages in a soft bubble at `--radius`, assistant messages as
  unboxed prose at `--fs-base` and `--lh-prose`; headings, lists, tables and code blocks on the
  type scale; tool calls as one quiet row each (icon, name, one-line summary, state) that
  expands; the composer as a single `--radius-lg` well on `--surface-sunken` with the footer
  (model, cost, options, context meter) in `--fs-sm` muted; the waiting line and steering marks
  restyled.
- **Acceptance checks:** budgets for `session.css` at zero; streaming, free scroll, jump to
  latest, steering, queue, attachments, voice and the context breakdown behave as before
  (manual run through each); the transcript at a 320 px column has no horizontal scroll except
  inside code blocks and tables.
- **Risks:** the transcript is the most used view; screenshot long sessions with code, tables,
  images, errors and subagent logs.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** the prose measure (`--prose-max`) and the user's prose and code scales must
  keep working.

### Stage 5: shell, sidebar and navigation

- **Objective:** the title bar, panel headers, the environments and sessions sidebar, the
  command palette and the panel editor, plus the launch-focus part of K5.
- **Depends on:** stage 3.
- **Owned paths:** `App.tsx`, `layout/` (all), `shared/useWindows.ts`, `ConnectionBanner.tsx`,
  `DaemonControl.tsx`, `UpdateControl.tsx`, `panels/NavigationPanels.tsx`, `SessionSearch.tsx`,
  `environments/projectIcons.tsx`, `commands/CommandPalette.tsx`, `styles/shell.css`,
  `styles/sidebar.css`, `styles/palette.css`.
- **What visibly changes:** a lighter title bar (icon buttons via IconButton, no metal), panel
  headers at one height with the title in `--fs-md` semibold, session and environment rows at
  `--row-h` with the selected row on `--accent-soft`, the palette on the Dialog with `Kbd`
  hints, the panel editor's handles and drop lines on the new tokens.
- **Acceptance checks:** K5 (launch): after launching the app and pressing Ctrl+, without
  clicking, Settings opens (the shell focuses the document once panels render); session rows
  in the sidebar line up with environment rows at every density; budgets for the three files
  at zero.
- **Risks:** `App.tsx` is 1,705 lines; change only markup and class names in it.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** do not add narrow-width behaviour here; stage 14 owns it, in these same
  files.

### Stage 6: Settings

- **Objective:** every Settings section with one row layout and spacing, plus K6.
- **Depends on:** stage 3.
- **Owned paths:** `settings/*.tsx`, `components/SettingsRow.tsx`, `ShortcutsSection.tsx`,
  `ApiSection.tsx`, `BenchmarkSection.tsx`, `BudgetsEditor.tsx`, `PermissionsEditor.tsx`,
  `styles/settings.css`.
- **What visibly changes:** rows at a consistent height with label and description on the left
  and the control right-aligned in a fixed-width column; sections separated by space rather than
  lines; the Keyboard section as a table with the chord in `Kbd` and a `--sp-3` gap before
  Change and Reset (K6); the benchmark's native select becomes a Select; loading and error states
  use the shared components.
- **Acceptance checks:** K6: in Settings → Keyboard at 1024 px the chord and the Change button
  are at least 12 px apart; budgets for `settings.css` at zero; search in Settings still
  filters and dims as before.
- **Risks:** none beyond the checklist.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 7: Connectors

- **Objective:** the Connectors window on the shared Dialog, Tabs, Card and form components.
- **Depends on:** stage 3.
- **Owned paths:** `connectors/` (all), `styles/connectors.css`.
- **What visibly changes:** the window uses the Dialog shell; Browse and Installed are Tabs;
  connector cards are `.card.interactive` in an even grid; forms use Field with hints and
  errors; the installed list's rows match the sidebar row height.
- **Acceptance checks:** connector add, configure, test and remove flows unchanged; budgets at
  zero; the window's own `@media (max-width: 720px)` rule is replaced by the Dialog's narrow
  sheet.
- **Risks:** none beyond the checklist.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 8: model configuration and model panels

- **Objective:** the model configuration dialog, the model picker and the model panels.
- **Depends on:** stage 3.
- **Owned paths:** `models/*.tsx`, `models/modelConfig.css`, `panels/ModelSelect.tsx`,
  `ModelsPanel.tsx`, `LocalModelsPanel.tsx`, `ModelSetup.tsx`, `ModelMarks.tsx`,
  `styles/models.css`.
- **What visibly changes:** one `.table` style for the model tables, the three native selects
  (`LocalForm`, `ModelConfigDialog`, `TableForm`) become Select, `info-tip` uses Tooltip, the
  first-run setup's three choices become cards with `--control-h-lg` actions.
- **Acceptance checks:** K3 (model panels): with the default layout at 1024 × 768, the Models and
  Local models panels' toolbars, tables and download form fit their column without clipping, in
  both themes; budgets for both CSS files at zero; drafts, apply and restart notices behave as
  before.
- **Risks:** `LocalModelsPanel.tsx` is 792 lines; keep the change to markup and classes.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 9: environments

- **Objective:** the environment window and its sections.
- **Depends on:** stage 3.
- **Owned paths:** `environments/EnvironmentWindow.tsx`, `panels/EnvironmentAgents.tsx`,
  `EnvironmentConfigSection.tsx`, `EnvironmentRuntimeFields.tsx`, `AgentInitiative.tsx`,
  `KnowledgeSection.tsx`, `styles/environments.css`.
- **What visibly changes:** sections in the shared Dialog with Field layout; the Danger zone as
  a `danger` Notice with one destructive button; agent lists as rows at `--row-h`.
- **Acceptance checks:** create, edit, unsaved-changes prompt and delete behave as before;
  budgets at zero.
- **Risks:** none beyond the checklist.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 10: side panels

- **Objective:** the panels that live in side columns, and the panel part of K3.
- **Depends on:** stage 3.
- **Owned paths:** `panels/AgentsPanel.tsx`, `ApprovalsPanel.tsx`, `BrowserPanel.tsx`,
  `PreviewPanel.tsx`, `ResourcesPanel.tsx`, `ReviewPanel.tsx`, `TelemetryPanel.tsx`,
  `UsagePanel.tsx`, `styles/panels.css`.
- **What visibly changes:** panel content uses the shared table, empty, loading and notice
  states; the Browser tabs use Tabs; the review panel's two native selects become Select; fixed
  control minimums give way to `min-width: 0` and wrapping toolbars, with container queries
  stacking label and control under 22 rem.
- **Acceptance checks:** K3: with the default layout at 1024 × 768, no input, select or button in
  a right-hand panel is clipped (each control's bounding box lies inside its panel), in both
  themes; each panel at a 280 px column has no horizontal scroll; budgets at zero.
- **Risks:** many small panels; split into two pull requests (tables and data panels; browser,
  preview and review) if the diff passes about 800 lines.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 11: tour

- **Objective:** the first-run tour card and its highlight.
- **Depends on:** stage 3.
- **Owned paths:** `tour/` (all), `styles/tour.css`.
- **What visibly changes:** the tour card as a `.card` at `--elev-2`, step dots and buttons on
  the shared styles, the highlight ring using `--focus`, the scrim using `--scrim`.
- **Acceptance checks:** the tour still skips missing targets, yields to approvals and
  remembers its outcome (`tourSteps.test.ts` passes, and a manual run); budgets at zero.
- **Risks:** none beyond the checklist.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 12: house-models card

- **Objective:** the house-models suggestion and the model suggestion card.
- **Depends on:** stage 3.
- **Owned paths:** `panels/HouseModelCard.tsx`, `ModelSuggestionCard.tsx`,
  `styles/house-models.css`.
- **What visibly changes:** both as `.card` with a title at `--fs-lg`, one primary action, a
  ghost dismiss IconButton, and model rows that match the model lists.
- **Acceptance checks:** dismiss and set-up actions unchanged; budgets at zero.
- **Risks:** none.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** small; may be paired with stage 11 in one pull request if both are ready.

### Stage 13: nursery and mod frames

- **Objective:** mod frames that follow the app's theme and size, and a nursery that fits a
  narrow sidebar: K2, K7 and the mod-frame part of K5.
- **Depends on:** stage 3.
- **Owned paths:** `apps/desktop/src/mods/ModFrame.tsx`, `styles/mods.css`,
  `mods/imperium-nursery/src/view.ts`, `panels.ts`, `space.ts` and its tests in
  `mods/imperium-nursery/test`, docs [06](../06-extensibility.md) (mod tokens) and
  [16](../16-pets.md) if they describe the frame.
- **What visibly changes:** frames set `color-scheme` to the resolved app theme (not
  `light dark`) and update their tokens when the theme or accent changes, so native controls in
  mods match (K2's dark select); frames receive the new type, spacing and radius tokens under new
  names (`--fs-sm`, `--sp-1`… added to `THEME_TOKENS`, existing names unchanged); the nursery's
  controls bar wraps under 220 px, its room fills the frame with one scroll owner, its select is
  styled with the tokens, and placement keeps each pet's whole box (sprite plus label) apart
  (K7).
- **Acceptance checks:** K2: the nursery panel at 140 px wide shows at most one scrollbar and
  its select matches the light theme; K7: a pure test in `mods/imperium-nursery/test` places
  ten pets in a 140 × 300 room and finds no two boxes (sprite and label) overlapping; K5
  (frames): with focus inside a mod frame, Ctrl+, opens Settings (the frame forwards chords
  that hold Ctrl, Cmd or Alt to the parent through the existing bridge, and the parent runs them
  through the same shortcut matcher); `pnpm --filter ./mods/imperium-nursery test` passes.
- **Risks:** forwarding keys from frames must not let a mod trigger anything a key press by the
  user could not: forward only real keyboard events' chords, never a mod-chosen message, and
  only the rebindable shortcuts. Review this part as security-relevant.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** after this stage no native `<select>` is left; turn on guard test 4.

### Stage 14: narrow layout

- **Objective:** a usable app at 600 px and below, and column minimums at every width (K3, K4).
- **Depends on:** stage 5.
- **Owned paths:** `App.tsx`, `layout/` (all), `styles/shell.css`, `styles/sidebar.css`, docs 07
  Layout and Scaling.
- **What visibly changes:** every column gets a minimum width (sidebar 220 px, session 360 px,
  side columns 280 px); when the window cannot fit all columns at their minimums, the
  rightmost side columns fold into a "More panels" switcher in the title bar; at 600 px and
  below the window shows one column at a time: the session column fills it, the sidebar opens
  as an overlay sheet from a title-bar button (closing on selection, Esc or scrim), and side
  panels open as a full-width sheet. Saved column proportions are kept and restored when the
  window widens again.
- **Acceptance checks:** K4: at 420 × 800 the session is readable without horizontal scroll and
  the sidebar and each side panel can be opened and closed by mouse and keyboard; at 1024 × 768
  no column is narrower than its minimum; a pure test in `apps/desktop/test` for the rule that
  picks which columns fold at a given width; saved layouts and presets load unchanged.
- **Risks:** this is the one new interaction in the plan; the fold rule must be pure and tested,
  and the panel editor (layout mode) should be disabled below 600 px rather than adapted.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** answer [Q4](#open-questions-for-the-user) before starting.

### Stage 15: motion and accessibility pass

- **Objective:** consistent motion and a full accessibility pass across the restyled app.
- **Depends on:** stages 1 to 14.
- **Owned paths:** every file under `apps/desktop/src/styles/`, `apps/desktop/test/styleTokens.test.ts`,
  docs 07 Accessibility and Theming; TSX only for missing labels or roles found in the pass.
- **What visibly changes:** menus, popovers, dialogs and sheets enter and leave with the
  token durations; panel folding uses `--dur-base`; hover and press transitions are uniform;
  the forced-colours rules cover the new components.
- **Acceptance checks:** budget comments removed and the style guard requires zero in every
  file; with reduced motion on, no transform or animation runs (checked by toggling the
  setting and the operating-system preference); every surface passes a keyboard-only run;
  `rg "outline: (none|0)" apps/desktop/src/styles` matches only rules that draw another focus
  indicator; docs 07 describe the token system and the components.
- **Risks:** touches every style file, so it must start after the others merge.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** when this stage is done, fold the token and component description into
  docs 07 and delete this plan (git keeps it).

## Decisions

Made while writing the plan; each can be revisited if its condition changes.

- **D1: plain React and CSS, no UI framework.** Rejected: Radix or a shadcn-style kit (a large
  new dependency, and the app already has working ARIA components for select, menu, switch and
  dialog); Tailwind (rewriting 7,000 lines of CSS and dropping the custom-property contract mods
  depend on). Revisit if more than three components need complex positioning or virtualisation.
- **D2: buttons stay native elements with classes.** 246 buttons use the class API; wrapping them
  in a component adds churn without changing the look. Only icon-only buttons get a component,
  because it enforces the accessible name.
- **D3: split the stylesheet by surface in a separate, no-change stage.** Rejected: leaving one
  file (parallel stages would conflict constantly, and the 54 repeated selectors would stay) and
  CSS-in-JS (a runtime cost and a new dependency). The order check and screenshot diff make the
  move safe.
- **D4: style budgets live in each CSS file.** A shared baseline file would be edited by every
  parallel stage.
- **D5: legacy token names stay as aliases until nothing reads them,** and the mod token names
  stay for good.
- **D6: flat surfaces.** The metal gradients and inset highlights go; elevation is a shadow on
  floating things only. The wordmark keeps its chrome gradient pending Q1.
- **D7: accents are defined per theme in CSS,** selected with `data-accent`, instead of being
  written inline (fixes K8 and keeps colours in the token file).
- **D8: no Toast and no Skeleton** (see [shared components](#shared-components)).
- **D9: K5 is fixed in the web view,** not with native menu accelerators, because Rust changes
  are out of scope.
- **D10: one pull request per stage,** merged to `experimental` as each is ready, unless the user
  prefers an integration branch (Q5).

## Open questions for the user

Each has a default the plan uses if unanswered.

- **Q1: metallic identity.** Keep a trace of today's brushed-metal look (the chrome wordmark and
  a subtle title-bar finish), or go fully flat? Default: keep the wordmark only.
- **Q2: default accent.** Keep cyan (`#1fb2c8` dark, `#0a7186` light) as the default of the
  twelve accents? Default: yes.
- **Q3: font.** Bundle Inter Variable (SIL Open Font License, about 300 KB of `woff2` for Latin,
  added as `@fontsource-variable/inter` or as files in `public/`) so every machine renders the
  same, or use each system's own UI font (Segoe UI Variable, San Francisco) with no new
  dependency? Default: bundle Inter.
- **Q4: density and narrow behaviour.** Keep comfortable rows at 30 px (today's control height),
  or move to roomier 36 px rows closer to the reference? And at 600 px and below, is one column at
  a time with the sidebar as a sheet acceptable? Default: 30 px rows; one column with a sheet.
- **Q5: rollout.** Merge each stage to `experimental` as it is ready (the app looks mixed between
  stage 2 and the last surface stage), or collect stages 2 to 13 on an integration branch and
  merge them together? Default: merge as ready.
