# Plan: desktop interface polish

Revision: 2026-11-16, revised after plan review round 1 and the user's answers; figures measured
on `experimental` at `feddfc9e` (last updated). Roadmap [item 47](../12-roadmap.md). Status:
planned; no stage started.

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
   budget at zero, and its budget comments are removed in [stage 15](#stage-15-motion-and-accessibility-pass).
2. **Shared components.** Buttons, icon buttons, fields, selects, switches, tabs, menus,
   dialogs, tables, cards, badges, empty, loading and notice states each have one
   implementation, used everywhere. Check: no native `<select>` in `apps/desktop/src` (guard 4);
   `rg 'className="(empty|error)"' apps/desktop/src` returns nothing; reviewers find no second
   implementation of a listed component.
3. **Known defects fixed.** The eight defects in [known defects](#known-defects) are gone, except
   the parts moved to [follow-ups](#follow-ups-outside-item-47). Check: each has a named check in
   the stage that fixes it.
4. **Works at every width.** The app is usable at 1280, 1024 and 420 px wide, in light and
   dark, by keyboard alone. Check: the [visual checklist](#visual-checklist) passes for every
   stage.
5. **Readable and accessible.** Body and secondary text meet WCAG AA (4.5:1); the focus ring
   and control boundaries (`--border-control`) meet 3:1; in both themes and for every accent;
   controls are at least 24 px tall at every density. Check: the contrast test from
   [stage 2](#stage-2-tokens-and-base-elements) and the size floors in [tokens](#tokens).
6. **No behaviour or contract change.** No feature, protocol, Rust or mod-contract change. Check:
   the diff of every stage touches only the paths it owns, and `bash scripts/check-web.sh` passes.

## Scope and non-goals

In scope: `apps/desktop/src` (TSX and CSS), `apps/desktop/public/boot.css`, the desktop tests in
`apps/desktop/test`, the desktop `package.json` (one font dependency), the mod frame
(`apps/desktop/src/mods/ModFrame.tsx`) and the bundled nursery mod's view code
(`mods/imperium-nursery/src`), and the desktop UI document ([07](../07-desktop-ui.md)).

Non-goals:

- **No feature changes.** No new panels, settings, commands or flows. The one new interaction is
  how the window behaves below 600 px wide ([stage 14](#stage-14-narrow-layout)), which today
  has no layout at all. A toast system is not added: nothing in the app needs one today.
- **No protocol changes.** `PROTOCOL_VERSION` and `packages/protocol` stay as they are.
- **No Rust changes.** Nothing in `apps/desktop/src-tauri` or `crates/` changes. The window
  backgrounds in `main.rs` and `tauri.conf.json` stay correct because `--bg` keeps today's values
  ([decision D11](#decisions)).
- **No mod contract change.** Mods receive `--bg`, `--panel`, `--fg`, `--muted`, `--border`,
  `--accent`, `--ok`, `--danger`, `--space` and `--radius` (`THEME_TOKENS` in `ModFrame.tsx`;
  [06](../06-extensibility.md#ui-mods) says theme tokens are copied into the frame). Those names
  stay, keep their meaning, and no names are added. No new mod-bridge message kinds
  ([decision D14](#decisions)).
- **No UI framework.** Plain React and CSS custom properties, as today (see
  [decision D1](#decisions)). The one new dependency is the Inter Variable font
  ([decision U3](#decided-by-the-user)).

## Assumptions

- The visual review that found the defects ran at the default UI scale (100 %), comfortable
  density, on Windows. Pixel values below are at that scale: `html` is 15 px, so 1 rem = 15 px.
- Implementers can run the app with a daemon and take screenshots; this plan was written from
  the code only (no browser was used), so the defect causes marked *suspected* are confirmed
  (and K5 reproduced) before their fixes are accepted.
- Stages land one pull request each against `experimental`, merged as each is ready, in the order
  below, with the parallel windows noted.

## Relevant paths

| Path | What it is |
| --- | --- |
| `apps/desktop/src/styles.css` | 7,176 lines; every rule in the app except the model dialog's |
| `apps/desktop/src/models/modelConfig.css` | 255 lines; the model configuration dialog, imported by `ModelConfigDialog.tsx` |
| `apps/desktop/src/components/` | Today's shared pieces: `Select`, `Switch`, `RowMenu`, `SettingsModal`, `SettingsRow` |
| `apps/desktop/src/settings/settings.ts` | Writes theme, accent, scales and fonts to the root element |
| `apps/desktop/src/shared/boot.ts`, `public/boot.css`, `public/boot.js` | The loading screen and the theme before first paint |
| `apps/desktop/test/boot.test.ts` | Checks that `--bg` and `--accent` agree across the CSS, `boot.css`, `boot.ts`, `main.rs` and `tauri.conf.json` |
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
Each stage measures its own files again after merging the latest `experimental`, since the
counts move as other stages land.

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

- **Rules repeat.** 54 selectors are repeated (`.settings-nav-item` at lines 4461 and 5190,
  `.turn-summary` and `.error` three times each, `button`, `.prose`, the `.message.assistant`
  headings), because the file grew by appending: late blocks such as "Settings polish"
  (line 5189) override earlier ones. Session rules alone sit in at least six separate regions of
  the file. *Counting rule:* comments removed; only rules at the top level (not inside `@media`,
  `@container`, `@supports` or `@keyframes`); each selector list split at commas and its
  whitespace collapsed; a selector is repeated when it heads two or more such rules. The 1,038
  top-level rules carry 1,117 distinct selectors.
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
- **Input boundaries are faint.** `--border` (`rgb(192 202 218 / 0.13)` dark,
  `rgb(40 50 66 / 0.14)` light) is the only edge of inputs and selects, well under the 3:1 a
  control boundary needs.
- **Responsive rules are few:** two `@media (max-width: 720px)` blocks (Settings and
  Connectors), seven `@container` width queries, five `(hover: none)` and one
  `(forced-colors: active)`. Columns have no minimum width.

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
| K1 | Settings navigation items and icons look ragged | `.settings-nav-item` is styled twice (line 4461: block padding, 0.88 rem; line 5190: flex, gap 0.6 rem, 1 rem icons), items with badges use a column layout (`.settings-nav-item-text`), and group labels use a different inset (0.2 rem 0.6 rem) from items; nothing fixes the row height | [Stage 3](#stage-3-shared-components) (the dialog navigation, in `styles/dialog.css`) |
| K2 | The nursery panel in the sidebar is about 140 px wide, shows both scrollbars, and has a dark select in the light theme | The frame sets `color-scheme: light dark` (`ModFrame.tsx` line 119), so native controls follow the operating system, not the app theme; the frame body has 8 px padding and the room a 160 px minimum height plus a computed `roomHeight`, so the room overflows the frame both ways at that width | [Stage 13](#stage-13-nursery-and-mod-frames); live theme switching in an open frame is a [follow-up](#follow-ups-outside-item-47) |
| K3 | Right-hand panels clip inputs at 1024 px | *Suspected:* fixed minimum widths on controls (`min-width: 10rem` on `.models-toolbar input`, `.local-model-table .model-name`, `.hf-download select` and `.row-menu`; `9rem` on `.new-session-subfolder`) inside columns that shrink proportionally with `overflow: hidden`; which panels the review saw needs confirming | [Stage 8](#stage-8-model-configuration-and-model-panels) and [stage 10](#stage-10-side-panels) (panel content), [stage 14](#stage-14-narrow-layout) (column minimums) |
| K4 | No narrow layout (600 px and below); three columns crush | Column widths are proportions (`Columns.tsx`, `flex-grow` share with `flex-basis: 0`) with no minimum and no breakpoint | [Stage 14](#stage-14-narrow-layout) |
| K5 | Ctrl+, works only after clicking the page | The handler is a `window` keydown listener (`shared/useWindows.ts`). *Suspected:* the web view's document has no keyboard focus at launch until clicked. Keys pressed while a sandboxed mod frame has focus also never reach the parent window | [Stage 5](#stage-5-shell-sidebar-and-navigation) (launch focus, after reproducing); keys inside mod frames are a [follow-up](#follow-ups-outside-item-47) |
| K6 | Shortcut text sits tight against its Change button | `.shortcuts td` has no column gap or fixed control column | [Stage 6](#stage-6-settings) |
| K7 | Pets and labels overlap in the nursery room | *Suspected:* placement in `space.ts` spaces pets by sprite size, while each pet's box is `LABEL_W` wide plus a 14 px label line under it | [Stage 13](#stage-13-nursery-and-mod-frames) |
| K8 | Light theme accent fails contrast | See [tokens today](#tokens-today): the dark accent is written inline over the light one | [Stage 2](#stage-2-tokens-and-base-elements) |

## Design direction

### Principles

1. **One accent.** The accent marks the primary action, the current selection, focus and links,
   and nothing else. Status uses the semantic colours; everything else is neutral.
2. **Quiet borders.** Hairline borders at low contrast separate regions; elevation (a shadow)
   is reserved for things that float: menus, popovers, dialogs. No gradients on surfaces, no
   inset highlights on buttons. The one exception to "quiet" is the edge of an input, select or
   checkbox, which uses `--border-control` so the control can be found.
3. **Generous whitespace, few sizes.** Spacing comes from one 4 px scale; text from six sizes
   and three weights. When in doubt, more space and a smaller, lighter label.
4. **Consistent rows.** Every list row (sessions, environments, settings navigation, menus,
   model lists) has the same height for its density; controls in a row align to its centre.
5. **Icons follow text.** Lucide line icons at 16 px beside 13 px text, 13 px in dense rows,
   19 px only in empty states and the title bar; the stroke width stays the single value set in
   `main.tsx`.
6. **Every view has its three states.** Empty, loading and error each use the shared
   component, with a one-line explanation and at most one action.
7. **Motion explains, never decorates.** Things that appear fade and rise a few pixels; nothing
   bounces; reduced motion removes movement and keeps fades instant.

### Identity, not a copy

Imperium keeps its own look: cool graphite neutrals in dark mode, cool near-white in light,
and its cyan accent; flat surfaces, with the chrome gradient kept only on the title-bar
wordmark. It does not use Anthropic's typefaces, logo, warm ivory backgrounds, terracotta
accent or serif headings. What is borrowed is the discipline: few sizes, calm colour, roomy
spacing, soft states.

### Tokens

All tokens live in `apps/desktop/src/styles/tokens.css`. Lengths are built from one unit,
`--px`, one CSS pixel at 100 % UI scale, so `--ui-scale` zooms everything; spacing, row and
control heights also multiply by `--space-scale` (density). Pixel values in comments are at
100 % and comfortable density.

```css
--px: calc(1rem / 15); /* 1px at 100 %; html is calc(15px * var(--ui-scale)) */
```

**Type scale.** Six sizes, three weights, three line heights. Prose and code keep their own
user scales. The UI face is Inter Variable, bundled ([U3](#decided-by-the-user)).

```css
--font-ui: "Inter Variable", "Segoe UI Variable Text", "Segoe UI", system-ui, -apple-system, sans-serif;
--font-code: "Cascadia Code", "JetBrains Mono", ui-monospace, Menlo, Consolas, monospace;
--fs-xs: calc(11 * var(--px));   /* 11px  badges, captions, keyboard hints */
--fs-sm: calc(12 * var(--px));   /* 12px  secondary text, meta, table cells */
--fs-md: calc(13 * var(--px));   /* 13px  controls, rows, labels: the UI default */
--fs-base: calc(15 * var(--px)); /* 15px  transcript prose (times --prose-scale) */
--fs-lg: calc(17 * var(--px));   /* 17px  section and panel titles */
--fs-xl: calc(21 * var(--px));   /* 21px  dialog titles, empty-state headings */
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

**Spacing scale.** True 4 px steps; `--space` stays as an alias of `--sp-2` for mods.

```css
--sp-1: calc(4 * var(--px) * var(--space-scale));  /* 4px  icon-to-label inside a chip */
--sp-2: calc(8 * var(--px) * var(--space-scale));  /* 8px  between controls in a row */
--sp-3: calc(12 * var(--px) * var(--space-scale)); /* 12px control padding, row padding */
--sp-4: calc(16 * var(--px) * var(--space-scale)); /* 16px panel padding, between groups */
--sp-5: calc(24 * var(--px) * var(--space-scale)); /* 24px dialog padding, between sections */
--sp-6: calc(32 * var(--px) * var(--space-scale)); /* 32px empty-state padding */
--sp-8: calc(48 * var(--px) * var(--space-scale)); /* 48px page margins in wide dialogs */
--space: var(--sp-2);
```

**Sizes.** Controls and rows share heights so a row and its controls line up. Rows stay at
30 px ([U4](#decided-by-the-user)), the one height off the 4 px grid. Every height has a 24 px
floor, so compact density (0.7) never makes a target smaller than WCAG 2.2's 24 px minimum.

```css
--size-floor: calc(24 * var(--px));
--control-h-sm: var(--size-floor);                                               /* 24px at every density: row tools */
--control-h: max(var(--size-floor), calc(30 * var(--px) * var(--space-scale)));  /* 30px: buttons, inputs, selects */
--control-h-lg: max(var(--size-floor), calc(40 * var(--px) * var(--space-scale))); /* 40px: composer send, first-run choices */
--row-h: var(--control-h);                                                       /* list rows, menu and navigation items */
--icon-sm: calc(13 * var(--px));
--icon: calc(16 * var(--px));
--icon-lg: calc(19 * var(--px));
```

**Radii.** Four steps and a pill; `--radius` keeps its name for mods. Radii scale with
`--ui-scale` (they are in `--px`) so shapes keep their proportions when zoomed, but not with
density.

```css
--radius-xs: calc(4 * var(--px));  /* badges, keyboard keys, inline code */
--radius-sm: calc(6 * var(--px));  /* buttons, inputs, selects, menu items */
--radius: calc(8 * var(--px));     /* cards, rows, menus, popovers, message bubbles */
--radius-lg: calc(12 * var(--px)); /* dialogs, the composer */
--radius-full: 999px;              /* pills, switches, avatars */
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

**Colours.** Semantic names; components never use a raw value. `--bg` keeps today's values
(`#0b0c0e`, `#f4f5f7`), which the loading screen, `shared/boot.ts`, `main.rs` and
`tauri.conf.json` repeat; the surfaces are derived from it. Contrast ratios were computed for
these values (WCAG relative luminance) and are checked again by the stage 2 test.

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `#0b0c0e` (today's) | `#f4f5f7` (today's) | Window background, behind panels |
| `--surface` (`--panel` alias) | `#121418` | `#ffffff` | Panels, dialogs |
| `--surface-raised` | `#1a1d22` | `#ffffff` | Cards, buttons, menus |
| `--surface-sunken` | `#0e1013` | `#eef0f2` | Inputs, code blocks, the composer well |
| `--surface-hover` | `rgb(255 255 255 / 0.04)` | `rgb(17 24 39 / 0.04)` | Row and ghost-button hover |
| `--surface-active` | `rgb(255 255 255 / 0.07)` | `rgb(17 24 39 / 0.07)` | Pressed, selected rows |
| `--border` | `rgb(255 255 255 / 0.08)` | `rgb(17 24 39 / 0.09)` | Dividers and cards: decorative, no contrast requirement |
| `--border-strong` | `rgb(255 255 255 / 0.14)` | `rgb(17 24 39 / 0.16)` | Table heads, hovered cards |
| `--border-control` | `#6a717b` (3.4:1 on raised, 4.0:1 on bg) | `#80868f` (3.2:1 on sunken, 3.7:1 on white) | Edges of inputs, selects, checkboxes, switches |
| `--text` (`--fg` alias) | `#e7e9ec` (13.9:1 or more) | `#16181b` (15.6:1 or more) | Body text |
| `--text-muted` (`--muted` alias) | `#a0a6ae` (6.9:1 or more) | `#545a63` (6.1:1 or more) | Secondary text, labels |
| `--text-subtle` | `#868d97` (5.1:1 or more) | `#5f656e` (5.1:1 or more) | Timestamps, placeholders |
| `--accent` | `#1fb2c8` | `#0a7186` | Primary fills, selection, links |
| `--accent-hover` | `#45c6d8` | `#086477` | Primary hover |
| `--accent-soft` | accent at 14 % | accent at 10 % | Selected rows, active navigation |
| `--accent-ink` | `#03161a` (7.3:1 on accent) | `#ffffff` (5.7:1 on accent) | Text on accent fills |
| `--accent-text` | `#5fd0e0` (9.3:1 or more) | `#0a7186` (4.9:1 or more) | Accent used as text or icon |
| `--focus` | `#5fd0e0` | `#0a7186` | Focus ring (both above 3:1 on every surface) |
| `--danger` / `--danger-soft` | `#f2766b` / 14 % | `#b42318` / 10 % | Errors, destructive actions |
| `--ok` / `--ok-soft` | `#58c98a` / 14 % | `#18794e` / 10 % | Success, running |
| `--warn` / `--warn-soft` | `#e3b25a` / 14 % | `#8a5a00` / 10 % | Warnings, budgets |

"Or more" ratios are the lowest over `--bg`, `--surface`, `--surface-raised` and
`--surface-sunken`. The context-chart colours (`--ctx-*`) stay as a separate
data-visualisation set, exempt from the one-accent rule. `--silver`, `--bubble`, `--code-bg`
and `--ink` become aliases of the tokens above and are removed once nothing reads them.

**Accents per theme.** Each of the twelve accents in `settings.ts` gets a dark and a light
value. Dark values are today's (dark ink on them is 5.0:1 or more for all twelve). Light values
are darkened in lightness, keeping hue and saturation, until white ink reaches about 5.2:1, which
also makes them pass 4.5:1 as text on `--bg`; cyan keeps the hand-picked `#0a7186` (5.7:1).
Darkened that far, amber, lime and silver turn brownish or olive (already at 4.6:1 amber is near
`#9c6b18`, lime near `#647d25` and silver near `#7c7362`). That is accepted for fills and text, the places contrast matters; their
`--accent-soft` washes use the dark-theme value at 16 % so selected rows keep the accent's
recognisable hue, and stage 2's screenshots of every accent in light mode are reviewed for
muddiness before merging (a light value may be nudged in hue only if the contrast test still
passes). `applySettings` sets `data-accent` on the root instead of writing colours inline, and
`tokens.css` defines each accent under `:root[data-accent="…"]` and
`:root[data-theme="light"][data-accent="…"]`, so the light theme's values win (K8). The loading
screen's light accent becomes `#0a7186`.

**Focus ring.** One rule for every focusable element:
`outline: 2px solid var(--focus); outline-offset: 2px` on `:focus-visible`, offset 0 inside
inputs (where the border also takes `--focus`). The `--accent-soft` halos go. Outlines show in
forced-colours mode, which halos do not.

## Shared components

Plain React components in `apps/desktop/src/components/` with their CSS in
`styles/primitives.css`, except the dialog, navigation column, menu and popover rules, which go
in `styles/dialog.css`. Stage 3 owns both files. Each component wraps native elements, keeps
WAI-ARIA patterns the app already uses, and takes `className` for layout only (never colour or
size).

| Component | Variants and API | Where it is used today | Migration |
| --- | --- | --- | --- |
| Button | Stays a native `<button>` styled by element and class: `primary`, `ghost`, `danger`, default (secondary); sizes `small`, default, `large` | 246 buttons in 67 files | The class API already exists; stage 2 restyles it, surface stages remove per-surface overrides |
| IconButton | `<IconButton icon label tooltip? size?>`; `label` is required and becomes `aria-label` and the tooltip | 40 `.icon-only`, panel and title-bar tools, row tools | Surface stages replace `.icon-only` markup |
| Tooltip | Shown on hover after 400 ms and at once on keyboard focus; `role="tooltip"` with `aria-describedby`; positioned by `shared/floating.ts` | 195 native `title=`; `info-tip` in `ConfigParts.tsx` | Used by IconButton and `info-tip`; other `title=` attributes stay (truncated text) |
| Field | `<Field label hint error>` around an input, textarea or Select; Input and Textarea are native elements styled globally | 67 inputs, 14 textareas, `FieldRow`, `FormParts` | Surface stages wrap labelled controls; `FormParts` and `FieldRow` become thin users |
| Select | Existing `components/Select.tsx`, restyled, plus `size="small"` | 28 uses; 6 native selects | The native ones move to Select in their surface stage, lowering guard 4's allowance |
| Switch | Existing `components/Switch.tsx`, restyled | 13 uses | None |
| Checkbox | Native checkbox with `accent-color: var(--accent)`, a `--border-control` edge and the focus ring; no component | Permission and connector forms | CSS only |
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
- [ ] Density compact and spacious, UI scale 150 %: rows stay aligned, nothing overflows, no
      control is under 24 px tall.
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
4. **No native select** (same file, from stage 1): counts `<select` in each
   `apps/desktop/src/**/*.tsx` file against an allowance declared in that file
   (`// style-budget: native-select=2`); a file without the comment is allowed none. Stage 1
   adds the comment to the five files that have one (`BenchmarkSection.tsx`, `LocalForm.tsx`,
   `ModelConfigDialog.tsx`, `TableForm.tsx`, `ReviewPanel.tsx`); stages 6, 8 and 10, which own
   them, remove it; stage 15 confirms none is left. The nursery's select lives in
   `mods/imperium-nursery`, outside this guard, and is handled by stage 13.
5. **`tokenContrast.test.ts`** (stage 2): parses `styles/tokens.css`, and for both themes and
   all twelve accents checks text, muted and subtle text on `--bg`, `--surface`,
   `--surface-raised` and `--surface-sunken` (4.5:1), accent ink on accent and accent text on
   `--bg` (4.5:1), and `--focus` and `--border-control` on every surface (3:1).
6. **`boot.test.ts`** (exists): keeps `--bg` and the boot accent in step across the CSS,
   `boot.css`, `boot.ts`, `main.rs` and `tauri.conf.json`. Stage 1 points it at
   `styles/tokens.css`; stage 2 changes its accent pattern to read the cyan
   `data-accent` definitions.
7. **Icon buttons are named** (stage 3): `IconButton`'s `label` prop is required by its type, so
   `pnpm typecheck` is the guard; no runtime test.

## Stages

Stages 1 to 3 run in order. After stage 3 merges, stages 4 to 13 own disjoint files and any two
(or more) can run in parallel; stage 14 needs stage 5, and stage 15 runs last. Every stage:
merges the latest `experimental` and re-measures its budgets before review; passes
`pnpm --filter @imperium/desktop test` and `pnpm --filter @imperium/desktop typecheck` locally
and `bash scripts/check-web.sh` in CI; passes the [visual checklist](#visual-checklist); and
touches no path outside its owned paths (a needed change elsewhere goes in its handoff notes for
the owning stage).

"Owned CSS" means the file in `apps/desktop/src/styles/` created by stage 1 for that surface.

### Stage 1: stylesheet split and guard tests

- **Objective:** split `styles.css` into one file per surface with no visible change, and add
  the guard tests at today's counts.
- **Depends on:** none.
- **Owned paths:** `apps/desktop/src/styles.css` (removed), `apps/desktop/src/styles/` (new:
  `tokens.css`, `base.css`, `primitives.css`, `dialog.css`, `shell.css`, `sidebar.css`,
  `palette.css`, `session.css`, `settings.css`, `connectors.css`, `models.css`,
  `environments.css`, `panels.css`, `tour.css`, `house-models.css`, `mods.css`, and `index.css`
  importing them in order), `apps/desktop/src/main.tsx` (the import),
  `apps/desktop/test/styleTokens.test.ts`, `apps/desktop/test/boot.test.ts` (reads
  `styles/tokens.css` instead of `styles.css`), the one-line `native-select` allowance
  comments in the five TSX files listed under guard 4, and docs [07](../07-desktop-ui.md)
  Source layout.
- **What visibly changes:** nothing.
- **How:**
  1. Merge each repeated selector into its last occurrence (where it already wins), one commit.
  2. Move each rule to its surface file. At-rule blocks (`@media`, `@container`, `@supports`)
     move whole, or are split per surface keeping their condition; `@keyframes` go to
     `base.css` (their names are unique, so order does not matter).
  3. Check order. Moving a rule (or a merged declaration) past another rule is a risk only when
     both have the same specificity, apply under the same at-rule conditions, and set a property
     in common. A script lists every such pair whose order changed, covering the merges of step
     1 and rules inside at-rules. Each pair is resolved by showing the two selectors cannot match
     the same element, or by placing the rule where its order is kept. The script stays out of
     the repository (no one-off checkers in the tree); its source and its full pair list, with
     each resolution, go in the pull request description.
  4. Keep the cascade position of `modelConfig.css`. It is imported by `ModelConfigDialog.tsx`,
     which `App.tsx` pulls in before `main.tsx` imports the stylesheet, so it comes earlier in
     the cascade than today's `styles.css`. `styles/index.css` replaces that import in the same
     place (after `import { App }`), so `modelConfig.css` stays earlier.
- **Acceptance checks:**
  - Computed styles are unchanged: a dump of `getComputedStyle` for every element of each
    surface, in both themes, matches before and after, taken in each state reached by real
    interaction: hover (pointer over each control), keyboard focus (Tab through), disabled
    controls, open menus, selects, dialogs and popovers, and error states (a failing form, an
    offline daemon). Differences are listed and explained or fixed.
  - The order-check pair list in the pull request has no unresolved pair.
  - `styleTokens.test.ts` passes with budgets equal to today's counts; the repeated-selector
    count, by the [counting rule](#tokens-today), is zero; `cssBalance.test.ts` and
    `boot.test.ts` pass.
- **Risks:** cascade order changes (mitigated above); a missed rule (the computed-style dump
  catches it).
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** keep each file's sections in their original relative order. Put rules used
  by several surfaces (`.muted`, `.error`, `.empty`, `.badge`) in `primitives.css`. Put the
  shared dialog shell (`.settings-backdrop`, `.settings-window`, its header, content and
  footer), the dialog navigation (`.settings-nav`, `.settings-group*`, `.settings-nav-item*`,
  `.settings-search`), the menu (`.row-menu*`) and the floating-list rules of `Select` in
  `dialog.css`, which stage 3 owns; `settings.css` keeps only rules for the Settings sections'
  content. Stage 3 can then fix K1 without touching `settings.css`.

### Stage 2: tokens and base elements

- **Objective:** the token system above, applied to element rules, with today's token names kept
  as aliases.
- **Depends on:** stage 1.
- **Owned paths:** `styles/tokens.css`, `styles/base.css`; every other `styles/*.css` file only to
  remove the metal gradients (`--metal*`, `--chrome-text` outside the wordmark), the `--highlight`
  inset lights and the `--accent-soft` focus halos, lowering those files' budgets to match;
  `apps/desktop/src/settings/settings.ts` (accents per theme, `data-accent`);
  `apps/desktop/src/shared/boot.ts` (comments naming `styles.css`; `BOOT_BACKGROUND` values
  unchanged); `apps/desktop/public/boot.css` (light accent `#0a7186`);
  `apps/desktop/test/boot.test.ts` (the accent pattern); `apps/desktop/test/tokenContrast.test.ts`;
  the Inter Variable font (`@fontsource-variable/inter` in `apps/desktop/package.json` and
  `pnpm-lock.yaml`, imported once in `styles/index.css`); docs 07 Theming.
- **What visibly changes:** everywhere at once: flat surfaces instead of metal gradients
  (except the title-bar wordmark), quieter borders, firmer input edges, the new neutrals derived
  from today's backgrounds, Inter on every machine, one focus ring, restyled buttons, inputs,
  checkboxes and scrollbars, and a light theme whose accent passes contrast (K8).
- **Acceptance checks:** `tokenContrast.test.ts` passes for both themes and all twelve accents;
  `boot.test.ts` passes with `main.rs` and `tauri.conf.json` untouched;
  `rg "metal|chrome-text" apps/desktop/src/styles` finds only the wordmark rule and
  `rg "highlight" apps/desktop/src/styles` finds nothing; screenshots of all twelve accents in
  light mode are reviewed for muddiness; the theme switch in the command palette still flips
  every surface, and the loading screen matches the theme before first paint.
- **Risks:** this is the largest visible change and lands before surfaces are tidied, so some
  surface overrides will look off until their stage (accepted: stages merge as ready, U5). Mods
  see new token values under the same names. The removal edits touch files later stages own, which
  is why this stage runs before them.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** keep `--fast`, `--slow`, `--ease`, `--space`, `--radius`, `--panel`, `--fg`,
  `--muted` as aliases. Stage-specific checks: Settings → Appearance accent swatches, theme
  System following the operating system. The font is bundled by Vite into the app (no network
  fetch), so the content security policy needs no change.

### Stage 3: shared components

- **Objective:** build the [shared components](#shared-components) and move their existing
  implementations onto them.
- **Depends on:** stage 2.
- **Owned paths:** `apps/desktop/src/components/` (new: `IconButton.tsx`, `Tooltip.tsx`,
  `Field.tsx`, `Tabs.tsx`, `Menu.tsx` (from `RowMenu.tsx`), `Dialog.tsx` (from
  `SettingsModal.tsx`), `Badge.tsx`, `EmptyState.tsx`, `LoadingState.tsx`, `Notice.tsx`,
  `Kbd.tsx`; restyled `Select.tsx`, `Switch.tsx`), `apps/desktop/src/shared/floating.ts`
  (positioning taken from `Select.tsx`), `apps/desktop/src/panels/StatusBadge.tsx`,
  `styles/primitives.css`, `styles/dialog.css`, and only the import lines and renamed tags of
  `RowMenu` and `SettingsModal` callers.
- **What visibly changes:** dialogs (Settings, environments, model configuration) get the new
  shell and a navigation column with fixed row height, one inset for group labels and items, and
  icons in a fixed 16 px column, which fixes K1; menus and selects share one popover look;
  badges and switches restyle.
- **Acceptance checks:** K1: every Settings navigation item is `--row-h` tall with its icon and
  label on the same baselines, checked by screenshot at 1280 and 1024 in both themes, with
  `settings.css` unchanged by this stage; the dialog traps focus, closes on Esc and scrim click,
  and returns focus to its trigger; Tabs and Menu pass their keyboard patterns (a pure test for
  their key handling, as `Select` has); `rg "RowMenu|SettingsModal" apps/desktop/src` returns
  nothing.
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
- **Acceptance checks:**
  - K5 reproduced first: the pull request records the exact steps that show Ctrl+, failing
    before the change (launch, which element has focus per `document.activeElement` and
    `document.hasFocus()`), and the fix is accepted only against those steps.
  - K5 fixed: after launching the app and pressing Ctrl+, without clicking, Settings opens.
    The intended fix focuses the document from the web view once panels render. If the
    reproduction shows the web view itself lacks operating-system focus (which only a Rust
    `set_focus` can give), that part moves to the [follow-ups](#follow-ups-outside-item-47) and
    this stage records it.
  - Session rows in the sidebar line up with environment rows at every density; budgets for the
    three CSS files at zero.
- **Risks:** `App.tsx` is 1,705 lines; change only markup and class names in it, plus the focus
  call.
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
  are at least 12 px apart; `BenchmarkSection.tsx` has no `native-select` allowance; budgets for
  `settings.css` at zero; search in Settings still filters and dims as before.
- **Risks:** none beyond the checklist.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** the navigation column is in `dialog.css` (stage 3), not here.

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
  both themes; the three files have no `native-select` allowance; budgets for both CSS files at
  zero; drafts, apply and restart notices behave as before.
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
  themes; each panel at a 280 px column has no horizontal scroll; `ReviewPanel.tsx` has no
  `native-select` allowance; budgets at zero.
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

- **Objective:** mod frames whose native controls follow the app's theme, and a nursery that fits
  a narrow sidebar: K2 and K7, with no change to the mod contract or the bridge.
- **Depends on:** stage 3.
- **Owned paths:** `apps/desktop/src/mods/ModFrame.tsx` (the frame's base CSS only),
  `styles/mods.css`, `mods/imperium-nursery/src/view.ts`, `panels.ts`, `space.ts` and its tests in
  `mods/imperium-nursery/test`.
- **What visibly changes:** a frame sets `color-scheme` to the app theme resolved when the frame
  is built (`dark` or `light`, not `light dark`), so native controls in mods match the app (K2's
  dark select); the frame body's padding uses the existing `--space`. The nursery's controls bar
  wraps under 220 px, its room fills the frame with one scroll owner, its select is styled with
  the existing tokens (`--panel`, `--fg`, `--border`, `--radius`), and placement keeps each pet's
  whole box (sprite plus label) apart (K7).
- **Acceptance checks:** K2: with the app in the light theme, the nursery panel at 140 px wide
  shows at most one scrollbar and its select is light; K7: a pure test in
  `mods/imperium-nursery/test` places ten pets in a 140 × 300 room and finds no two boxes (sprite
  and label) overlapping; `THEME_TOKENS` and the bridge's message kinds are unchanged;
  `pnpm --filter ./mods/imperium-nursery test` passes.
- **Risks:** a frame opened before a theme switch keeps its old `color-scheme` and tokens until
  it is reopened, as tokens behave today ([07](../07-desktop-ui.md#theming): "Mod panels pick up
  the tokens when they open"). Live updates are a [follow-up](#follow-ups-outside-item-47).
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** guard 4 does not cover `mods/`; the nursery's select is checked by the
  screenshot only.

### Stage 14: narrow layout

- **Objective:** a usable app at 600 px and below, and column minimums at every width (K3, K4).
- **Depends on:** stage 5.
- **Owned paths:** `App.tsx`, `layout/` (all), `styles/shell.css`, `styles/sidebar.css`, docs 07
  Layout and Scaling.
- **What visibly changes:** every column gets a minimum width (sidebar 220 px, session 360 px,
  side columns 280 px); when the window cannot fit all columns at their minimums, the
  rightmost side columns fold into a "More panels" switcher in the title bar; below 600 px the
  window shows one column at a time ([U4](#decided-by-the-user)): the session column fills it,
  the sidebar opens as an overlay sheet from a title-bar button (closing on selection, Esc or
  scrim), and side panels open as a full-width sheet. Saved column proportions are kept and
  restored when the window widens again.
- **Acceptance checks:** K4: at 420 × 800 the session is readable without horizontal scroll and
  the sidebar and each side panel can be opened and closed by mouse and keyboard; at 1024 × 768
  no column is narrower than its minimum; a pure test in `apps/desktop/test` for the rule that
  picks which columns fold at a given width; saved layouts and presets load unchanged.
- **Risks:** this is the one new interaction in the plan; the fold rule must be pure and tested,
  and the panel editor (layout mode) should be disabled below 600 px rather than adapted.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** none yet.

### Stage 15: motion and accessibility pass

- **Objective:** consistent motion and a full accessibility pass across the restyled app.
- **Depends on:** stages 1 to 14.
- **Owned paths:** every file under `apps/desktop/src/styles/`, `apps/desktop/test/styleTokens.test.ts`,
  docs 07 Accessibility and Theming; TSX only for missing labels or roles found in the pass.
- **What visibly changes:** menus, popovers, dialogs and sheets enter and leave with the
  token durations; panel folding uses `--dur-base`; hover and press transitions are uniform;
  the forced-colours rules cover the new components.
- **Acceptance checks:** budget and `native-select` comments removed and the guards require zero
  everywhere; with reduced motion on, no transform or animation runs (checked by toggling the
  setting and the operating-system preference); every surface passes a keyboard-only run;
  `rg "outline: (none|0)" apps/desktop/src/styles` matches only rules that draw another focus
  indicator; docs 07 describe the token system and the components.
- **Risks:** touches every style file, so it must start after the others merge.
- **Status:** pending.
- **Outcome:** none yet.
- **Handoff notes:** when this stage is done, fold the token and component description into
  docs 07 and delete this plan (git keeps it).

## Follow-ups outside item 47

Each needs its own design and review because it changes the mod bridge or Rust, which item 47
does not.

- **Live theme updates in open mod frames.** A new bridge message (for example
  `theme_changed` carrying the token values and `color-scheme`) so an open frame restyles when
  the user switches theme or accent. Adds a message kind to the mod contract
  ([06](../06-extensibility.md#ui-mods)).
- **Shortcuts while a mod frame has focus** (the frame part of K5). Forwarding key chords from
  the frame to the parent needs a new message kind and a security review: a mod must not be able
  to trigger a shortcut the user did not press.
- **Native focus at launch,** only if stage 5's reproduction shows the web view lacks
  operating-system focus: a Rust `set_focus` on the main window after it is shown.

## Decisions

Made while writing and reviewing the plan; each can be revisited if its condition changes.

- **D1: plain React and CSS, no UI framework.** Rejected: Radix or a shadcn-style kit (a large
  new dependency, and the app already has working ARIA components for select, menu, switch and
  dialog); Tailwind (rewriting 7,000 lines of CSS and dropping the custom-property contract mods
  depend on). Revisit if more than three components need complex positioning or virtualisation.
- **D2: buttons stay native elements with classes.** 246 buttons use the class API; wrapping them
  in a component adds churn without changing the look. Only icon-only buttons get a component,
  because it enforces the accessible name.
- **D3: split the stylesheet by surface in a separate, no-change stage.** Rejected: leaving one
  file (parallel stages would conflict constantly, and the 54 repeated selectors would stay) and
  CSS-in-JS (a runtime cost and a new dependency). The order check and the computed-style dump
  make the move safe.
- **D4: style budgets live in each file.** A shared baseline file would be edited by every
  parallel stage.
- **D5: legacy token names stay as aliases until nothing reads them,** and the mod token names
  stay for good.
- **D6: flat surfaces.** The metal gradients and inset highlights go; elevation is a shadow on
  floating things only (U1).
- **D7: accents are defined per theme in CSS,** selected with `data-accent`, instead of being
  written inline (fixes K8 and keeps colours in the token file).
- **D8: no Toast and no Skeleton** (see [shared components](#shared-components)).
- **D9: no Rust in item 47.** K5's fix is in the web view; anything needing Rust is a follow-up.
- **D10: one pull request per stage,** merged to `experimental` as each is ready (U5).
- **D11: `--bg` keeps today's values** (`#0b0c0e`, `#f4f5f7`) and the surfaces derive from it, so
  the copies in `boot.css`, `boot.ts`, `main.rs` and `tauri.conf.json` stay true and no Rust file
  changes.
- **D12: a separate `--border-control`** at 3:1 or more for control edges, so decorative borders
  can stay quiet.
- **D13: tokens are built on `--px`.** Spacing is on a true 4 px grid; rows and controls have a
  24 px floor at every density; radii scale with the UI scale but not with density.
- **D14: no new mod-bridge message kinds and no new mod tokens in item 47.** K2 is fixed with
  `color-scheme` set when the frame is built; live updates and in-frame shortcuts are
  [follow-ups](#follow-ups-outside-item-47).

## Decided by the user

The open questions of the first draft, answered:

- **U1: look.** Flat surfaces; the chrome gradient stays on the title-bar wordmark only.
- **U2: default accent.** Cyan stays the default (`#1fb2c8` dark, `#0a7186` light).
- **U3: font.** Bundle Inter Variable (SIL Open Font License), added as
  `@fontsource-variable/inter` in stage 2.
- **U4: density and narrow layout.** Rows stay at 30 px at comfortable density; below 600 px the
  window shows one column at a time, with the sidebar as a sheet.
- **U5: rollout.** Each stage merges to `experimental` as it is ready; the app may look mixed
  between stage 2 and the last surface stage.

No questions are open.
