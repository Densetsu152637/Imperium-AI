# Plan: built-in text editor with Markdown presentation

Revision: 2026-11-26, revision 6 (round-2 review: R1, closing the remaining command-path
gap of B2 by checking before every forward, polling during background commands and freezing
servers during foreground ones, and the Windows `.cmd` shim fix, in [D21](#decisions),
[D28](#decisions) and stage E10c). Revision 5 answered the Opus review of revision 4: blockers B1, project
configuration read by probes and installs, and B2, agents changing execution-relevant files in a
trusted project, majors 1 to 6, the minors and nits; and the user's answers to open questions 7
to 13, recorded as [settled decisions](#settled-decisions). Revision 4 brought the user's request for linting and themes: language
servers run by the daemon, a per-project trust gate, built-in syntax checks for untrusted
projects, diagnostics, quick fixes, formatting, an agent diagnostics tool and a theme model, as
decisions [D18 to D27](#decisions) and stages [E8 to E15](#stages) (revision 5 adds D28 and
E10c), after a study of
[how VS Code does it](#how-vs-code-does-it). Revision 3 folded in the round-2 review (N1 and
the stage-text majors), the review of revision 1 (B1, B2, M1 to M5, the minor findings and nits)
and the root's answers to open questions 1, 2, 3 and 7, recorded as
[settled decisions](#settled-decisions). Revisions 1 to 3 were written against `experimental`
at `243602ab`; revision 4's additions against `a2b26dc6`, revision 5's and 6's against `f17bdca5`. Roadmap
[item 66](../12-roadmap.md#next-in-order). Status: planned (no stage started).

The user asked for "an in-built text editor that can also do markdown presentation". The
**Files** panel of [item 55](terminal-file-panes.md) is a read-only viewer; this plan turns it into
an editor for the person using the app: tabs, unsaved-change markers, find and replace, saves
through the daemon with the same root confinement as item 55's reads, conflict handling when an
agent (or anything else) changes an open file, a live Markdown preview, and a presentation mode
that shows a Markdown file as slides.

The plan's core is three rules:

- **Only the user's editor writes, and only inside the session's folders.** The new write method
  is answered only to the identified desktop app on the local socket, relayed by the app's native
  side (the gate of item 55's terminals, [D2](terminal-file-panes.md#decisions)). An agent's process
  that holds the daemon token cannot use it to step around the permission rules that govern its own
  `write_file` and `edit_file`. Agents get no editor tool and cannot drive the editor.
- **Refusals are judged on what was opened, not on what was asked for.** Every guarded-path check
  runs on the opened handle's final path and on directory identities, so a link, a hard link, a
  short name or a case or trailing-dot spelling cannot reach `.git`, the daemon's folders or a
  guarded file by another name.
- **Neither side's edit is lost silently.** Every save carries the hash of the bytes it was based
  on; the daemon refuses a save whose base no longer matches the file, and the editor shows the
  difference and lets the user merge, keep theirs or take the file's. An agent's full-file
  `write_file` is refused in turn when the file changed since that agent last read it.

Revision 4 adds **language tooling** (the user: "auto-linting for coding languages ... with
different themes as well"), under three more rules:

- **Language servers and linters run in the daemon, never in the webview, and only in projects
  the user trusts.** Running one executes project-controlled code and configuration (build
  scripts, proc macros, ESLint configs and plugins, a project's own TypeScript). An untrusted
  project gets only built-in checks that run no project code: parse errors from the editor's own
  grammars and JSON and TOML checks in the daemon ([D20](#decisions)). When an agent changes a
  file that decides what a trusted project's servers execute (`build.rs`, an ESLint config,
  `.cargo/config.toml` and the rest), the project's servers **pause** until the user reads the
  change and resumes them ([D28](#decisions)).
- **Nothing is installed without the user.** Imperium detects servers already installed and
  offers an install command; the user runs it or approves that one install. No install is ever
  automatic, and an agent's install asks every time ([D19](#decisions)).
- **A fix is an edit like any other.** A quick fix or a format changes the buffer as one undo
  step, or, for other files, is shown as a diff and saved through `files.write` with its base
  hash and confirmations. Agents can read diagnostics, but they fix code with their own
  `edit_file` under the permission rules ([D23, D24](#decisions)).

Each stage below is one pull request against `experimental` of at most about 800 changed lines,
with the docs it changes. The agent engine is this session's to change; stage E4b changes it
(`write_file`'s stale-read refusal and the next-turn note).

## Settled decisions

The root answered these on 2026-11-26; they are no longer open.

- **Secret files** (`.env`, keys and the rest of the safety floor's list) are **editable after a
  one-time confirmation per tab** ([D7](#decisions)). `.git`, `<data>` and `<config>` stay hard
  refused.
- **Harness files** (`.imperium/`, `.claude/`, `.mcp.json`, `.claude-plugin/`) **ask on every
  save**: the dialog names the risk, shows the diff against the last checkpoint (or `HEAD` when
  there is none), and says when an agent changed the file in this session ([D7](#decisions)).
- **Agents and user saves:** `write_file` **refuses a full overwrite of a file that changed since
  that agent's last `read_file` of it** (a last-read record per agent instance), and the next turn may
  carry a note listing the files the user saved ([D10](#decisions), stage E4b).
- **Legacy encodings** stay read-only; a later "reopen with encoding" is editable only for a file
  whose decode and encode round trip is byte-identical ([D8](#decisions)).
- **Installs** (the user's standing rule, recorded in revision 4): every install that an agent
  starts needs the user's explicit approval, each time; installs are never automatic. Language
  servers are installed by the user, or by the user approving one offered install
  ([D19](#decisions)).
- **Agents changing execution-relevant files** (answer to the revision 4 review's B2, which
  replaces open question 11's default): **pause and resume.** When an agent changes an
  execution-relevant file in a trusted project, that project's language servers pause until the
  user clicks *Resume* on a banner that names the files and shows their diff. rust-analyzer's
  `cargo.buildScripts.enable` and `procMacro.enable` are **off per project** until the user turns
  them on. Servers run at the **strictest isolation tier** of the project's open sessions where
  the OS has that tier ([D28](#decisions), [D21](#decisions)).
- **Open questions 7 to 13** (revision 4), answered on 2026-11-26:
  - **7. Languages first:** Rust, TypeScript and JavaScript with ESLint, and Python with Ruff
    are tested with real servers; the others are in the catalogue, tested for discovery and
    start.
  - **8. Ship or detect:** detect installed servers and offer installs; nothing bundled now.
    Bundling Ruff is a possible later step.
  - **9. One trust list:** yes, `mcp.trusted_projects` covers language servers too, with a
    **one-time re-consent** the first time a server would start in a project trusted before
    revision 4 ([D20](#decisions)).
  - **10. Data-only servers in untrusted projects:** no; built-in checks only.
  - **11. Confining servers:** as B2 above (pause and resume, build scripts and proc macros off
    by default, the strictest OS tier of the project's sessions).
  - **12. Agents' `diagnostics` starting servers:** no.
  - **13. Lint on type or save:** on type, debounced, with a per-entry boolean default in the
    catalogue (`lint_on_type`), overridable per language.
- The other [open questions](#open-questions) keep their defaults.

## Goals

Each goal is boolean and has a check that runs in CI, with no manual step. "The fake daemon" and
"browser checks" are item 55's harness ([stage 4](terminal-file-panes.md#stage-4-browser-check-harness):
Playwright in Chromium and WebKit against a Node fake daemon, run by `scripts/check-web.sh` in the
`web` job); "the matrix" is the `rust` job of `ci.yml` on Ubuntu, Windows and macOS.

1. **The user edits and saves a file of the session.** Typing, undo, find and replace, and
   Ctrl+S (Cmd+S) save the file; the bytes on disk are the buffer in the file's own encoding, byte
   order mark and line endings, and the file keeps its permissions, extended attributes and ACLs.
   Check: stage E3a browser checks; stage E1b `cargo test -p imperium-core --test files_write`;
   the stage E7 end-to-end run.
2. **Writes stay inside the session's folders and out of guarded places.** No spelling, link, hard
   link, junction, short name, race or root swap makes `files.write` create, replace or truncate a
   file outside the session's folders and scratch folder, inside a `.git` folder, `<data>` or
   `<config>`, or a guarded file without its confirmation ([D7](#decisions)). Check: stage E1b
   tests on the matrix, with deterministic barriers for the races.
3. **Only the desktop app writes.** `files.write` is refused over loopback TCP, to a paired phone,
   to a caller that is not the installed release `imperium-desktop`, to anything the daemon
   started, and to a mod frame or Preview page in the app's window. Check: stage E1b gate tests,
   stage E2 `tauri::test` access test, stage E7 real frames.
4. **No edit is lost silently.** A save based on stale bytes is refused with the current state; an
   agent's change to an open file with no unsaved edits reloads the buffer and says so; with unsaved
   edits it never overwrites the buffer and never lets a save overwrite the agent's change without
   the user choosing to; an agent's full-file write over a file changed since its last read is
   refused. Check: stage E1b conflict tests; stage E4a browser checks; stage E4b engine tests.
5. **Markdown renders safely.** No raw HTML, script, event handler, `style` attribute,
   `javascript:` or `data:` link, remote request or file outside the session's folders is reachable
   from a rendered Markdown file, and a hostile file cannot hang the window. Check: stage E5
   `npm test`, browser checks and the DOM scan (no dialog, no request to the fake's sentinel URL).
6. **Presentation mode shows a Markdown file as slides.** Split on top-level `---`, navigable by
   keyboard, full window or full screen, announced to screen readers. Check: stage E6 `npm test`
   (splitter) and browser checks.
7. **The editor is accessible and themed.** Every control works by keyboard, focus can always leave
   the editor, syntax colours meet 4.5:1 on the editor background in both themes and all accents.
   Check: `test/tokenContrast.test.ts` extended in stage E3b; browser checks for focus.
8. **A paired phone gets no editor; mods cannot drive it.** Check: `tests/phone.rs` (`files.write`,
   `files.stat` and `files.baseline` refused with `-32005`); stage E2 access test; stage E7 frame
   attempts.
9. **Every check runs in CI.** No manual check, display server outside the job, or network service
   other than package installs.
10. **Problems are shown while typing.** In a trusted project with a server installed, a type
    error appears as a squiggle, a gutter mark and a Problems entry within 2 s of typing it,
    and goes when it is fixed; hover shows the message, and completion offers the server's
    items. Check: stage E11 browser checks against a scripted fake server; stage E15 end-to-end
    run with real `rust-analyzer`, `typescript-language-server` and `ruff server`.
11. **An untrusted project runs no project code.** With the project untrusted, opening and
    editing its files starts no language server, linter, formatter or project binary, and reads
    no server configuration; syntax errors still show. Check: stage E8 and E10b tests (the fake
    server records any start; a project `build.rs` and an ESLint config that write a sentinel
    file leave none), stage E15.
12. **No install happens without the user.** No code path downloads or installs a server
    unasked; an offered install runs only after the user confirms that one install, and an
    agent's install command asks every time. Check: stage E10b tests (no process starts from
    an offer until the confirmation; the floor asks for the catalogue's install commands).
13. **Fixes go through the save path.** A quick fix or format on another file is written only
    after the user accepts its diff, with the file's base hash, so a changed file conflicts and
    a harness or secret file asks. Check: stage E12 browser and daemon tests.
14. **Language servers are capped.** Each runs in an `lsp` cap unit (item 68); one that passes
    its memory cap is stopped, restarted at most three times in ten minutes, and its status
    names the cap and the setting that raises it. Check: stage E9 tests with a fake server that
    allocates past a test cap.
15. **Themes are data and readable.** An imported VS Code theme runs no code and sets only
    validated colours; every shipped theme meets 4.5:1 for syntax colours (7:1 for the
    high-contrast themes) on its background. Check: stage E14 `npm test`.
16. **Project configuration cannot steer probes or installs.** Discovery, `lang.status` and an
    approved install, run with a hostile `.npmrc`, `.cargo/config.toml`, `rust-toolchain.toml`
    and `go.mod` in the open project, use none of them. Check: stage E10b tests (B1).
17. **An agent cannot change what a trusted project's servers run.** After an agent changes an
    execution-relevant file, no server starts, restarts or re-indexes until the user resumes.
    Check: stage E10c tests (B2).

## How VS Code does it

Research for revision 4, from VS Code's documentation, the Language Server Protocol (LSP)
specification 3.17 and the VS Code repositories, read on 2026-11-26.

**Diagnostics.** A server pushes diagnostics with `textDocument/publishDiagnostics`; 3.17 added
pull diagnostics (`textDocument/diagnostic`, `workspace/diagnostic`), where the client asks and
the server answers `full` or `unchanged` against a `resultId`, can declare
`interFileDependencies`, and can ask the client to re-pull with `workspace/diagnostic/refresh`
([LSP 3.17](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/)).
`vscode-languageclient` pulls on change, save or focus as the extension chooses
([`diagnostic.ts`](https://github.com/microsoft/vscode-languageserver-node/blob/main/client/src/common/diagnostic.ts)).
Problems show as squiggles, in the overview ruler, as a count in the status bar and in the
Problems panel (Ctrl+Shift+M), with F8 stepping through them in an inline zone that also offers
fixes ([Basic editing](https://code.visualstudio.com/docs/editing/editingevolved)).

**Lint on type or on save, and debouncing.** Each tool chooses: ESLint runs on type by default
(`eslint.run`: `onType` or `onSave`,
[vscode-eslint](https://github.com/microsoft/vscode-eslint)); the Python linter extensions run
on open and save ([Python linting](https://code.visualstudio.com/docs/python/linting)). The
TypeScript extension debounces its error request by the file's length: a delay of line count
divided by 20, kept between 300 and 800 ms
([`bufferSyncSupport.ts`](https://github.com/microsoft/vscode/blob/main/extensions/typescript-language-features/src/tsServer/bufferSyncSupport.ts)).

**Quick fixes and formatting.** Code actions have hierarchical kinds (`quickfix`,
`refactor.*`, `source.organizeImports`, `source.fixAll`), may be `isPreferred` or `disabled`
with a reason, and carry an `edit`, a `command` or both; a command usually comes back as a
server-initiated `workspace/applyEdit` (LSP 3.17). `editor.codeActionsOnSave` runs chosen kinds
on save, `explicit` (only on a save the user made) or `always` (auto saves too); Refactor
Preview shows a multi-file edit as diffs to accept or untick
([Refactoring](https://code.visualstudio.com/docs/editing/refactoring)). Format on save, type
and paste are settings, off by default
([Basics](https://code.visualstudio.com/docs/editing/codebasics)); `willSaveWaitUntil` edits are
applied before saving, and a client may drop a slow answer (LSP 3.17).

**Finding and starting servers.** Each language is an extension. The client runs in the
extension host and starts the server as a separate process, because "running them in separate
process avoids performance cost"; settings reach the server through `workspace/configuration`;
multi-root support is either one server for all folders (`workspaceFolders`) or one per folder
([Language server guide](https://code.visualstudio.com/api/language-extensions/language-server-extension-guide)).
Tools are found in several ways. ESLint loads the project's own `eslint` package, else a global
one. Ruff ships a binary but prefers the one in the active environment or on `PATH`
([ruff-vscode](https://github.com/astral-sh/ruff-vscode)). rust-analyzer runs build scripts and
proc macros by default (`cargo.buildScripts.enable`, `procMacro.enable`) and `cargo check` on
save, with an overridable command
([rust-analyzer configuration](https://rust-analyzer.github.io/book/configuration.html)).

**Workspace Trust.** Restricted Mode turns off tasks, debugging, the terminal, agents,
workspace settings that name executables, and every extension that has not declared
support ([Workspace Trust](https://code.visualstudio.com/docs/editing/workspaces/workspace-trust)).
Extensions declare `untrustedWorkspaces.supported` as `true`, `false` or `"limited"`, and list
`restrictedConfigurations` whose workspace values are ignored until trusted
([extension guide](https://code.visualstudio.com/api/extension-guides/workspace-trust)).
rust-analyzer, ESLint and the built-in TypeScript extension all declare `false`, and their
manifests give the reason: they run binaries named by configuration, or load and run code and
configuration from the workspace
([rust-analyzer](https://github.com/rust-lang/rust-analyzer/blob/master/editors/code/package.json),
[ESLint](https://github.com/microsoft/vscode-eslint/blob/main/package.json),
[TypeScript](https://github.com/microsoft/vscode/blob/main/extensions/typescript-language-features/package.nls.json)).
On top of trust, ESLint asks again before it first runs a workspace's ESLint library, and Ruff
in an untrusted workspace uses only its bundled binary and ignores `ruff.path` and
`ruff.interpreter`. Trusting a folder trusts its subfolders.

**Themes.** A colour theme is JSON with workbench `colors`, TextMate `tokenColors` (scope
selectors) and, when `semanticHighlighting` is on, `semanticTokenColors`
([Color theme guide](https://code.visualstudio.com/api/extension-guides/color-theme)). Semantic
tokens from a server are layered on the grammar's tokens; a token with no
`semanticTokenColors` rule falls back to a TextMate scope (`function` to
`entity.name.function`, `variable.readonly` to `variable.other.constant`, `parameter` to
`variable.parameter`, and so on), and the standard types and modifiers are fixed by LSP
([Semantic highlight guide](https://code.visualstudio.com/api/language-extensions/semantic-highlight-guide)).
Users pick a colour theme, a file icon theme and a product icon theme; preferred dark, light
and both high-contrast themes follow the OS when `window.autoDetectColorScheme` and
`window.autoDetectHighContrast` are on
([Themes](https://code.visualstudio.com/docs/configure/themes)). The built-in themes are Light
and Dark Modern, Light and Dark+, Light and Dark 2026, the Visual Studio pair and Dark and
Light High Contrast (`uiTheme` `hc-black` and `hc-light`,
[theme-defaults](https://github.com/microsoft/vscode/blob/main/extensions/theme-defaults/package.json)).
Settings Sync carries settings and extensions, so the theme travels with them, and leaves
`machine`-scoped settings behind
([Settings Sync](https://code.visualstudio.com/docs/configure/settings-sync)).

**What it does well, and this plan takes:**

- servers out of the UI process, speaking a standard protocol, so any server works;
- a trust gate, decided once per folder, that turns project execution off by default, with
  workspace settings that name binaries ignored until trust ("restricted configurations"), and
  a second, narrower confirmation before running a project's own copy of a tool (ESLint's
  library approval);
- the Problems list, squiggles, gutter, F8 and Ctrl+. as the shape of the UI;
- fixes and formatting as edits the user can preview, with `explicit` and `always` kept
  separate so auto save does not reformat unasked;
- debouncing by file size, and off-by-default formatting;
- the semantic-token-to-TextMate fallback, so one theme colours both layers.

**Where it is weak, and this plan does differently:**

- **Trust is a single switch that extensions interpret.** Each extension decides what "limited"
  means, the manifest is the only record, and a trusted parent folder trusts every clone below
  it. Here the daemon decides what runs, per server, and trust is per project folder, never
  inherited by subfolders ([D20](#decisions)).
- **Extensions are code.** A theme or a server comes with an extension that can run anything in
  the extension host. Here servers are catalogue entries (a program name and arguments), themes
  are imported as validated data, and no extension code is loaded ([D25](#decisions)).
- **Language servers have no resource limits.** rust-analyzer on a large workspace can take
  several gigabytes, and nothing in VS Code caps it. Here every server is an `lsp` cap unit
  (item 68, [D21](#decisions)).
- **Installs are pushed.** Extensions suggest and auto-update, and some download servers on
  activation. Here nothing downloads by itself ([D19](#decisions)).
- **Server text is trusted as Markdown.** Hover and diagnostic text from a server can carry
  links; here it renders through the D11 pipeline, links behind the confirmation.

## Decisions

Each records the choice, the reasons, the rejected options and when to revisit it.

- **D1. CodeMirror 6, the component item 55 already chose.** Item 55's viewer
  ([D13](terminal-file-panes.md#decisions)) is CodeMirror 6 read-only; the editor is the same
  view with editing extensions turned on, so there is one component, one theme and one set of
  language chunks. Pinned versions (exact, each at least two weeks old on 2026-11-26; item 55's
  stage 7a adds the viewer's packages at these versions, and this plan adds the rest):

  | Package | Version | Used for |
  | --- | --- | --- |
  | `@codemirror/state` | 6.7.6 | documents, transactions, line separator |
  | `@codemirror/view` | 6.43.15 | the view (6.44.0 is one day old) |
  | `@codemirror/language` | 6.13.1 | highlighting, folding, indentation |
  | `@codemirror/language-data` | 6.5.2 | languages loaded on demand |
  | `@codemirror/search` | 6.7.2 | find, replace, go to line |
  | `@codemirror/commands` | 6.11.1 | history, default keys, `indentWithTab` |
  | `@codemirror/lang-markdown` | 6.5.2 | Markdown source mode |
  | `@codemirror/merge` | 6.12.3 | the compare view and its `diff` (D9) |
  | `@lezer/highlight` | 1.2.5 | static highlighting of preview code blocks (D11) |

  All are MIT. Reasons for CodeMirror over **Monaco** (`monaco-editor` 0.58.0, MIT):
  - **Size.** Monaco's package is about 80 MB unpacked and its minimal editor bundle is several
    megabytes before languages; CodeMirror's core is a few hundred kilobytes and languages load as
    separate chunks.
  - **The window's policy.** Monaco needs web workers for its language services built from
    `blob:` URLs in practice, and its loader evaluates modules in ways the policy's `script-src`
    (no `unsafe-eval`) resists. CodeMirror runs on the main thread with no eval. (The preview's
    parse worker of D11 is a bundled chunk from `'self'`, which `default-src 'self'` already
    allows.)
  - **Webviews.** CodeMirror uses `contenteditable` with native selection and input, so IME,
    spell check and the platform's screen reader work in WebView2, WebKitGTK and WKWebView alike.
    Monaco draws its own text layer over a hidden textarea and needs its accessibility mode for
    screen readers; WebKitGTK is its least tested host.
  - **Virtualised rendering** of long files, which item 55 relies on, is built in to both.

  Rejected: Monaco (above); **a `<textarea>`** (no highlighting, no virtualising, no search);
  **Ace** (older model, global state, no virtualised `contenteditable`). Revision 4 adds
  language-server features (D18 to D24) and keeps CodeMirror: Monaco's language services are
  built-in TypeScript, CSS and JSON workers in the webview, which this plan does not want (D18),
  and its LSP support would still need a client, so it would bring size without the feature.
  `@codemirror/lint` 6.9.7 and `@codemirror/autocomplete` 6.20.4 (both MIT; 6.9.8 of lint is
  13 days old) join the pinned table in stage E10a; `@codemirror/lang-json` 6.0.2 and
  `@codemirror/lang-yaml` 6.1.3 come with item 55's `language-data` chunks at those versions.
- **D2. Editing is the user's, not the agents'.** No agent tool, MCP bridge method, hook, mod event
  or skill reaches the editor; nothing an agent says can open, type into or save a buffer. Agents
  keep their own `write_file` and `edit_file`, governed by the permission rules and the safety
  floor ([05](../05-tools-and-isolation.md#permissions)). Reasons: the editor is outside those
  rules (it is the user's hand), so anything that let an agent drive it would bypass them.
  Rejected: an "apply this suggestion in the editor" action from the transcript (transcript text
  can be injected; the user copies and pastes).
- **D3. Writes go through the daemon, behind the desktop-app gate, relayed by the native side.**
  - Item 55's `files.*` reads are answered to any full client, the window's own loopback
    connection included, because agents may read the session's files anyway. A write is
    different: an agent's command at isolation tier 0 can read `daemon.json`, and a write method
    open to any token holder would let it write without the permission ask, the safety floor or
    the audit of its own tools.
  - So `files.write` is answered only when the connection passes the gate of item 55's terminals:
    `app_identity::InstalledApp` and `peer::may_grant`, checked once per connection and cached
    ([D2 of item 55](terminal-file-panes.md#decisions)). **Single owner:** item 55 stage 2b
    creates `crates/imperium-core/src/app_gate.rs` (the `TerminalGate` trait, named `AppGate`
    from the start for both users); stage E1b depends on it and only adds `files.write` as a
    caller. Changes to the gate after that go through 2b's tests and this plan's together.
  - **Release builds only.** A release daemon refuses the gate to a debug build of
    `imperium-desktop` (the build profile is read from the identified executable's embedded build
    info, which `build.rs` writes). A debug daemon accepts a debug app beside it, so development
    works; the docs say that this path exists only for development builds, which are never shipped.
  - **WebView2 policy keys.** WebView2 reads per-user policy from
    `HKCU\Software\Policies\Microsoft\Edge\WebView2\`, where `AdditionalBrowserArguments` can add
    `--remote-debugging-port` and `BrowserExecutableFolder` can point the app at another browser
    runtime. The native side reads both (and `ReleaseChannelPreference`, `ChannelSearchKind`) for
    `imperium-desktop.exe` and `*` at start, beside the refused `WEBVIEW2_*` variables
    (`imperium_ipc::refused_variable`); if any is set, `files_write` and the terminal commands are
    refused, and the panel says why. `HKLM` policy needs an administrator and is not checked.
  - **Tauri version floor.** `CVE-2024-35222` (frames could reach IPC commands meant for the
    app's own origin) is fixed in Tauri 2.0.0-beta.20; the app is on Tauri 2.12. Stage E2 adds a
    test that fails if `Cargo.lock`'s `tauri` is older than 2.0.0-beta.20 or than the version the
    access test was written against, so a downgrade cannot slip in.
  - The webview never calls `files.write` itself. The app's Rust side
    (`src-tauri/src/file_write.rs`, built like item 55's terminal relay and `plugin_approval.rs`)
    holds its own local-socket connection, takes no daemon lease, and exposes one Tauri command,
    `files_write`, granted in `capabilities/default.json` to `main` and `panel-*` for the app's own
    origin only, so a mod frame or Preview page is refused by Tauri's access control.
  - Writes are refused while a trusted main-context mod is or was loaded, with item 55's latch
    ([D2](terminal-file-panes.md#decisions)): such a mod runs in the window that holds the editor.
  - There is no direct file access from the webview: no Tauri `fs` plugin, no `dialog` save to an
    arbitrary path. `@tauri-apps/plugin-dialog` stays as it is (it is used for folder pickers, not
    for file contents).
  - This is **not a boundary against a same-user process trying to evade it**, as item 55 says of
    `may_grant`; it stops the plain case, an agent's shell or an `imperium` command reaching for a
    write.
  - Rejected: **answering writes to any full client** (above); **a native confirmation per save**
    (friction on every save, no gain over the gate); **the Tauri `fs` plugin with a scope** (a second
    path checker that must agree with the daemon's, no root identities, no link-free opening, and
    nothing for a remote daemon later).
- **D4. Paths and opening: item 55's rules, for writing, on a judged handle.** Paths are
  `{root, relative}` pairs only; roots are `Workspace::roots()` and `Workspace::scratch()` with
  identities recorded at creation; the same up-front spelling checks (no `..` past the root,
  absolute, drive, UNC, `\\?\`, stream or reserved device names); every open through the held root
  `Dir` with no link followed out of the root ([D12 of item 55](terminal-file-panes.md#decisions)).
  For a write (and for `files.stat`, `files.baseline` and the editable read) the open is done in a
  fixed order, so what is judged is exactly what is written:
  1. **Open the parent first.** The parent directory is opened from the root one component at a
     time, each component opened without following a link (`openat2` with `RESOLVE_BENEATH` and
     `RESOLVE_NO_MAGICLINKS` on Linux, cap-std's no-follow open elsewhere); a component that is a
     link inside the folders is resolved by reading it and continuing from its target, again one
     component at a time, so no step follows a link out of the root.
  2. **Open the target relative to the parent without following its last component**
     (`O_NOFOLLOW`; on Windows `FILE_FLAG_OPEN_REPARSE_POINT`, refusing a reparse point). A last
     component that is a link inside the folders is resolved as in step 1 (its target's parent
     becomes the parent) and the target is opened again the same way. The target is opened without
     write access first, `fstat`ed, and must be a regular file with **exactly one link**
     (`nlink == 1`); a FIFO, socket or device is refused without opening it for writing.
  3. **Check the device.** The target's device (volume serial on Windows) must equal the parent's,
     so a mount point or a junction to another volume swapped in as the last component is refused.
  4. **Judge** the parent's final path plus the target's name, and the parent's identity chain
     (D7). On Unix the chain is built upward from the parent's descriptor with
     `openat(fd, "..")` and `fstat` at each step, up to the file system's root, so it describes
     the directories actually opened, not a path that might since have been renamed; on Windows
     from the parent handle's final path, each ancestor opened by handle and its file index read.
  5. Keep the parent open for the rest of the operation; the save's temporary file, rename and any
     in-place write all go through it (D5). The judgement is repeated on the held parent just before
     the rename (cheap: one final-path read and the chain).
  - **Creating a file** (`create: true`) opens with `O_EXCL` (`create_new`) relative to the held
    parent and creates no directories; its name passes the same spelling checks.
  - **On Windows**, every path component on **every** operation (read, stat, write, create, the
    temporary file and the rename) is refused when it ends in a dot or a space or is a short (8.3)
    name that differs from the component's long name; the parent's final path is the check of
    record, so a spelling that slipped past these is still judged by what it opened.
  - **Residual:** an attacker who can already rename directories inside the folders can move an
    ancestor of the held parent into a guarded place between the last judgement and the rename;
    the rename then lands there. Such an attacker can write there directly anyway; it is listed
    under [residual risks](#residual-risks).

  Rejected: `Workspace::resolve_writable` (it accepts the loose spellings models write and
  includes path grants, which are agents' permissions, not the user's folders); **opening the
  target by its full path and judging afterwards** (a link as the last component would be
  followed before it could be judged).
- **D5. Atomic replace, keeping what the file was.**
  - **The sequence.** Under the file's lock (D6), with the parent directory held open (D4): create
    a temporary file in that directory through the held handle (`.<name>.imperium-<random>.tmp`,
    `O_EXCL`); write it and flush it (`sync_all`; on macOS `fcntl(F_FULLFSYNC)`, because `fsync`
    there does not reach the disk); copy the old file's metadata onto it, handle to handle (below);
    re-judge the parent (D4 step 5); rename it over the target **relative to the held parent**; on
    Unix flush the parent directory (`fsync`, `F_FULLFSYNC` on macOS). A crash leaves either the old
    or the new file, never a truncated one.
  - **Handle-relative rename.** Unix: `renameat` through the held `Dir` (cap-std's `Dir::rename`),
    so a parent swapped for a link after it was opened cannot redirect the rename. Windows, in a new
    opt-in unsafe module `crates/imperium-ipc/src/windows/replace.rs` (the workspace allows `unsafe`
    only there), in this order, each tried only when the previous is refused as unsupported:
    1. `SetFileInformationByHandle` with `FileRenameInfoEx` on the temporary file's handle,
       `RootDirectory` set to the held parent handle, flags `FILE_RENAME_FLAG_REPLACE_IF_EXISTS`
       and `FILE_RENAME_FLAG_POSIX_SEMANTICS` (NTFS and ReFS on Windows 10 1709 and later);
    2. `FileRenameInfo` with `RootDirectory` and `ReplaceIfExists` (older NTFS builds);
    3. the in-place write below, with a note. FAT, exFAT and SMB shares take this path: their
       drivers refuse POSIX semantics, and SMB requires `RootDirectory` to be zero, which would
       make the rename path-based, so it is not used.
  - **Metadata copied** from the old file's handle to the temporary file's handle before the
    rename:
    - Linux: the mode (`fchmod`), the group when the daemon's user may set it (`fchown(fd, -1,
      gid)`), and every extended attribute (`rustix` `flistxattr`, `fgetxattr`, `fsetxattr`),
      which carries POSIX ACLs (`system.posix_acl_access`) and the SELinux label
      (`security.selinux`);
    - macOS: `fcopyfile` with `COPYFILE_METADATA` (mode, ACL, extended attributes, the quarantine
      flag included) and the group as on Linux, in a new opt-in unsafe module
      `crates/imperium-ipc/src/macos/copy_metadata.rs`;
    - Windows: the security descriptor (owner, group, DACL, label) through `GetSecurityInfo` and
      `SetSecurityInfo` on the handles, the basic attributes through `FileBasicInfo`, and every
      named stream (`Zone.Identifier` included) listed with `FileStreamInfo` and copied by opening
      each stream relative to the file handle, in `replace.rs`.
  - **Written in place instead** (with a `note` in the save's result) when: the file's owner is
    not the daemon's user; its group cannot be restored; any metadata copy fails; the file is
    EFS-encrypted, compressed, sparse or has an integrity stream (Windows attributes
    `FILE_ATTRIBUTE_ENCRYPTED`, `_COMPRESSED`, `_SPARSE_FILE`, `_INTEGRITY_STREAM`, whose state a
    new file would not carry); or the rename fell through to step 3. The docs say a crash during an
    in-place write can leave a mixed file. Hard-linked files are not written at all (D7).
  - **In-place without a path race.** The in-place write never reopens the file by its path:
    Windows `ReOpenFile` on the handle already opened and judged, with write access; Linux
    `open("/proc/self/fd/<n>", O_WRONLY)` on that descriptor; macOS `openat(parent, name,
    O_WRONLY | O_NOFOLLOW)` followed by an `fstat` whose file id must equal the judged one
    (refused otherwise). The new bytes are written from offset 0 over the old content, the file is
    then truncated to the new length (`ftruncate`, `SetEndOfFile`) and flushed, so a crash before
    the truncate leaves the new bytes followed by old ones, never a short file.
  - **Windows sharing violations** (another program or antivirus holding the target or the
    temporary file): the rename is retried after 50, 100, 200, 400 and 800 ms; still refused, the
    save falls back to in-place if the handle can be reopened for writing, and otherwise fails with
    `FILE_BUSY` (D6).
  - **Leftover temporary files** (from a crash) are removed when the root is next opened: only
    names matching the pattern, only regular files with `nlink == 1`, older than a day, opened and
    removed through the held `Dir`.
  - Rejected: **always in place** (a crash or full disk leaves a mixed file); **`ReplaceFileW`**
    (it keeps ACLs and streams, but takes paths, so a parent swapped between the check and the call
    redirects it); **renaming by path** (the same race).
- **D6. Saves compare identity and content hash; one lock per file in the daemon.**
  - **Reading an editable file is one consistent read.** `files.read` of a text file up to the
    editable cap (D8) returns **the whole file in one response**, read from one open handle; its
    `hash` (SHA-256, `sha2` is in the lock already) is computed over exactly the bytes returned,
    with `file_id` (device and inode, or volume serial and file index) and `size` from the same
    handle's `fstat`. So the hash the editor holds always describes the text it shows. Larger
    files stay in item 55's paged, read-only view; there each page carries `{file_id, size,
    modified_ms}` and the viewer discards and re-reads from the start when a page's differ from the
    first page's.
  - **Saving.** `files.write` carries `base: {hash, file_id}` (or `null` with `create: true`).
    Under the lock below the daemon opens the target (D4), checks it is the same file and hashes
    its bytes; if either differs it writes nothing and fails with the new error `FILE_CONFLICT`
    (the next free code, `-32010` today), whose data is the current `{hash, file_id, size,
    modified_ms}` (or `deleted`). `etag` (item 55's size, time and partial hash) is used only to
    skip re-reads, never to accept a save.
  - **One lock per file, shared with every writer in the daemon.** A new
    `crates/imperium-tools/src/file_locks.rs` keeps an async mutex per **lock key** (weakly held,
    so the table does not grow). The key is the **final path of the opened handle** (Linux:
    `readlink` of `/proc/self/fd/<n>`; macOS: `fcntl(F_GETPATH)`; Windows:
    `GetFinalPathNameByHandleW` with `FILE_NAME_NORMALIZED`), case-folded on a case-insensitive
    volume. For a file that does not exist yet it is the final path of its **nearest existing
    ancestor** joined with the remaining components (case-folded the same way), so two spellings
    of a missing file in a missing folder get one key. Not the requested spelling (aliases would
    get different locks) and not the file id (an atomic replace changes it). The holders:
    - `files.write`, around compare and replace; it waits at most 5 s and then fails with the new
      error `FILE_BUSY` (the next free code after `FILE_CONFLICT`);
    - `write_file` and `edit_file` (`imperium-tools/src/files.rs`), around their read and write.
      They take it **after** the permission check, any approval and the `tool.before` hook have
      finished (those run in the engine's dispatch before the tool body), so a waiting approval
      never holds a lock, and **re-open the file after taking it** (the key is computed from a first
      open; the read and write use a fresh open made under the lock, and if its final path differs
      from the key the tool drops the lock and starts again, at most three times);
    - `session.rewind` (restoring a checkpoint) and `session.review_hunk` when it reverts, around
      each file they rewrite; an operation that rewrites several files takes all their locks first,
      **in sorted key order**, so two multi-file operations cannot deadlock. A rewind takes its
      locks **only once the session's turn and every subagent are idle or stopped** (it already
      stops the turn first), and never waits on an in-flight tool while holding any lock: if a tool
      still holds one of its keys, it releases everything it took, waits for the tool to finish and
      starts again.
  - A command (`run_command`, a build, another editor) can still change the file between the
    compare and the rename; that window is the time to hash and rename and is a stated
    [residual risk](#residual-risks).
  - Rejected: **compare by `etag` only** (a same-size edit within the clock's resolution keeps
    it); **advisory OS locks** (`flock` and Windows share modes are ignored by most programs and
    would block agents' tools on Windows); **paging editable files** (a page boundary can fall
    between two versions of the file).
- **D7. Guarded targets, judged on what was opened.**
  - **How a target is judged.** D4 gives the parent's final path, the target's name and the
    parent's identity chain; those are judged, never the requested spelling. The name checks reuse
    the safety floor's own resolved-form check (`imperium_policy::permissions::safety_floor` with
    `CallKind::Edit` on the final path, which already folds case on Windows and macOS and covers
    `.git` anywhere in the path and the floor's file list), so the editor and agents' tools cannot
    drift. Because the final path comes from the handle, a symbolic link inside a root to
    `.git/hooks/pre-commit`, a short name (`GIT~1`), a trailing dot (`.git.`) and a case variant
    (`.GIT`) are all judged as the `.git` folder they opened.
  - **Identity, not only names.** The daemon records at start the file identities of `<data>`,
    `<config>` and of the parts of `<data>` that hold sessions' files (below), and at root open the
    identities of each repository's git folder and common folder (`git rev-parse --git-dir
    --git-common-dir`, through `safe_git`). A directory in the target's identity chain that equals
    one of the refused identities is refused whatever it is called (a bind mount, or a folder
    reached through a link).
  - **The daemon's folders, with the parts that are sessions' folders allowed.** `<data>` holds
    session roots, so refusing all of it would make default sessions read-only:
    `workspaces.root` (by default `<data>/workspaces`) holds the Home environment's workspace and
    every session's scratch folder, and `<data>/worktrees` (and the configured worktree folders)
    holds auto-fix and data-mode worktrees. The rule is therefore: a target whose identity chain
    contains `<data>` or `<config>` is refused **unless** the chain, below that point, passes
    through the identity of `workspaces.root` or of a worktree folder, which are judged as ordinary
    session folders (D4 has already confined the target to a root of this session). Named, so the
    docs and tests can list them, the refused parts are: `daemon.json`, `ipc/`, `secrets.*`
    (`secrets.enc`, `secrets.key`, `secrets.lock`, `secrets.backend`), `devices.json`, `remotes/`,
    `marketplaces/`, the plugin records and approvals (`plugin-installs.json`,
    `plugin-migration.json`, `marketplaces.json`, `mod-approvals.json`, and from revision 4
    `lsp-approvals.json`, D20), `mods/`, `plugins/`,
    `config.toml`, sessions' journals and every other file of `<data>` and `<config>` outside the
    allowed folders.
  - **Hard refusals** (`-32005`, and the file opens read-only with the reason): anything in a
    `.git` folder or a repository's git or common folder; the daemon's folders as above; a file
    the editor cannot decode losslessly (D8); a file with **more than one hard link**, whose reason
    reads "This file has other names (hard links), as files in package stores such as pnpm's and
    uv's do; editing it here would change them too." A file system that reports `nlink == 1` for
    every file (some network and FUSE file systems) passes this check; that is a safe split,
    because the save then replaces only this name (D5's rename) and the other names keep the old
    content. On FUSE and NFS file systems (found from `fstatfs`), whose link counts may be
    synthetic, the in-place fallback is refused rather than taken, so a save there that cannot
    rename fails with the reason instead of writing through other names. SMB shares report the
    server's own link count (`FileStandardInformation`), so the check holds there and their
    in-place saves (D5) are allowed.
  - **Secret files** (the safety floor's file list: `.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`,
    `id_ed25519*`, `id_ecdsa*`, `.npmrc`, `.pypirc`, `.netrc` and the rest) are editable after a
    **one-time confirmation per tab** ("This file holds secrets. Agents ask before writing it.
    Edit it here?"). `files.write` requires `confirmed: ["secret"]` for such a file and otherwise
    fails with `-32005` and `data: {needs: "secret"}`.
  - **Harness files** (`.imperium/`, `.claude/`, `.mcp.json`, `.claude-plugin/`: hooks, skills,
    commands, agents, MCP servers, plugins and environment config, which change what agents run)
    **ask on every save**. The dialog names the risk ("This file changes what agents in this
    project run"), shows the diff between the buffer and the file at the last checkpoint (or
    `HEAD`, or "new file"), fetched with the new `files.baseline`, and says "An agent changed this
    file in this session" when one did. `files.write` requires `confirmed: ["harness"]`.
    `files.baseline` applies the same root confinement and opening as `files.read` and is refused
    to a phone.
  - The confirmations are checked by the daemon so a webview bug cannot skip them, but they are
    the user's own guard against surprises, not a boundary: the gate (D3) is the boundary.
  - Agent deny rules do not apply: they say what agents may touch, and the editor is the user's, as
    item 55's reads are.
- **D8. Encodings, line endings and size.**
  - **Editable encodings:** UTF-8 with or without a byte order mark, and UTF-16 LE or BE with a
    byte order mark (`encoding_rs` is in the lock already). A file is editable only when it decodes
    without loss: invalid UTF-8, BOM-less UTF-16 and legacy encodings open read-only with a note.
    A later "reopen with encoding" choice may make a legacy file editable only when decoding and
    re-encoding it gives back exactly the same bytes; it is not in this item. The save encodes the
    text back with the same encoding and BOM; the daemon, not the webview, encodes, and
    `files.write` takes `encoding` and `bom`.
  - **Line endings:** `files.read` reports `eol` (`lf`, `crlf`, `mixed` with the majority, or
    `none`). The buffer uses `\n` internally (CodeMirror's default) and the save joins lines with the
    file's ending (`doc.sliceString(0, length, eol)`), so untouched lines keep their bytes. A
    `mixed` file is saved with its majority ending; the status bar says "Mixed line endings: saving
    as CRLF" before the first save, and the ending can be changed there. A lone CR is treated as
    `mixed`. The final newline is kept as it was; nothing is trimmed or inserted on save.
  - **Size:** a file is editable when its **encoded bytes** are at most 4 MiB, both when read and
    when saved: the relay refuses a `text` whose UTF-8 length is over 4 MiB before sending, and the
    daemon refuses a request whose `text` parameter is over 4 MiB as received or whose encoded
    bytes are over 4 MiB, with `-32602`, before writing anything. Highlighting is off above 1 MiB
    or for a line over 10,000 characters (item 55); the Markdown preview is off above 1 MiB. Larger
    files open in item 55's capped read-only view and say why. Binary and image files are never
    editable.
- **D9. Agent edits and user edits in the editor: reload clean buffers, never overwrite dirty ones.**
  - **Noticing a change.** The editor re-checks an open file's `{hash, file_id}` through
    `files.stat` (new, cheap: size, time and identity, with the hash computed only when size and
    time moved) on: a `tool_result` of `write_file` or `edit_file` naming the file in any session the
    window follows; window focus; the tab becoming visible; and every 2 seconds for the visible tab
    while the window has focus. No file-system watcher (item 55's non-goal): a `notify`-based
    watcher means per-OS quirks, watch limits on Linux and network drives that do not report; the
    poll costs one `fstat` per visible tab. Revisit if the poll shows in profiles.
  - **A clean buffer** (no unsaved edits) takes the new text at once, applied as the minimal
    change set (`@codemirror/merge`'s `diff`) so the cursor, selection, folds and scroll stay where
    they were relative to unchanged text. The change is one undo step: Ctrl+Z returns to the old
    text, which makes the buffer dirty. A notice in the tab says who changed it when known ("Changed
    by the agent in this session" from a tool result, else "Changed on disk") with *Show changes*.
  - **A dirty buffer** is never changed under the user. The tab shows a conflict banner:
    - *Compare*: a merge view (`@codemirror/merge`, side by side, or unified in a narrow panel) of
      the file on disk against the buffer, with per-chunk *Take theirs* and *Keep mine*;
    - *Merge*: a three-way merge of the base (the text the buffer was loaded from), the buffer and
      the file; when no changed regions overlap it applies cleanly as one undo step and the base
      moves to the file's new hash; when they overlap it opens *Compare* at the first conflict;
    - *Use file*: replace the buffer with the file (one undo step);
    - *Keep mine*: the next save overwrites the file, confirmed once ("Replace the agent's change
      to `src/lib.rs`?"); the save is sent with the file's new hash as its base, so a further
      change in between is still refused.
  - **A save that meets a conflict** (`FILE_CONFLICT`) shows the same banner; nothing is written.
    `FILE_BUSY` says the file is being written by an agent or a rewind and retries once after 2 s.
  - **An agent's `edit_file` after a user save** reads the file from disk, so it applies to the
    user's text or fails to find its old string; an agent's **full-file `write_file`** built from
    text read before the user's save is refused (D10). If one still lands (an agent in a session
    that never read the file), the reload notice offers *Restore my save*: the editor keeps the
    user's last saved text in memory per tab and re-saves it on request, with the agent's text as
    the base.
  - **The Review panel** shows the working tree against the checkpoint, so the user's saves appear
    there as changes in the session, not attributed to the agent; checkpoints taken before the
    next turn capture them, so a rewind undoes them too
    ([14](../14-projects-and-automation.md#checkpoints-and-rewinding)). The docs say so.
  - **Deleted or renamed on disk:** the tab says the file is gone and keeps the buffer;
    *Save* recreates it (a create, `O_EXCL`), *Close* discards.
  - Rejected: **locking a file while it is open in the editor** (agents would fail mid-turn for a
    tab left open); **always reloading** (loses unsaved work); **last writer wins** (the item's
    requirement).
- **D10. Agents learn about user saves.**
  - **Stale full overwrites are refused.** Each **agent instance** (the session's root agent, and
    each subagent, keyed by session id plus agent id) keeps a **last-read record** per file, by
    lock key (D6):
    - `read_file` records the SHA-256 of the whole file as it was when read, for files up to
      4 MiB; above that it records `{file_id, size, modified_ms}` instead of a hash, and a paged
      read whose pages disagree on those records `inconsistent`;
    - `write_file` and `edit_file` record what they wrote.
    `write_file` over an **existing** file is refused when the current file differs from the
    record (by hash, or by `{file_id, size, modified_ms}` above 4 MiB) or the record is
    `inconsistent`, with a message the model can act on: "`src/lib.rs` changed since you last read
    it; read it again before replacing it". The words "(saved by the user in the editor)" are added
    only when the daemon's record of user saves (below) shows a `files.write` of that file whose
    resulting hash equals the current one. A file the agent instance never read is written as today
    (no behaviour change for new files or first writes), and `edit_file` is unchanged (it re-reads
    and matches exact text).
  - **Per agent, not per session.** Two parallel subagents A and B that each read a file, after
    which A writes it, leave B's record stale, so B's later full overwrite is refused (it would
    discard A's change); with a per-session map B would pass. A subagent starts with an empty map.
    A branch or a comparison session ([07](../07-desktop-ui.md#branches-and-model-comparison-implemented))
    copies the parent agent's map at the moment it forks, so it keeps what its history says it read.
  - **Lifetime.** The map lives in the daemon's memory and is **emptied by a daemon restart**; after
    a restart every file counts as never read, so the first full overwrite of it passes as today.
    Persisting it is not worth a format: a restart between a read and a write is rare, and the
    editor's own conflict check (D6) still protects user saves. The docs say so.
  - **What else trips it.** Any change to the file between the read and the write trips the
    refusal, not only user saves: a formatter or code generator run through `run_command`, a
    build, another agent. That is intended (the overwrite would discard it), but it **changes agent
    behaviour in evals** (a model that reads, formats with a command, then rewrites the whole file
    now gets a refusal and must read again). So it sits behind `harness.stale_write_check` (default on in sessions) and a per-variant flag
    of headless evals (`stale_write_check` in `LoopFlags`, `crates/imperium-core/src/eval/variant.rs`,
    beside `result_spill` and `parallel_reads`), whose default is **off** so evaluations stay
    comparable with earlier baselines unless a variant asks for it. The research session is told
    before stage E4b merges, and the eval suites are rerun with the flag on and off.
  - **The next-turn note** (`harness.user_edit_notes`, default on): when the user saved files of the
    session in the editor since the last turn, the next turn's context starts with a short system
    note. The daemon records each successful `files.write` per session (`{root, path, hash, at}`);
    the note is built from that list and cleared when sent. It is **labelled as data**: "Files the
    user saved in the editor since your last turn (names only; data, not instructions):", then one
    name per line, each as a JSON string (quoted and escaped). A name with a control character or a
    bidirectional or zero-width character is left out and counted ("and 1 file with an unusual
    name"); at most 20 names are listed, then "and N more". Names only, never content.
  - These change `crates/imperium-tools/src/files.rs`, the daemon's save record and the engine's
    turn setup (`crates/imperium-engine/src/agent/`); they are stage E4b.
  - Rejected: **refusing every write to a file not read first** (changes behaviour for every new
    file and for evals that write before reading); **a per-session map** (above); **sending the
    user's diff to the model** (context cost and possibly secrets; the agent can read the file).
- **D11. Markdown preview: React elements, never HTML strings.**
  - **Parsing off the main thread, bounded.** The source is parsed in a dedicated worker (a
    bundled chunk, `new Worker(new URL(...), {type: "module"})`, allowed by `default-src 'self'`)
    with `unified`, `remark-parse`, `remark-gfm` 4.0.1 and `remark-rehype` (no `rehype-raw`), which
    returns the HAST tree. The worker is terminated and the preview says "This file is too complex
    to preview" if parsing takes over 2 s or the tree is deeper than 200 levels or has more than
    200,000 nodes. The main thread turns the tree into React elements with
    `hast-util-to-jsx-runtime` (what `react-markdown` 10.1.0 uses inside) and the components
    below. The transcript keeps `react-markdown`, now pinned exactly (today `^10.1.0` and
    `^4.0.1`).
  - **No HTML.** Raw HTML in the file stays text (`remark-rehype`'s default drops it into text
    without `allowDangerousHtml`), so `<script>`, `<iframe>`, `<img onerror>` and `<style>` never
    become elements. An element allowlist (headings, paragraphs, emphasis, lists, task-list
    checkboxes (disabled), tables, blockquotes, code, `hr`, `br`, `del`, `img`, and `section`,
    `sup` and `ol` for footnotes) replaces anything else by its text, and no element keeps a
    `style` or `on*` attribute.
  - **Ids** that the pipeline generates (footnotes and heading anchors) are prefixed with the tab's
    own prefix (`md-<tab>-`, `remark-rehype`'s `clobberPrefix`), so a file cannot produce an id
    that collides with the app's own elements. Footnotes are supported: references are `sup`, the
    notes a `section` at the end, and the jump links buttons (below).
  - **Links** are never `<a>` elements: every link becomes a `<button>` styled as a link, so
    there is no `href` to navigate. URLs pass `react-markdown`'s `defaultUrlTransform` first (which
    empties `javascript:`, `vbscript:` and unknown schemes), then the classifier: `#anchor` scrolls
    the preview (to the prefixed id); a relative path to a file inside the session's folders opens
    it in the editor (through `files.locate`); `http`, `https` and `mailto` open only after a
    confirmation that shows the full address, the host in punycode and the visible text when it
    differs, through the one link function item 55 builds (`src/terminal/policy.ts`, moved to
    `src/shared/links.ts` by whichever stage needs it second), then `openExternal`. Everything else
    (`data:`, `file:` outside the folders) is shown as text with a tooltip saying why it does not
    open.
  - **Images** (`components.img`): only a relative path inside the same root as the Markdown file,
    resolved against the file's folder and normalised in a pure function (a path that climbs out of
    the root is refused before any call), fetched with `files.read`, and shown only when the daemon
    found image magic bytes (item 55's rule, up to 10 MiB each) as a `data:` URL (`img-src 'self'
    data:` already allows it). They load **lazily** (when within one screen of view), at most 50
    per file and 40 MiB in all; past either cap the rest show placeholders saying so. SVG is not
    shown as an image (it can carry script and remote references); it shows as a placeholder. A
    remote image is **never loaded**: the window's policy blocks remote images anyway, and a remote
    image in a file the agent wrote is a tracking beacon. It shows as a placeholder with its alt
    text and host and *Open in browser* behind the link confirmation. The roadmap text is updated
    to match.
  - **Code blocks** are highlighted with `@lezer/highlight`'s `highlightCode` into React spans
    (languages from `@codemirror/language-data`), never `innerHTML`.
  - **Updates and scroll:** the preview re-parses 150 ms after typing stops; each block carries
    its source line (HAST `position`), so the preview follows the editor's scroll and a click on a
    preview block moves the cursor there.
  - Not supported: Mermaid, math and embedded HTML ([open question 5](#open-questions)).
  - Rejected: **`marked` or `markdown-it` with DOMPurify** (HTML strings into `innerHTML`, one
    sanitiser between a file and the DOM); **`rehype-sanitize` with `rehype-raw`** (lets some HTML
    through, to no benefit in a local editor); **parsing on the main thread** (a crafted file of
    deeply nested lists or emphasis can stall the window).
- **D12. Presentation mode.**
  - **Splitting** (`src/editor/slides.ts`, pure, tested by `npm test`): the file is parsed with the
    same pipeline (in the worker) and split at **top-level thematic breaks** (`---`, `***`, `___`),
    using node positions to slice the source, so a `---` inside a code fence, a list or a
    blockquote and a setext heading's underline (`Title` over `---`) do not split. A leading YAML
    front-matter block (`---` on line 1 to the next `---` or `...` within the first 100 lines) is
    dropped. A file with no top-level break splits before each level 1 and level 2 heading (item
    66's "or headings"). An empty slide is dropped.
  - **Showing:** *Present* (toolbar, palette, **Start presentation**, F5 by default, rebindable)
    opens an overlay over the whole window (a portal to the body, above every panel, `role="dialog"`
    with `aria-modal`, each slide a `section` with `aria-roledescription="slide"` and a label "Slide
    3 of 12"; a polite live region announces the change). Slides are laid out at 16:9 and scaled to
    fit; the slide renders with the preview's components (D11), so images and links follow the
    same rules. *Full screen* (F) puts the window itself in full screen through Tauri's
    `setFullscreen` (permission `core:window:allow-set-fullscreen` for `main` and `panel-*`) and
    restores the previous state on exit; in a detached panel window it fills that window.
  - **Keys:** Right, Down, Page Down, Space and Enter go forward; Left, Up, Page Up and Backspace
    back; Home and End to the ends; a number then Enter jumps; Esc leaves and returns focus to the
    editor at the slide's first line. Focus stays in the overlay while it is open.
  - **Live:** edits while presenting (in another window) re-split and keep the current slide by
    its source position. Motion: none under reduced motion, a short fade otherwise.
  - **Theme:** the app's theme by default; Settings → Editor → *Presentation theme* (app, light,
    dark) overrides it, with the light or dark tokens set on the overlay only.
  - **Read-only files present too:** preview and presentation need only item 55's reads, not the
    write path, so they work on any Markdown file the viewer can show.
  - Not in this item: speaker notes, a presenter view on a second screen, export to PDF or PPTX,
    slide transitions ([open question 3](#open-questions)).
  - Rejected: **reveal.js or Marp** (each brings its own HTML rendering, themes and, for reveal.js,
    plugins that run scripts; the preview's components already render safely).
- **D13. Keyboard and accessibility.**
  - CodeMirror's default keymap (undo, redo, selection, multiple cursors, `Mod-f` find,
    replace in the same panel, `Mod-Alt-g` go to line, comment toggles), with `indentWithTab`; when
    Tab indents, **Esc then Tab** (CodeMirror's escape hatch) and **F6** (*Leave editor*, the same
    rebindable key as item 55's *Leave terminal*) move focus to the next panel, so the keyboard is
    never trapped. The editor's own keys are listed in Settings → Keyboard as fixed, and a
    rebindable app shortcut that would shadow one is refused there with the reason.
  - New rebindable shortcuts in `src/settings/shortcuts.ts`: **Save** (`Mod+S`), **Save all**
    (`Mod+Shift+S`; not `Mod+Alt+S`, which is AltGr on many layouts), **Close tab** (`Mod+W`, only
    while the editor has focus), **Toggle Markdown preview** (`Mod+Shift+V`), **Start
    presentation** (`F5`), **Leave editor** (`F6`).
  - Tabs follow the WAI-ARIA tabs pattern (arrow keys between tabs, Delete closes); the unsaved
    marker is a dot with the accessible name "unsaved changes", never colour alone; banners are
    `role="status"` (reload notices) or `role="alert"` (conflicts); the editor's
    `aria-label` is the file's relative path.
  - Item 55's bidi and zero-width markers stay on while editing.
- **D14. Themes: CSS variables, so the theme switch needs no rebuild.** The CodeMirror theme
  (`EditorView.theme`) and `HighlightStyle` use only CSS custom properties: the existing surfaces
  (`--panel`, `--fg`, `--muted`, `--border`, `--accent`, `--focus`) and new `--syntax-*` tokens
  (keyword, string, number, comment, type, function, variable, operator, heading, link, invalid,
  plus `--editor-selection`, `--editor-active-line`, `--diff-added`, `--diff-removed`) defined for
  dark at `:root` and for light at `:root[data-theme="light"]` in `styles/tokens.css`, per accent
  where they use the accent. `test/tokenContrast.test.ts` checks every syntax colour at 4.5:1 on the
  editor background in both themes and for all twelve accents; `styles/forced-colors.css` maps
  selection and the active line to system colours. The code font and `--code-scale` are the app's.
- **D15. Settings.** App-side (`src/settings/settings.ts`, Settings → Editor; they are
  preferences of this window, not of the daemon): *Tab size* (2, 4, 8; default 4, and a file's own
  indentation wins when detected), *Indent with* (spaces, tabs, detect; default detect), *Word wrap*
  (off for code, on for Markdown by default), *Line numbers* (on), *Markdown preview* (beside the
  source, or off; default beside), *Presentation theme* (D12), *Auto save* (off, after 1 s idle, on
  focus change; default off). Auto save still goes through the conflict rules and never answers a
  confirmation: a conflict, a secret file not yet confirmed or a harness file stops it and shows
  the banner or dialog. Daemon-side, user layer only (in the registry, not on the environment
  layer's allow-list): `files.write_enabled` (bool, default `true`): off refuses `files.write` and
  the editor opens everything read-only; `harness.user_edit_notes` (bool, default `true`, D10);
  `harness.stale_write_check` (bool, default `true`, D10; also settable per headless eval variant).
- **D16. Built-in, closed to mods and the phone; buffers in memory only.** The Files panel stays a
  built-in panel that takes no modules ([D15 of item 55](terminal-file-panes.md#decisions)); no mod
  permission reaches buffers or `files_write`. A paired phone is refused `files.write`,
  `files.stat` and `files.baseline` as it is every `files.*` method
  ([D8 of item 55](terminal-file-panes.md#decisions)). Unsaved text is never written to
  `localStorage`, IndexedDB or the app store; the list of open tabs per session (`{root, path,
  line, column}`, no content) is kept in a new `editor` document of the app store. Closing a dirty
  tab, switching the Files panel's session, detaching or re-attaching the Files panel, reloading
  the window and closing the app each ask *Save*, *Don't save* or *Cancel*, listing the dirty
  files. Rejected: hot exit ([open question 1](#open-questions)); carrying buffers across a detach
  (a new webview; asking is simpler and rare).
- **D17. Testing.** Item 55's four layers: Rust tests on the matrix for the daemon's write path,
  locks and gate; `tauri::test` for the relay's access rules; Playwright browser checks against the
  fake daemon for the editor, conflicts, preview and presentation (the IPC is replaced there, so
  access rules are not checked in that layer); and real-app scenarios in `presence-e2e`.
  - **Races are deterministic.** `imperium-core` gains a test-only feature `test-hooks` with named
    barrier points in the write path (`after_parent_open`, `after_compare`, `before_rename`) and in
    the read path (`after_open`); a test parks the save at a barrier, swaps a folder for a link or
    rewrites the file, and releases it. No race test depends on a loop winning by chance. The
    feature is off in release builds and a build check fails if it is on.
  - **WebView2.** Playwright's Chromium is close to WebView2 but not it. Stage E7 adds a Windows
    run of the end-to-end scenario through `tauri-driver` and `msedgedriver` matched to the
    runner's WebView2 version, if that driver can be installed on the hosted runner; if not, the
    stage records it as a named gap and the Windows access rules rest on the `tauri::test` layer.
  - Rejected: manual checks of any kind.

Decisions D18 to D27 are new in revision 4 (revised in 5); D28 is new in revision 5.

- **D18. Language servers run in the daemon; the webview never speaks LSP.**
  - **Where.** A new crate, `crates/imperium-lsp`, is an LSP 3.17 client: Content-Length framed
    JSON-RPC over the server's stdin and stdout, the lifecycle (`initialize` with the client's
    capabilities, `initialized`, `shutdown`, `exit`), document sync, capability checks and
    the normalisation of results. Types come from `lsp-types` 0.97.0 (MIT), pinned exactly.
    The daemon owns one `LspHost` per **(server, project folder)**: the folder is the
    environment's first folder or the session root that holds the file, sent as one
    `workspaceFolders` entry, so two sessions on one project share a server.
  - **Why in the daemon:** the daemon already confines paths, spawns processes under caps
    (item 68) and isolation, and keeps policy; agents' diagnostics tool (D24) needs the same
    servers; and a webview that spoke LSP would talk to a process that runs project code. VS
    Code's choice of a separate process for the server holds here too.
  - **What the webview sees** is Imperium's own `lang.*` protocol (see
    [protocol additions](#protocol-additions)): positions as UTF-16 offsets (CodeMirror's own
    units and LSP's mandatory encoding, so no conversion), diagnostics, hover text, completion
    items, code-action titles and edits as `{root, path, edits}` lists, all normalised and capped
    by the daemon. Raw LSP messages never reach the webview.
  - **Gate.** `lang.open`, `lang.change`, `lang.close` and every method that can start a server
    are answered only to the identified desktop app through the native relay, like
    `files.write` (D3): starting a server runs project code, and an agent's process holding the
    token must not get that around the trust gate and its own permission rules. The relay is one
    more command, `lang_request`, beside `files_write` in `file_write.rs`, and one event,
    `lang://diagnostics`, for pushed results. `lang.check` (D20) and `lang.status` are answered
    to any full client; a phone and mods get none.
  - **Document sync.** The daemon sends `didOpen` for a tab's buffer, `didChange`
    (incremental when the server supports it, else full) from the buffer's changes, debounced
    by size (D22), and `didClose` when the last tab on the file closes. **One tab owns the sync
    per URI:** when several tabs or windows show one file, the focused one (else the most
    recently focused) sends its buffer; the others show its diagnostics, and a change of owner
    re-sends the full text as a new version. Files changed on disk by agents' tools, saves and
    rewinds are sent as `workspace/didChangeWatchedFiles`, from the daemon's own record of
    writes, filtered by the globs the server registered (`client/registerCapability` for
    `workspace/didChangeWatchedFiles`, honoured only for patterns inside the project folder); a
    server that registered none gets the open files re-synced on window focus. No file-system
    watcher is added (item 55's non-goal), and servers that watch files themselves
    (rust-analyzer) keep doing so.
  - **Server-to-client requests** are answered by the daemon under a fixed policy:
    `workspace/configuration` from the catalogue's defaults and the user's settings (D19);
    `workspace/workspaceFolders` with the one folder; `client/registerCapability` for the
    capabilities the client declared; `window/workDoneProgress/create` and `$/progress` as the
    status line; `window/showMessage` and `window/logMessage` as text in the server's log, capped;
    `workspace/applyEdit` as a reviewable edit (D23), never applied without the user;
    `window/showDocument` only for a file inside the folder, opened in a background tab
    (`takeFocus` is honoured only within 2 s of a user action on that server, such as running a
    code action), and an external URI shown as a link behind item 55's confirmation, never
    opened; every other request gets
    `MethodNotFound`. A URI outside the project folder in any result is dropped and counted.
  - **Bounds.** A message over 16 MiB ends the server (logged); diagnostics are capped at 1,000
    per file and 20,000 per server, messages at 4 KiB, hover and documentation at 64 KiB,
    completion lists at 500 items; a request unanswered after 10 s is cancelled with
    `$/cancelRequest`.
  - Rejected: **`@codemirror/lsp-client` in the webview** (6.3.0, MIT) over a transport relayed
    to the server's pipes: the webview would handle server requests such as `applyEdit` and
    `showDocument`, and agents could not share the server; its UI parts are not needed when
    `@codemirror/lint` and `@codemirror/autocomplete` take normalised data. **Monaco's built-in
    workers** (above). **One server per session** (two sessions on one repository would start two
    rust-analyzers).
- **D19. Which servers, how they are found, and installs only by the user.**
  - **A built-in catalogue,** `crates/imperium-lsp/catalog/servers.json` (data, reviewed like
    code), names for each server: the languages and file patterns, the program and arguments,
    where to look for it, a version probe, the settings it is sent, whether it needs a trusted
    project, and the install commands to offer. Projects and plugins cannot add entries; the user
    can add or override one in the user layer (`editor.lang.servers.<id>`: `command`, `args`,
    `languages`, `enabled`), which is the only way to name a program.
  - **First languages and servers** (licences checked on their repositories and the npm
    registry; versions are the user's, not pinned):

    | Language | Server (licence) | Diagnostics and formatting | Found by | Offered install |
    | --- | --- | --- | --- | --- |
    | Rust | `rust-analyzer` (MIT or Apache-2.0) | compiler errors through `cargo check`; Clippy when `editor.lang.rust.check = "clippy"`; `rustfmt` through the server | `rustup which rust-analyzer`, then `PATH` | `rustup component add rust-analyzer` |
    | TypeScript, JavaScript | `typescript-language-server` (Apache-2.0) with the project's `typescript` or a user-installed one | type errors; formatting off (ESLint or Prettier is the formatter) | `PATH`; the project's `node_modules/typescript` only when trusted and approved (below) | `npm install -g typescript typescript-language-server` |
    | TypeScript, JavaScript lint | `vscode-eslint-language-server` from `vscode-langservers-extracted` (MIT) | ESLint rules and fixes; it always loads the project's `eslint` and config | `PATH` | `npm install -g vscode-langservers-extracted` |
    | Python | `ruff server` (MIT) for lint, fixes and formatting; `basedpyright-langserver` or `pyright-langserver` (MIT) for types | Ruff rules; type errors | `PATH`, the active environment's `bin` only when trusted and approved | `uv tool install ruff`, `pipx install ruff`; `npm install -g pyright` |
    | Go | `gopls` (BSD-3-Clause) | compiler and vet errors; `gofmt` | `PATH`, `$(go env GOPATH)/bin` | `go install golang.org/x/tools/gopls@latest` |
    | C, C++ | `clangd` (Apache-2.0 with LLVM exception) | compiler errors from `compile_commands.json`; clang-tidy when the project's `.clang-tidy` asks | `PATH` | the OS package (`winget install LLVM.LLVM`, `brew install llvm`, `apt install clangd`) |
    | Markdown | `marksman` (MIT) | broken links and headings | `PATH` | the release from its GitHub page (shown, not run) |
    | JSON | `vscode-json-language-server` (MIT) | parse errors and schemas; remote schema download off | `PATH` | `npm install -g vscode-langservers-extracted` |
    | TOML | `taplo lsp stdio` (MIT) | parse errors and schemas; remote schemas off | `PATH` | `cargo install taplo-cli --locked` |
    | YAML | `yaml-language-server --stdio` (MIT) | parse errors and schemas; SchemaStore off | `PATH` | `npm install -g yaml-language-server` |

  - **Settings sent to servers that would otherwise execute or fetch:** rust-analyzer gets
    `cargo.targetDir = true` (its own target folder, so its checks do not lock the agents'
    builds; stage E9 confirms the key), **`cargo.buildScripts.enable = false` and
    `procMacro.enable = false` per project until the user turns them on** in the project's
    language settings ([settled](#settled-decisions), D28), and `check.overrideCommand` and
    `cargo.buildScripts.overrideCommand` are sent only from the user layer, never from a
    project; the JSON, TOML and YAML servers get remote schema download off, with a bundled set
    of schemas (below). With build scripts and proc macros off, rust-analyzer still type-checks,
    but code generated by them is unresolved; the status bar says so and links the switch.
  - **Probes and installs read no project configuration (B1).** Package managers and toolchain
    managers read configuration from the folder they run in and its parents: `.npmrc`
    (`registry`, `script-shell`, `node-options`), `.cargo/config.toml` (`build.rustc-wrapper`,
    `[registries]`), `rust-toolchain.toml` (a `path` toolchain, or a channel rustup would install),
    `uv.toml`, `pip.conf` and `pyproject.toml`'s `[tool.uv]` index URLs, and `go.mod`'s
    `toolchain` line, which with `GOTOOLCHAIN=auto` downloads and runs another Go. So:
    - every version probe (`rustup which`, `go env GOPATH`, `<server> --version`) and every
      offered install runs with its working folder set to a **neutral folder**,
      `<data>/lsp-work` (created empty by the daemon, no parent holding project files), never a
      project folder, and with the project's folders absent from the environment
      (`CARGO_HOME`, `RUSTUP_HOME`, `npm_config_*`, `PIP_*`, `UV_*` taken from the user's
      environment only, never set from a project);
    - on Windows, the program lookup of D21 (no current-folder search, no `cmd.exe` for shims,
      no relative or in-project `PATH` entries) applies to probes and installs too;
    - the environment adds **`GOTOOLCHAIN=local`** and **`RUSTUP_AUTO_INSTALL=0`**, and removes
      `RUSTUP_TOOLCHAIN`, `GOFLAGS`, `RUSTC_WRAPPER` and `CARGO_BUILD_RUSTC_WRAPPER`;
    - installs name their source explicitly: npm with `--registry https://registry.npmjs.org/`
      and `npm_config_userconfig` pointing at the user's own `~/.npmrc` (or an empty file when
      there is none), `--ignore-scripts` where the package needs no install script (all of the
      catalogue's npm packages); `uv tool install` and `pipx install` with `--index-url
      https://pypi.org/simple` (through `--pip-args` for pipx); `cargo install --locked` from
      the neutral folder, so no project `.cargo/config.toml` applies; `go install` from the
      neutral folder with the environment above. The confirmation shows these flags as part of
      the exact command. The user's own configuration files (`~/.npmrc`, `~/.cargo/config.toml`,
      `~/.config/uv/uv.toml`) still apply: they are the user's;
    - a toolchain the project points at (a `path` in `rust-toolchain.toml`, a project-local
      `node`, Go from `go.mod`'s `toolchain`) is a **project-local program** (D20): never used in
      an untrusted project, and in a trusted one only after its own approval. Without approval
      the server runs on the user's default toolchain, and the status bar says so.
    - When a **server** starts in a trusted project its working folder is the project (it must
      be), with the same environment additions (`GOTOOLCHAIN=local`, `RUSTUP_AUTO_INSTALL=0`), so
      starting a server never downloads a toolchain.
  - **Discovery** looks only in the catalogue's places and the user's override, never in a
    project folder unless the project is trusted **and** the user approved that program (D20).
    The status bar shows each language's state: running, starting, not installed, off, untrusted,
    or stopped with the reason.
  - **Installs are never automatic** (the user's rule, [settled](#settled-decisions)):
    - **Detect first.** A missing server shows once per language and project as a notice in the
      tab: "No Rust language server found. Install `rust-analyzer`?" with *Install*, *Copy
      command*, *Not now* and *Don't ask for Rust*.
    - **One install per approval.** *Install* opens a confirmation naming the exact command, what
      it downloads and from where, and that it runs with your rights. Only *Run this install*
      runs it; there is no "always" and no batch. The command runs through `lang.install` (desktop
      gate only, [D3](#decisions)) as a `command` cap unit, its output is shown, and the server is
      found again afterwards. An offer whose command line is not exactly the catalogue's is
      refused, so the webview cannot be made to run another command. "Exact" covers the
      argument list, the neutral working folder and the environment above, so the command means
      the same thing whichever project is open.
    - **Agents.** No agent tool installs a server. An agent's `run_command` of a catalogue install
      command (and of `rustup component add`, `npm install -g`, `pipx install`, `uv tool install`,
      `go install` and `cargo install` in general) is added to the safety floor's always-ask list,
      the rule of pull requests #304 and #330: asked every time in every mode, never "always
      allow".
    - **Nothing is bundled** in this item ([settled](#settled-decisions); bundling Ruff may
      follow): shipping
      servers would add hundreds of megabytes, their own update and licence notices, and a
      second copy beside the user's toolchain (rust-analyzer must match the toolchain).
  - **Bundled schemas** for the built-in checks and the JSON, TOML and YAML servers: a small set
    from SchemaStore (Apache-2.0) for `package.json`, `tsconfig.json`, `.eslintrc.json`,
    `Cargo.toml`, `pyproject.toml`, GitHub workflow files and `.mcp.json`, plus Imperium's own
    schema for `.imperium/config.toml`, pinned by revision in `crates/imperium-lsp/schemas/`.
  - Rejected: **downloading servers on first use** (the user's rule); **a plugin or mod
    contributing servers** (a plugin would then name a program the daemon runs; a later item may
    add that behind the plugin approval of [06](../06-extensibility.md)); **the project's
    `.imperium/config.toml` naming servers** (the environment layer never runs a program).
- **D20. Project trust, and what an untrusted project gets.**
  - **What trust means.** A language server or linter in a project executes project-controlled
    code and configuration: rust-analyzer runs `build.rs` and proc macros and reads
    `rust-analyzer.toml`; ESLint loads `eslint.config.*` and its plugins from `node_modules`;
    `typescript-language-server` with the project's TypeScript runs the project's copy of
    `tsserver` and its plugins; Ruff and Pyright read `pyproject.toml`, and a project
    environment's `ruff` is a project binary; `gopls` runs `go list`, which can run toolchain
    downloads named by `go.mod`; clangd's `.clangd` and `compile_commands.json` choose flags and,
    with `--query-driver`, compilers to run. So a server runs only in a **trusted project**.
  - **One trust, the existing one.** The trusted list is the user's `mcp.trusted_projects`, which
    already decides whether a project's `.mcp.json` servers, its environment config's servers
    and plugins and its named agents load ([10](../10-configuration.md),
    `crates/imperium-config/src/environment.rs`): it means "this repository's own files may make
    Imperium run programs". Revision 4 uses the same list and calls it **trusted projects** in
    the interface and docs; the key keeps its name, so no migration. Rejected: a second list for
    the editor (two answers to one question); VS Code's "trust the parent folder" (a folder of
    clones would trust every future clone).
  - **Which files a trust covers (major 1).** A file is in a trusted project when **its
    environment's project folder** (the environment's first folder, which `mcp.trusted_projects`
    already names) is trusted; the file must also be inside one of the session's roots (D4).
    Folder comparison is by **identity** (device and inode, or volume serial and file index, of
    the opened folder), with the canonical path (`McpSettings::trusts` today) kept only as the
    stored form and as the fallback when the folder cannot be opened; stage E8 changes
    `trusts` to compare identities first, so a junction, a case variant or a moved spelling of
    the same folder matches and a different folder at an old path does not. **A worktree**
    (an auto-fix or data-mode worktree, or one the user made) inherits its main repository's
    trust when its git common folder (`git rev-parse --git-common-dir`, through `safe_git`) has
    the same identity as the trusted repository's; a worktree of an untrusted repository is
    untrusted. **Parent folders never pass trust down:** trusting `~/src` trusts no project
    under it; only a listed folder, and worktrees of it, are trusted.
  - **Who may grant it.** Only the user: their config file, Settings → *Trusted projects*, or the
    editor's banner, which call a new `projects.trust { folder }` and `projects.untrust` answered
    only to the desktop app through the native relay **and** to a caller that passes
    `peer::may_grant` ([06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)).
    Found on `experimental`: the safety floor asks about `imperium config set|unset` of
    `extensions.*` keys but not of `mcp.trusted_projects`, so an agent's command in a permissive
    mode can trust a project today. **Pull request #330 is fixing this now** (the floor for
    runtime and trust settings); stage E8 builds on it, checks that `config set|unset
    mcp.trusted_projects` from an agent's command is **refused outright** (as resource caps do
    for `limits.mode`), adds that if #330 lands with only an ask, and makes the daemon's
    `config.set` of that key refuse a caller that fails `may_grant`.
  - **Re-consent for projects trusted before revision 4 (major 2, the settled answer to
    question 9).** A project trusted for MCP servers was trusted before trust meant "language
    servers may run its build scripts". So the first time a language server would start in a
    project that is trusted but has no language-server consent record, nothing starts and the
    banner asks once: "`<folder>` is trusted for MCP servers. Language servers would also run
    its own code (build scripts, lint plugins). Allow language servers here?" with *Allow* and
    *Not now*. *Allow* writes a consent record (folder identity, time) to `lsp-approvals.json`
    through the same gated, `may_grant` path; trusting a project from the banner after revision
    4 records both at once.
  - **The banner.** Opening a file of an untrusted project shows once per project: "Language
    servers and linters are off in this project, because they run the project's own code. Trust
    `<folder>`?" with *Trust project*, *Not now* and *What runs?* (a list of the servers that
    would start and what each reads). Untrusting stops its servers at once.
  - **Project-local programs need a second approval (major 3).** Even in a trusted project, a
    program found inside the project (`node_modules/typescript`, `node_modules/.bin/eslint`,
    `.venv/bin/ruff`, a `path` toolchain in `rust-toolchain.toml`, B1) runs only after the user
    approves it, as ESLint's library approval does. The approval binds, and the dialog says
    which:
    - for a **package** (`typescript`, `eslint`): the resolved package folder
      (`node_modules/<name>` after links are resolved, judged inside the project) and a hash
      over its files, **bounded**: at most 20,000 files and 200 MiB, skipping nested
      `node_modules`; past the bound it binds the lockfile instead;
    - or the **lockfile** hash (`package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`, `uv.lock`)
      when the package is too large to hash, with the honest caveat in the dialog: "Bound to
      the lockfile. A change made inside `node_modules` without changing the lockfile is not
      detected."
    - for a **single binary** (`.venv/bin/ruff`, a toolchain's `rustc`): the file's path and
      content hash.
    A changed hash asks again. The record lives in `<data>/lsp-approvals.json`, which joins
    D7's refused daemon files and the safety floor's refused list for agents' file tools and
    commands. The default is the user-installed program, so most
    projects never ask.
  - **Untrusted projects get built-in checks only, which run no project code:**
    - **In the webview:** parse errors from the file's Lezer grammar (the error nodes of
      `syntaxTree`, shown as "Syntax error (built-in)") for every language item 55's
      `language-data` loads with a Lezer parser (Rust, JavaScript, TypeScript, Python, Go, C,
      C++, JSON, YAML, CSS, HTML, XML); stream-mode languages have none.
    - **In the daemon:** `lang.check { language, text }` (pure, answered to any full client,
      at most 4 MiB): JSON and JSON with comments through `serde_json` with positions, TOML through
      the `toml` crate already in the lock, and JSON Schema validation of the bundled schemas
      (D19) through `jsonschema` 0.61.1 (MIT) with default features off, so no `$ref` is ever
      fetched over the network or from disk. Schema choice is by file name only, never by a
      `$schema` in the file.
    - No server, linter, formatter, project binary or project configuration is used, whatever
      the catalogue says. Trusted projects get the built-in checks too, under the servers'.
  - Rejected: **running "data-only" servers (JSON, YAML, TOML, Markdown) in untrusted projects**
    as VS Code's built-in JSON support does: they are still third-party programs parsing
    attacker files, and the built-in checks cover their main value ([settled](#settled-decisions)); **trust by default with a warning** (a cloned repository would run
    its build scripts on first open).
- **D21. Language servers are an item 68 cap class.** Item 68 (resource caps,
  [plan](resource-caps.md), merged in pull request #332, no stage built yet) gives every
  process Imperium starts a cap unit. This plan adds a class to its table
  ([L6](resource-caps.md#l6-default-caps-per-class)) and to `Class`
  ([L1](resource-caps.md#l1-one-crate-owns-every-os-mechanism-imperium-limits)):

  | Class | Covers | Memory | 64 GB machine | Processes (Windows, macOS) | Linux tasks | CPU |
  | --- | --- | --- | --- | --- | --- | --- |
  | `lsp` | each language server with everything it starts (rust-analyzer's `cargo check` and `proc-macro-srv`, clangd's workers, ESLint's Node) | 25% of RAM, between 2 and 8 GiB | 8 GiB | 256 | 8,192 | none; below-normal priority |

  - **Why 8 GiB:** rust-analyzer on a large Rust workspace commonly takes several gigabytes, and
    its `cargo check` runs inside the same unit; 2 GiB would stop it on this repository. Other
    servers use far less, so the class cap is a leak guard. Stage E9 measures rust-analyzer on
    this workspace and `typescript-language-server` on `apps/desktop` in CI and adjusts before
    the class enforces; it starts in `warn` like item 68's other new classes.
  - **Per server:** `limits.lsp.memory_mb` for the class and `limits.lsp.servers."<id>".memory_mb`
    for one server, user layer only, as item 68's L7 says for MCP servers.
  - **Not in a session aggregate:** servers belong to a project, not a session (D18), so they
    count against Imperium's total but no session's aggregate.
  - **Watchdog order (major 6).** In [L11](resource-caps.md#l11-the-daemon-watchdog)'s step 1,
    `lsp` sits beside `mcp`: after `internal`, `browser`, lingering descendants and background
    commands, together with `mcp` and `hook`, and **before** `cas`, `script` and foreground
    commands. A server is restartable and losing it costs only diagnostics; a foreground command
    is the agent's work in flight. Stage E9 adds the class to L11's list in the same pull
    request.
  - **At most `editor.lang.max_servers` servers (default 4)** run at once; starting a fifth stops
    the least recently used. A server with no open file is stopped after 10 minutes.
  - **Ending (L14):** when the server exits, its descendants are stopped with the unit. A server
    stopped by its cap or crashed is restarted after 1, 5 and 30 s, at most three times in ten
    minutes; then its status says "Stopped: memory cap 8 GiB reached. Raise
    `limits.lsp.memory_mb` in Settings → Resources." and diagnostics from it are marked stale.
  - **Environment:** the server gets the user's environment with the daemon's token, endpoint
    and `IMPERIUM_*` variables removed and the spawn mark `IMPERIUM_SPAWNED_BY_DAEMON` set, so a
    server, or the project code it runs, cannot pass `may_grant` or the desktop gate.
  - **Windows program lookup (round 2).** The catalogue's npm servers are installed as `.cmd`
    shims (`typescript-language-server.cmd`), which `cmd.exe` runs, and `cmd.exe` looks for
    `node` in the **current folder** first, which for a server is the project. So, for every
    server, probe and install on Windows:
    - the environment sets **`NoDefaultCurrentDirectoryInExePath=1`**, which stops `cmd.exe`
      and the `CreateProcess` search from trying the current folder;
    - a `.cmd` or `.bat` shim is **never run through `cmd.exe`**: the daemon reads the shim
      (npm's generated form, matched exactly) for the script it names and launches
      `<absolute node.exe> <absolute script path> <args>`, with `node.exe` resolved as below; a
      shim that does not match npm's form is refused with a reason;
    - every program, `node.exe` included, is resolved to an **absolute path** by a `PATH` search
      that **drops empty and relative entries and every entry inside a project folder of any
      open session**, as item 55's [D6](terminal-file-panes.md#decisions) does for shells; the
      child's `PATH` is filtered the same way.
  - **Isolation tier ([settled](#settled-decisions), B2).** A server runs at the **strictest
    isolation tier of the project's open sessions** ([05](../05-tools-and-isolation.md#isolation-tiers))
    where that tier exists on the OS: tier 1 (the OS sandbox: Landlock and seccomp on Linux,
    Seatbelt on macOS) with the project folder and the server's own cache and target folders
    writable, the user's toolchain folders (`~/.cargo`, `~/.rustup`, the npm and Go caches)
    readable, and the network as the session's tier allows; a session at tier 2 (a container)
    runs the server at tier 1 on the host, since the server must reach the host's toolchain,
    and the status says so. Windows has no OS tier ([05](../05-tools-and-isolation.md#isolation-tiers)),
    so there servers run at tier 0 with the caps and D28's pause, and Settings says so. **With
    no open session** on the project, the server uses the tier the environment is configured
    for (its `[isolation]` setting, or the user's default), never tier 0 by default; **background
    agents** (scheduled runs, auto-fix sessions) count as open sessions. When a
    stricter session opens on the project, running servers restart at the new tier. A server
    that fails under the sandbox (rust-analyzer fetching crates with the network off) reports
    the error in its status; it is never retried at a looser tier.
- **D22. Diagnostics in the editor.**
  - **Shown with `@codemirror/lint`:** squiggles by severity (LSP 1 to 4 as error, warning, info
    and hint; hints as a faint underline), `lintGutter()` marks, the hover tooltip with the
    message, source and code, the diagnostic panel (`openLintPanel`) and `lintKeymap`; F8 and
    Shift+F8 (rebindable, *Next problem*, *Previous problem*) move between them as VS Code's
    Alt+F8 does. Diagnostics arrive by `setDiagnostics` from the daemon's `lang://diagnostics`
    event and from the built-in checks, merged and labelled by source.
  - **Problems list.** A *Problems* view in the Files panel lists the open file's problems and,
    for the project, every problem the servers have reported (pushed and, where the server
    declares `workspaceDiagnostics`, pulled), grouped by file, filterable by severity and text,
    each opening the file at its range. The status bar shows the counts; Ctrl+Shift+M
    (rebindable) opens the list.
  - **When.** Each catalogue entry has a boolean default, `lint_on_type` (true for every entry
    in the first catalogue, [settled](#settled-decisions)), and the user may override it per
    language with `editor.lang.<language>.lint_on` (`type` or `save`). On type, changes are sent after a delay of the line count
    divided by 20, kept between 150 and 800 ms (VS Code's TypeScript rule with a lower floor,
    because CodeMirror shows results without a layout pass); pull diagnostics are requested after
    the same delay, and on save, focus and tab visibility, as `vscode-languageclient` does.
  - **Staleness.** Each diagnostic set carries the document version it was computed for; a set
    for an older version than the buffer is shown, mapped through the changes since, until the
    next arrives (no flicker); a set for a version the daemon never sent is dropped.
  - **Text is data.** Messages, hover text and completion documentation come from project
    tooling and may hold anything: diagnostic messages render as plain text; hover and
    documentation Markdown renders through the D11 pipeline (no HTML, links as buttons behind
    the confirmation, no images), into React elements.
  - **Completion** with `@codemirror/autocomplete`: the server's items on its trigger characters
    and on Ctrl+Space, filtered in the webview, with `resolve` for documentation; snippet items
    are inserted as plain text with tab stops turned into CodeMirror snippet fields; an item's
    `additionalTextEdits` apply only within the open buffer. Completion is off in untrusted
    projects (it needs a server) except for words in the buffer.
- **D23. Quick fixes and formatting are reviewable edits.**
  - **Asking.** Ctrl+. (rebindable, *Quick fix*) or the lightbulb in the gutter lists the code
    actions for the cursor or diagnostic, preferred ones first and `disabled` ones greyed with
    their reason. *Fix* (Shift+Alt+. by default) applies the single preferred fix.
  - **An edit to the open buffer** is applied as one CodeMirror transaction (one undo step) and
    leaves the tab dirty; the user saves as usual, through D6's base hash and D7's
    confirmations. Nothing is written by the fix itself.
  - **An edit to other files** (a `WorkspaceEdit` from the action, or a server's
    `workspace/applyEdit` after a `command`) opens a **review dialog**: one diff per file
    (`@codemirror/merge`), each ticked by default except harness and secret files, which are
    unticked and say why; *Apply* writes the ticked files through `files.write` with their base
    hash read when the edit was shown, so a file changed since conflicts (D9's banner), and with
    D7's confirmations. Open tabs of those files get the edit in their buffer instead. At most 50
    files and 4 MiB per edit; `CreateFile`, `RenameFile` and `DeleteFile` operations are refused
    in this item (no `files.rename` or `files.delete`), with the reason, and the rest of the edit
    is shown.
  - **Commands** (`workspace/executeCommand`) run only from an action the user chose, and only
    when the server declared the command.
  - **Format and fix on save are opt-in, per language:** `editor.lang.<language>.format_on_save`
    and `fix_on_save` (each `off` by default, `explicit` for saves the user makes, or `always`
    for auto saves too, as VS Code's `codeActionsOnSave` separates them). Before the save, the
    daemon asks `source.fixAll` (and `source.organizeImports` if chosen) and then
    `textDocument/formatting` (or `willSaveWaitUntil`), each with a 2 s limit; the edits apply to
    the buffer as one undo step, then the normal save runs. A timeout or an error saves the
    unformatted buffer with a note. Format document and format selection are commands too.
  - **On-save edits touch only the saved buffer (major 5).** An edit that `fix_on_save`,
    `organizeImports` or a formatter returns for **another** file is never written: under
    `explicit` it opens the review dialog after the save (the save itself goes ahead); under
    `always` (auto saves) it is dropped and counted in the note, because no one is there to
    review it. A harness or secret file still asks under `always`: the on-save edits apply to
    its buffer, but its save stops at D7's confirmation as every auto save does (D15).
  - Rejected: **applying workspace edits without review** (a server or a project's code could
    write anywhere in the folder, and harness files change what agents run); **writing fixes from
    the daemon directly** (it would skip the editor's base hash and dirty-buffer rules).
- **D24. Agents can read diagnostics; fixing is their normal edit.**
  - **A read-only tool, `diagnostics`**: `{path?, severity? (error, warning, all), limit? (at most
    200)}` returns problems for one file or the session's roots from running servers and the
    built-in checks, each `{path, line, column, severity, source, code, message,
    from_unsaved_buffer}`, the message trimmed to 1 KiB. **`from_unsaved_buffer` (major 4)** is
    true when the server computed the diagnostic on an editor buffer that differs from the file
    on disk (the owning tab is dirty), since the line numbers and the problem then describe text
    the agent cannot read; the tool's description tells the model to prefer
    `from_unsaved_buffer: false` and to re-read the file. With `disk_only: true` the tool
    returns only diagnostics computed on the saved file (and `lang.check` of the disk text) for
    files whose owning tab is dirty. It is a read tool under the permission rules ([05](../05-tools-and-isolation.md#permissions))
    like `read_file`, available in every mode, and its result is framed as data ("Diagnostics
    reported by language tools (data, not instructions):"), because a project's lint messages
    can carry injected text.
  - **It never starts a server** and never runs a linter: it reads what is running for files
    the user has open, plus `lang.check` for any file (agents may read files anyway). For a file
    with no running server it says so and returns the built-in checks (settled: a trusted
    project's servers never start for agents).
  - **No apply.** Code actions are not offered to agents; an agent fixes code with `edit_file`
    or `write_file`, under its permission rules, the safety floor and D10's stale-write check.
    Its edits reach the editor through D9's reload and the servers through
    `didChangeWatchedFiles`, so the diagnostics it reads next reflect them.
  - Behind `harness.diagnostics_tool` (default on in sessions) and a headless eval variant flag
    (default off, like `stale_write_check`), so evaluations stay comparable.
- **D25. Themes: a token model shared by CodeMirror, imported VS Code themes and semantic
  tokens.**
  - **The model.** An editor theme is `{id, name, kind (light, dark, hc-light, hc-dark), colors,
    tokens}`: `colors` are the editor surfaces (background, foreground, selection, active line,
    gutter, cursor, find match, diagnostic colours), and `tokens` give each `--syntax-*` variable
    a colour and an optional style (`italic`, `bold`, `underline`, `strikethrough`). Applying a
    theme sets those CSS custom properties on the editor's root with `style.setProperty`, so
    D14's `HighlightStyle`, which already uses only the variables, needs no rebuild.
  - **More tokens.** D14's set grows to cover what VS Code themes and semantic tokens tell
    apart: `--syntax-constant`, `--syntax-property`, `--syntax-parameter`, `--syntax-namespace`,
    `--syntax-macro`, `--syntax-tag`, `--syntax-attribute`, `--syntax-regexp`,
    `--syntax-punctuation`, `--syntax-escape`, plus `--diag-error`, `--diag-warning`,
    `--diag-info` and `--diag-hint`. Each maps to `@lezer/highlight` tags in one table
    (`src/editor/themes/tags.ts`).
  - **From TextMate scopes.** A fixed table maps each token to the TextMate scopes that VS Code
    themes colour, most specific first, for example: keyword to `keyword`, `storage.type`,
    `storage.modifier`; string to `string`; number to `constant.numeric`; constant to
    `constant.language`, `variable.other.constant`; comment to `comment`; type to
    `entity.name.type`, `support.type`, `support.class`; function to `entity.name.function`,
    `support.function`; variable to `variable`, `variable.other.readwrite`; parameter to
    `variable.parameter`; property to `variable.other.property`; namespace to
    `entity.name.namespace`; macro to `entity.name.function.macro`; tag to `entity.name.tag`;
    attribute to `entity.other.attribute-name`; operator to `keyword.operator`; regexp to
    `string.regexp`; escape to `constant.character.escape`; heading to `markup.heading`; link to
    `markup.underline.link`; invalid to `invalid`; punctuation to `punctuation`. The importer
    resolves each token against the theme's `tokenColors` with TextMate's rule (a selector
    matches a scope when it is a dot-segment prefix; the longest match wins; later rules win
    ties); descendant selectors and exclusions (`-`) are ignored and listed in the import report.
  - **Semantic tokens.** When a server offers `semanticTokens/full` (with `delta` when it can),
    the daemon decodes the five-integer encoding against the server's legend and sends ranges
    with a token type and modifiers; the editor shows them as mark decorations above the
    grammar's highlighting (VS Code's layering). The colour comes from the theme's
    `semanticTokenColors` when the imported theme has a matching selector
    (`(*|type)(.modifier)*(:language)?`, the wildcard `*` included, so `*.deprecated` and
    `*.readonly:rust` match any type with those modifiers; the most specific match wins, as in
    VS Code), else from VS Code's fallback table (`function` as
    `entity.name.function`, `variable.readonly` as `variable.other.constant`, `parameter` as
    `variable.parameter` and the rest) through the same scope table, so one theme colours both
    layers. `editor.semantic_highlighting` (`theme`, default: on when the theme asks for it, as
    VS Code's `configuredByTheme`; `on`; `off`).
  - **Importing a VS Code theme is data only.** Settings → Editor → *Import theme* takes one
    `*-color-theme.json` file chosen with the webview's file input (no file-system access) or
    pasted text, at most 1 MiB: parsed as JSON with comments by a small tolerant parser, never
    evaluated. Only `name`, `type`, `colors` (the editor keys in the mapping table),
    `tokenColors` (an inline array; a string path to a `.tmTheme` is refused), `semanticHighlighting`
    and `semanticTokenColors` are read; `include` is not followed (it would read another file),
    and every other key is ignored. A colour must match `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa`
    (alpha flattened against the background), a style must be one of the four words, and
    anything else is dropped and reported, so no value can carry CSS. The normalised theme (not
    the original) is kept in the app store's `editor-themes` document, at most 20 themes.
    Extension packages (`.vsix`), icon themes and product icon themes are not imported: they
    carry code, fonts or SVG.
  - **Contrast check.** Every theme, shipped or imported, is checked with
    `test/tokenContrast.test.ts`'s function: each token and diagnostic colour against the
    editor background and the active line, 4.5:1 for normal themes and 7:1 for `hc-*` themes.
    An imported theme that fails shows which tokens fail and offers *Adjust for contrast* (on
    by default), which moves only the failing colours' lightness in OKLCH until they pass,
    keeping hue; *Use as designed* keeps the originals, with a warning in the theme picker.
    *Use as designed* applies to **shipped** themes too: a shipped theme adjusted at conversion
    keeps its original colours in the file, and the user can choose them.
  - **Shipped themes**, converted once with the same importer and committed with their
    licence and source revision under `src/editor/themes/`, the MIT notices added to the app's
    third-party notices: **Imperium Light and Dark** (D14's tokens, the defaults); **Imperium
    High Contrast Dark and Light** (new, 7:1, and the editor's forced-colours rules stay on
    top); **Dark Modern and Light Modern** (VS Code, MIT); **GitHub Dark and Light**
    (`primer/github-vscode-theme`, MIT); **One Dark Pro** (MIT); **Dracula** (MIT); **Solarized
    Dark and Light** (MIT); **Nord** (MIT); **Catppuccin Mocha and Latte** (MIT). Each passes the
    contrast test as shipped (adjusted at conversion where it does not, and the theme says
    "adjusted for contrast"). Monokai is not shipped (its licence is unclear).
  - **Selection** is per user, in the window's settings (D15): *Editor theme* (`follow app`,
    default, which uses Imperium Light or Dark with the app's `data-theme`, or a named theme),
    and *Preferred light*, *Preferred dark* and *Preferred high contrast* themes used when
    following the app or the OS; `prefers-contrast: more` picks the high-contrast one, as VS
    Code's `autoDetectHighContrast` does. Imperium has no settings sync; the choice stays with
    this machine's app store.
  - **File icons** in the tree stay the app's own set (`projectIcons.tsx`); icon themes are not
    in this item.
  - Rejected: **TextMate grammars in the editor** (`vscode-textmate` with Oniguruma in
    WebAssembly: a second highlighting engine beside Lezer, and the window's policy would have
    to allow `wasm-unsafe-eval`);
    **rendering imported themes as stylesheets** (a value could break out of a declaration);
    **themes from mods** (a mod could ship one as data later; not in this item).
- **D26. Settings for language tooling.** Daemon-side, user layer only (in the registry, not on
  the environment layer's allow-list, so a project cannot turn a server on or name one):
  `editor.lang.enabled` (bool, default `true`), `editor.lang.max_servers` (default 4),
  `editor.lang.servers.<id>` (D19), `editor.lang.<language>.lint_on` (the catalogue's
  `lint_on_type` boolean when unset), `format_on_save`,
  `fix_on_save` (D22, D23), `editor.lang.rust.check` (`check`, `clippy`),
  `harness.diagnostics_tool` (D24), the item 68 keys `limits.lsp.*` (D21), and per project,
  kept in `lsp-approvals.json` through the gated path rather than in a config layer,
  *Run project build code* (D28, default off). A project's
  environment config may only turn servers **off** for itself (`[editor] lang_enabled = false`,
  narrowing). App-side (Settings → Editor): *Editor theme*, the preferred themes, *Semantic
  highlighting*, *Show problems inline* (on), *Lightbulb* (on).
- **D27. Testing language tooling without real servers in most stages.** A fake LSP server,
  `crates/imperium-lsp/tests/fake_server.rs` built as a test binary, is scripted per test:
  which capabilities it declares, what it answers, which server requests it sends
  (`applyEdit`, `showDocument` outside the folder, a 20 MiB message), how much memory it
  allocates, and whether it writes a sentinel file when started (to prove it was not started).
  The browser checks' fake daemon (item 55 stage 4) gains scripted `lang.*` answers. Only stage
  E15 installs real servers, in CI (`rustup component add rust-analyzer`, `npm install -g`
  pinned versions, `pip install ruff==<pinned>`), which is CI provisioning, not an agent
  install.

Decision D28 is new in revision 5.

- **D28. Agents' changes to execution-relevant files pause the project's servers
  ([settled](#settled-decisions), the review's B2).**
  - **The problem.** Trust says the user accepts the project's code as it was. An agent in that
    project (steered by injected text, say) can change `build.rs` or an ESLint config, and the
    next server start or re-index would run the new code with the user's rights, outside the
    agent's permission rules and the safety floor: a way around them.
  - **Execution-relevant files** (`crates/imperium-lsp/src/exec_files.rs`, one list, tested):
    - Rust: `build.rs` and any file a `Cargo.toml` names in `build =`; every `.rs` file of a
      crate whose `Cargo.toml` has `proc-macro = true`; `Cargo.toml` and `Cargo.lock` (they
      choose which dependencies' build scripts and proc macros run); `.cargo/config.toml` and
      `.cargo/config`; `rust-toolchain.toml` and `rust-toolchain`; `rust-analyzer.toml`;
    - JavaScript and TypeScript: `eslint.config.*` (`js`, `mjs`, `cjs`, `ts`, `mts`, `cts`),
      `.eslintrc*` and any file an ESLint config imports from inside the project (followed one
      level); `tsconfig*.json` and `jsconfig*.json` (their `plugins` and `extends`); `package.json`
      (`eslintConfig`, `type`, dependencies); `package-lock.json`, `pnpm-lock.yaml`, `yarn.lock`,
      `.npmrc`, `.yarnrc.yml`, `.pnpmfile.cjs`; any file under the resolved folder of an approved
      project-local package (D20);
    - Python: `pyproject.toml` (its `[tool.*]` sections and build backend), `ruff.toml`,
      `.ruff.toml`, `pyrightconfig.json`, `setup.cfg`, `uv.lock`, and files under an approved
      `.venv` program's folder;
    - Go: `go.mod`, `go.work`, `go.sum`;
    - C and C++: `.clangd`, `compile_commands.json`, `compile_flags.txt`, `.clang-tidy`;
    - Imperium's own: `.imperium/` and the harness files of D7 are already guarded and listed
      too;
    - **for agents' file-tool writes only (R1d),** everything under `node_modules/**` and
      `.venv/**` (and `venv/**`, `.tox/**`, `__pypackages__/**`): code there is loaded by ESLint,
      TypeScript plugins and Python tools, and is too large to fingerprint in full, so a tool
      write there pauses first; changes there by commands are caught by the approved programs'
      package hashes (D20) and the lockfiles above.
  - **Detection.** The daemon keeps, per project with running servers, a fingerprint (path,
    size and SHA-256) of the execution-relevant files it found when the user last started or
    resumed the servers, persisted in `lsp-approvals.json` so a pause survives a restart. A
    change is detected:
    - **before it lands, for agents' file tools:** `write_file`, `edit_file`, a rewind and a hunk
      revert classify the target path; for an execution-relevant path in a trusted project with
      servers, the daemon **pauses** the project's servers (below) before writing, so no server
      or its own file watcher sees the new content while running;
    - **before forwarding anything that makes a server evaluate project code** (round-2 R1a):
      `didOpen`, `didChange`, `didSave`, `didChangeWatchedFiles`, diagnostic pulls, code
      actions, formatting, completion and hover requests, and any start, restart or re-index.
      ESLint 9 re-imports a changed flat config on its next lint, and clangd re-reads `.clangd`
      and `compile_commands.json` on its next parse, so any of these can run changed code. The
      check is a **stat sweep** (size and modification time of each listed file, and whether one
      appeared or went) that hashes only the files that moved; its result is cached for one
      debounce window (D22, at most 800 ms), so typing costs one sweep per window, not one per
      keystroke. A difference not explained by the user's own saves in the editor (the
      `files.write` record of D10) pauses the servers before the message is sent;
    - **every second while any agent command or background process started in the project is
      alive** (R1b), by the same sweep, and once more when it ends;
    - **during each foreground agent command** in the project (R1c), the project's servers are
      **frozen** for the command's length: on Linux by `cgroup.freeze` on the server's unit
      (item 68's cgroup), on macOS by `SIGSTOP` to the unit's processes and `SIGCONT` after, on
      Windows by suspending every process in the server's job (`NtSuspendProcess` on each id
      from `JobObjectBasicProcessIdList`, in `imperium-limits`' Windows module) and resuming
      them after. A frozen server runs nothing, so it cannot react to a change mid-command. When
      the command ends the daemon sweeps: if nothing moved it thaws them, else it pauses them
      (stopping the frozen units). Freeze and thaw are `Unit::freeze` and `Unit::thaw`, added to
      item 68's `imperium-limits` by stage E10c. Editor requests during a freeze wait, within
      D18's 10 s limit, rather than fail.
  - **Pausing** stops the project's servers (`shutdown`, then the unit) and starts none until
    the user resumes; diagnostics stay, marked stale.
  - **Watching goes through the client where the server allows it** (R1e), so the daemon's
    notifications, checked first, are the trigger: rust-analyzer with `files.watcher =
    "client"` and `cargo.buildScripts.rebuildOnSave = false`; ESLint's server, which relies on
    the client's watchers; `tsserver` with client watch events (`canUseWatchEvents`, TypeScript
    5.4 and later) if `typescript-language-server` passes them through, which stage E9
    verifies (if it does not, `tsserver` keeps its own watchers, and the freeze and the sweeps
    above are what cover it). `--allowLocalPluginLoads` is **never** passed to `tsserver`, so it
    never loads a plugin from the project folder by path.
  - **The banner.** "An agent changed files that decide what runs in `<project>`. Language
    servers are paused." It names each file, who changed it when known (the agent and session
    from the tool record; "changed outside the editor" otherwise), and shows the diff of each
    against the fingerprinted version (kept by the daemon for files up to 1 MiB) with
    `@codemirror/merge`. A file with no stored version (over 1 MiB, newly appeared, or found by
    the stat sweep in a folder whose contents are not stored, such as an approved package) is
    listed with its old and new size and SHA-256, and *Open* shows its current text. *Resume* (gated, `may_grant`) takes the
    new fingerprint and restarts the servers; *Keep paused* leaves them stopped; *Review in
    Review panel* opens the session's changes. Resuming never happens by itself, by timer or by
    an agent; an agent's `diagnostics` call during a pause returns the built-in checks and says
    the servers are paused.
  - **Build code is off until the user turns it on.** Per project, *Run project build code*
    (default off) controls rust-analyzer's `cargo.buildScripts.enable` and `procMacro.enable`
    and also **`checkOnSave`**, because `cargo check` and Clippy compile and run build scripts
    and proc macros themselves; with it off, Rust diagnostics come from rust-analyzer's own
    analysis only, and the status bar says what is missing. Turning it on is gated and
    `may_grant`, and recorded in `lsp-approvals.json`. Other servers' equivalents (ESLint's
    plugins, TypeScript plugins) are covered by trust, the program approval and the pause.
  - **Isolation** of servers is in D21.
  - Rejected: **re-asking for trust after every agent change** (the same question in a heavier
    form); **ignoring agents' changes** (B2's bypass); **reverting the agent's change** (it is
    the agent's work, reviewed in the Review panel as usual).

## Current state (found on `experimental` at `243602ab`)

- **Item 55 is planned, not started.** There is no `files.*` method, no `crates/imperium-core/src/files.rs`,
  no `cap-std`, no CodeMirror in `apps/desktop/package.json`, no `apps/desktop/e2e/` harness, no
  `app_gate.rs` and no native relay. This plan builds on its stages 2b (the gate), 3 (the relay
  pattern), 4 (browser checks), 6a and 6b (roots, reads, listing, locate) and 7a and 7b (viewer and
  tree).
- **Markdown** is already rendered in the transcript with `react-markdown` and `remark-gfm`
  (`src/panels/SessionPanel.tsx`), default options, versions with `^` ranges.
- **Opening links:** `src/shared/openExternal.ts` calls the native `open_external` (http and https
  only).
- **Panels and layout:** built-in panels are `rightPanels` entries and the `PANELS` list in
  `App.tsx`; `src/layout/` arranges, stacks, folds and detaches them (`panel-<id>` windows); the
  `layout` document is in the app store (`src/layout/state.ts`, `src/shared/store.ts`). Styles are
  split under `src/styles/` (`tokens.css`, `panels.css`, `forced-colors.css`); settings and
  shortcuts are `src/settings/settings.ts` and `src/settings/shortcuts.ts`. The user's working tree
  on `feat/imperium-nursery` has a different layout (`src/settings.ts`, `src/styles.css`); this plan
  follows `experimental`.
- **Theme:** `data-theme` on the root, light tokens at `:root[data-theme="light"]`, accents per
  theme, contrast tested by `test/tokenContrast.test.ts`.
- **Agents' file tools:** `write_file` and `edit_file` (`crates/imperium-tools/src/files.rs`) write
  with `tokio::fs::write` (truncate and write), with no lock, no hash and no record of reads.
- **The safety floor's path check** is `imperium_policy::permissions::safety_floor` (`.git`
  anywhere in the path, case folded on Windows and macOS; protected folders; `FLOOR_PATHS`), called
  today with the path an agent asked for.
- **Writers of files besides the tools:** `session.rewind` (checkpoints) and `session.review_hunk`
  (reverting a hunk) rewrite files with no lock.
- **Crates already in the lock:** `sha2`, `same-file`, `encoding_rs`, `similar`, `tempfile`; no
  `notify`. Tauri is 2.x (CLI 2.12.4).
- **Protocol** is 0.62; error codes run to `-32009` (`DAEMON_STOPPING`).

Found for revision 4 on `experimental` at `a2b26dc6`:

- **Protocol** is 0.64 (pull request #329); the stages still take the next minor at merge.
- **Item 55** has begun: stage 1 (the PTY library, #307) is merged; the file stages are not.
- **No language tooling:** no LSP client, no `lsp-types`, no `@codemirror/lint` or
  `@codemirror/autocomplete`, no `jsonschema`; `toml` and `serde_json` are in the lock.
- **Project trust** is `mcp.trusted_projects` (`crates/imperium-config/src/settings.rs`,
  `McpSettings::trusts`, compared by canonical path): it gates a project's `.mcp.json`
  servers, its environment config's servers, plugin folders and suggestions
  (`environment.rs`), and its named agents. A repository cannot trust itself (the environment
  layer drops the key).
- **The safety floor** (`crates/imperium-policy/src/floor.rs`, `changes_plugin_setting`) asks
  about `config set|unset` of `extensions.disabled_plugins` and `extensions.plugin_dirs` but
  not `mcp.trusted_projects`, so an agent's command outside Ask mode can add a trusted project
  today. Stage E8 closes this.
- **Resource caps** (item 68) are planned in [resource-caps.md](resource-caps.md), merged in
  pull request #332 (revision 5 found it merged at `f17bdca5`; no stage built); its `Class` has
  no language-server class yet.
- **Agent installs:** pull request #304 (open) makes `install_extension` ask every time; pull
  request #330 (open) floors runtime installs. Neither covers language-server install commands.

## Scope and non-goals

In scope: editing, saving and creating files in the Files panel; conflict handling with agents
and other programs; the per-file lock across every writer in the daemon; the agent-side stale-read
refusal and next-turn note; the Markdown preview; presentation mode; settings, shortcuts, themes;
the daemon's write path and gate; the native relay; browser checks; the end-to-end scenarios; the
docs (02, 03, 05, 06, 07, 10, 12, 15).

Not in scope:

- Agents driving the editor in any way; any agent tool for it.
- Rename, move, delete and folder creation (the tree's *New file* is the only structural action);
  "Save as" outside the file's folder.
- In language tooling (revision 4 brings diagnostics, hover, completion, quick fixes and
  formatting into scope): go to definition, find references, rename, refactorings beyond
  code actions, call hierarchies, inlay hints and debugging; servers for languages outside the
  D19 table; bundled servers; servers contributed by plugins or mods; icon themes; TextMate
  grammars; remote schema downloads.
- Remote runtimes (container, WSL, SSH), a remote daemon, the phone, the CLI.
- Persisting unsaved buffers; a file-system watcher; collaborative editing.
- Speaker notes, presenter view and exporting slides; reopening with another encoding.

## Threat model

Assets: the files of the session's folders and anything the user can write; repository internals
(hooks, config) and the daemon's own folders; secrets in files; the user's unsaved work.

Untrusted parties and inputs: models and their tool calls; agents' processes that can read the
daemon token at tier 0; mods and their frames; Preview pages; repository content (file names,
links, hard links, Markdown with HTML, links and images, files agents wrote); a paired phone; the
user's own registry hive (writable by any same-user process).

1. **A write that bypasses agents' permissions.** An agent's command calls `files.write` with the
   token: refused by the gate (D3), tested with a real child process carrying the spawn mark, and
   over loopback TCP.
2. **A mod frame or Preview page invoking the write.** Refused by Tauri's origin-based access
   control (D3), tested by `tauri::test` and by real frames; refused while a main-context mod is
   or was loaded; a Tauri downgrade below the CVE-2024-35222 fix fails a test.
3. **A webview made debuggable or swapped.** WebView2 policy keys in `HKCU` and the refused
   `WEBVIEW2_*` variables refuse writes and terminals (D3); a debug app is refused by a release
   daemon.
4. **Writes outside the folders.** Spellings, links, junctions, a root swapped for a link, a parent
   swapped during a save (handle-relative rename), special files, other sessions' worktrees (D4,
   D5).
5. **Writes into guarded places by another name** (D7): an inside link to `.git/hooks`, a hard
   link to a hook or to a file outside, `.GIT`, `GIT~1`, `.git.`, a bind mount of `<data>`; all
   judged on the final path and directory identities, with the parts of `<data>` that are session
   folders (`workspaces.root`, the worktree folders) allowed by identity.
6. **Hostile Markdown** (D11): raw HTML shown as text, an element allowlist, no anchors at all,
   scheme allowlist with confirmation, no remote images, SVG not rendered, local images by magic
   bytes only, inside the file's root, capped and lazy, prefixed ids, parsing in a bounded worker.
7. **Lost work** (D6, D9, D10, D16): stale saves refused by hash; one consistent read for editable
   files; dirty buffers never overwritten; one lock across every writer in the daemon; stale agent
   overwrites refused; leaving with unsaved edits asks.
8. **Lost metadata** (D5): permissions, ACLs, extended attributes, SELinux labels, quarantine and
   `Zone.Identifier` carried to the new file, or the save written in place.
9. **Secrets at rest** (D16): buffers in memory only; tab lists hold paths, not content; secret
   files need a confirmation; the next-turn note carries names only.
10. **Resource exhaustion:** saves at most 4 MiB encoded; `files.stat` at most 50 files per call;
    the poll is one visible tab every 2 s; the preview's worker is bounded in time and size, and
    images are capped in count and bytes; a busy file fails after 5 s rather than queueing.

Revision 4 adds language servers, which run project code, and themes from outside. New
untrusted inputs: a project's build scripts, proc macros, lint configs and plugins, local
copies of tools and server config files; servers' messages and requests; imported theme files.

<!-- markdownlint-disable MD029 -->
11. **A cloned project runs code on open.** No server starts in an untrusted project (D20);
    trust is granted only by the user through the desktop gate and `may_grant`, or their own
    config; an agent's `config set mcp.trusted_projects` is refused (stage E8); trust is never
    inherited by subfolders; a project-local program needs its own approval bound to its hash.
12. **A token holder starts a server.** `lang.open` and every starting method are answered only
    to the desktop app through the native relay (D18); agents' `diagnostics` tool never starts
    one (D24).
13. **A server or project code acts through the client.** Server requests are answered by a
    fixed policy: `applyEdit` only becomes a reviewed edit, `showDocument` only opens files in
    the folder, URIs outside the folder are dropped, unknown requests get `MethodNotFound`
    (D18); servers run with the spawn mark and without the daemon's token or endpoint (D21).
14. **Fixes that bypass the save rules.** Every fix or format is a buffer change the user saves,
    or a reviewed diff written through `files.write` with base hashes and D7's confirmations;
    resource operations are refused (D23).
15. **Injected text from tooling.** Diagnostic messages are plain text; hover and documentation
    go through D11's pipeline; the agents' tool frames results as data and trims them (D22,
    D24).
16. **Installs without the user.** Nothing downloads by itself; an offer runs only its exact
    catalogue command after a per-install confirmation, through the desktop gate; agents'
    install commands are on the floor's always-ask list (D19).
17. **A hostile theme.** One file, at most 1 MiB, parsed as data; only validated colours and
    four style words are applied, through `style.setProperty`; no `include`, no `.tmTheme` path,
    no packages, icons or fonts (D25).
18. **Servers exhausting the machine.** `lsp` cap units, a server limit, idle stops, bounded
    messages and result caps, restart backoff (D18, D21).
19. **Project configuration read by probes and installs (B1).** Probes and installs run in a
    neutral folder with `GOTOOLCHAIN=local`, `RUSTUP_AUTO_INSTALL=0`, explicit registries and
    the user's own npm config; a project's toolchain is a project-local program (D19, D20).
20. **An agent changing what servers run (B2).** Execution-relevant files changed by an agent
    pause the project's servers until the user resumes after seeing the diff; build code is off
    per project until the user turns it on; servers run at the project sessions' strictest OS
    tier; every forwarded message is preceded by a sweep, background commands are polled each
    second, and servers are frozen during foreground commands (D21, D28).
21. **A planted program found first (Windows).** `NoDefaultCurrentDirectoryInExePath=1`, shims
    run through an absolute `node.exe`, never `cmd.exe`, and `PATH` filtered of relative and
    in-project entries (D21).

<!-- markdownlint-enable MD029 -->

### Residual risks

- **Commands racing a save.** A program outside the daemon (an agent's `run_command`, a build, the
  user's other editor) that writes between the hash check and the rename loses its write to the
  user's save, or the reverse. The window is the time to hash and rename; the lock covers every
  writer inside the daemon.
- **An evading same-user process** can drive the app's window or detach from the daemon to pass
  `may_grant` (item 55's residual risk, unchanged); it can also set `HKLM` WebView2 policy only
  with an administrator's rights.
- **In-place fallback** (D5): when the owner or group differs, metadata cannot be copied, the file
  is encrypted, compressed, sparse or has an integrity stream, or the volume cannot rename by
  handle (FAT, exFAT, SMB), a crash during the write can leave the new bytes followed by old ones;
  the save's note says it was written in place.
- **An ancestor moved into a guarded place** (D4): someone who can already rename directories
  inside the session's folders can move an ancestor of the held parent into a guarded place
  between the last judgement and the rename. They could write there directly.
- **Agents' last-read records are lost on a daemon restart** (D10): the first full overwrite after
  a restart is not checked against an earlier read.
- **macOS has no `RESOLVE_BENEATH`;** the component walk and the held parent handle cover it, as in
  item 55.
- **A trusted project's own code runs.** Once the user trusts a project (and, for Rust, turns
  on its build code), its build scripts, proc macros and lint plugins run whenever its servers
  start, as they would in VS Code or a build: at the project sessions' strictest OS tier, with
  the user's rights on Windows, which has no OS tier (D21). Caps bound their memory, not what
  they do.
- **The sweep's own window (D28).** File tools pause before writing, foreground commands run
  with the servers frozen, background processes are swept every second, and every message that
  makes a server evaluate project code is preceded by a sweep. What remains is the few
  milliseconds between a sweep's `stat` and the server's own read of the file. A change that
  keeps both size and modification time (a deliberate `touch -r`) is not hashed by the sweep
  until the next full fingerprint at resume or restart. On Windows, if stage E10c cannot
  suspend a job's processes on the runner, changes during a foreground command are caught only
  by the 1 s poll and the sweeps, and the stage records that as a named gap.
- **User-installed servers are the user's.** Imperium runs the program the user installed or
  named; a compromised global install is outside this plan.

## Protocol additions

In `crates/imperium-protocol/src/methods/files.rs` (created by item 55 stage 6a). Each stage that
adds methods or fields takes **the next protocol minor at its merge** (0.62 today; other pull
requests may take the next ones first) and updates `PROTOCOL_VERSION`,
[02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method, field or error | Stage | Does |
| --- | --- | --- |
| `FileContent` gains `hash?`, `file_id?`, `bom?`, `eol?`, `writable`, `write_refusal?`, `needs?` | E1a | for editable text (at most 4 MiB encoded), the whole file in one response and the SHA-256 of exactly those bytes with the handle's identity; the BOM and line endings found; whether `files.write` would accept it, the reason when not, and the confirmation it would need (`secret`, `harness`) |
| paged `files.read` gains `file_id`, `size`, `modified_ms` on every page | E1a | so the read-only viewer can detect a file that changed between pages and re-read |
| `files.stat` | E1a | `{session_id, files: [{root, path}], with_hash?}` (at most 50) → `{files: [{root, path, exists, size?, modified_ms?, file_id?, hash?}]}` |
| `files.baseline` | E1a | `{session_id, root, path}` → `{source (checkpoint, head, none), text?, agent_changed_this_session}`: the file at the last checkpoint or `HEAD`, for the harness-file dialog |
| `files.write` | E1b | `{session_id, root, path, base: {hash, file_id} \| null, create?, text, encoding (utf8, utf16le, utf16be), bom, confirmed?: [secret, harness]}` → `{hash, file_id, size, modified_ms, note?}`; desktop app only (D3) |
| `FILE_CONFLICT` (`-32010`, or the next free code) | E1b | `files.write`'s base no longer matches; `data` is the current `{hash, file_id, size, modified_ms}` or `{deleted: true}` |
| `FILE_BUSY` (the next free code after it) | E1b | the file's lock was not free within 5 s |

`files.write` is answered only to the identified release desktop app (D3), otherwise `-32005`
with the reason; a guarded file without its confirmation gets `-32005` with `data: {needs}`.
`files.stat` and `files.baseline` are answered to any full client, like the other reads, and
refused to a phone. None is offered to agents. Native (Tauri) command: `files_write`, the same
arguments, relayed (stage E2). Config: `files.write_enabled`, `harness.user_edit_notes`,
`harness.stale_write_check` (D15).

Not added: a `files.changed` notification (the editor polls, D9), `files.delete`, `files.rename`.

Language tooling (revision 4), in a new `crates/imperium-protocol/src/methods/lang.rs` and
`projects.rs`; "gated" means answered only to the desktop app through the native relay (D3):

| Method, field or event | Stage | Does |
| --- | --- | --- |
| `projects.trust`, `projects.untrust` | E8 | `{folder}` → `{trusted}`; gated and `may_grant` (D20) |
| `FileContent` gains `project_trusted` | E8 | whether the file's project is trusted, for the banner |
| `lang.check` | E10a | `{language, path?, text}` → `{diagnostics}`: built-in JSON, TOML and schema checks, no project code; any full client, not the phone |
| `lang.status` | E9 | `{session_id}` → per language `{server, state (running, starting, missing, off, untrusted, stopped), reason?, version?, offer?}`; any full client |
| `lang.open`, `lang.change`, `lang.close` | E9 | sync a tab's buffer `{session_id, root, path, version, text \| changes}`; start the server if allowed; gated |
| `lang.diagnostics` event | E9 | `{root, path, version, source, items}`, pushed to the relay and re-emitted as `lang://diagnostics` |
| `lang.problems` | E11 | `{session_id, severity?}` → the project's problems by file; gated |
| `lang.hover`, `lang.complete`, `lang.resolve` | E11 | at a UTF-16 offset; normalised, capped results; gated |
| `lang.actions`, `lang.run_action` | E12 | list code actions at a range; run one → `{buffer_edits, review?: [{root, path, base, edits}]}`; gated |
| `lang.format` | E12 | `{…, range?}` → edits for the buffer; gated |
| `lang.install` | E10b | `{offer_id}` → runs the catalogue's exact command as a `command` unit, streams output; gated, after the dialog |
| `lang.approve_program`, `lang.revoke_program` | E10b | approve a project-local program by its binding (package folder hash, lockfile hash, or binary hash, D20); gated and `may_grant` |
| `lang.consent` | E8 | the one-time language-server consent for a project trusted before revision 4 (D20); gated and `may_grant` |
| `lang.resume`, `lang.paused` event | E10c | the pause banner's data (files, authors, diffs) and *Resume* (D28); resume gated and `may_grant` |
| `lang.set_build_code` | E10c | per project *Run project build code* (D28); gated and `may_grant` |
| `lang.semantic_tokens` | E14 | `{…, version}` → decoded ranges with type and modifiers; gated |

Agents' tool: `diagnostics` (D24, stage E13). Native (Tauri) command: `lang_request`, one
command for the gated methods with a method allowlist; event `lang://diagnostics`. Config: D26.

## Assumptions

- Item 55 stages 2b, 3, 4, 6a, 6b, 7a and 7b land first, with the interfaces their plan names
  (`AppGate`, the relay pattern, the fake daemon's scriptable `files.*`, `openAt`, the tree). If
  one changes, the dependent stage here follows it.
- `cap-std`'s `Dir::rename` and `open_with` (`create_new`, `write`) refuse escapes the same way its
  `open` does, on all three OSes; stage E1b tests that rather than trusting it.
- `FileRenameInfoEx` with `RootDirectory` and POSIX semantics is available on the Windows versions
  the app supports (Windows 10 1709 and later have `FileRenameInfoEx`); stage E1b confirms on the
  runner, and on an older build the save falls back to in-place with a note.
- The Windows hosted runner can enable short names on a test volume (`fsutil 8dot3name set`) or
  already has them on its system drive; if neither, the `GIT~1` test is a named known gap and the
  final-path check is still covered by the `.git.` and case tests.
- `@codemirror/merge` 6.12.3 exports `diff` usable without a merge view; stage E4a confirms.
- Tauri 2's `setFullscreen` works from a panel window as well as `main`; stage E6 confirms.

## Relevant paths

`crates/imperium-core/src/files.rs` and `src/files/write.rs` (new), `src/app_gate.rs` (item 55's),
`src/rpc/files.rs`, `src/state/` (the rewind path), `src/review.rs` (the hunk revert),
`crates/imperium-core/tests/files_write.rs` (new) and `tests/phone.rs`,
`crates/imperium-tools/src/file_locks.rs` (new) and `src/files.rs`, `crates/imperium-engine/src/agent/`
(the next-turn note), `crates/imperium-policy/src/permissions.rs`,
`crates/imperium-ipc/src/windows/replace.rs` and `src/macos/copy_metadata.rs` (new),
`crates/imperium-protocol/src/methods/files.rs` and `src/jsonrpc.rs`,
`crates/imperium-config/src/settings.rs` and `registry.rs`, `apps/desktop/src-tauri/src/file_write.rs`
(new), `main.rs`, `build.rs`, `capabilities/default.json`, `apps/desktop/src/editor/` (new:
`buffer.ts`, `tabs.ts`, `conflicts.ts`, `merge3.ts`, `encoding.ts`, `preview/`, `slides.ts`,
`present/`), `src/panels/FilesPanel.tsx`, `src/files/`, `src/shared/links.ts`, `App.tsx`,
`src/settings/settings.ts`, `shortcuts.ts` and a new `EditorSection.tsx`, `src/styles/tokens.css`,
`panels.css`, `forced-colors.css` and a new `editor.css`, `apps/desktop/test/`,
`apps/desktop/e2e/`, `scripts/ci/presence-e2e/`, `scripts/ci/detect.sh`.

Revision 4: `crates/imperium-lsp/` (new: `src/`, `catalog/servers.json`, `schemas/`,
`tests/fake_server.rs`), `crates/imperium-core/src/lang/` and `rpc/lang.rs`, `rpc/projects.rs`
(new), `crates/imperium-limits/` (item 68, the `lsp` class), `crates/imperium-policy/src/floor.rs`,
`crates/imperium-config/src/settings.rs`, `registry.rs` and `environment.rs`,
`crates/imperium-tools/src/diagnostics.rs` (new), `crates/imperium-protocol/src/methods/lang.rs`
and `projects.rs` (new), `apps/desktop/src-tauri/src/file_write.rs` (the `lang_request`
command), `apps/desktop/src/editor/lang/` (new: `lint.ts`, `complete.ts`, `hover.ts`,
`actions.ts`, `problems.ts`, `builtinChecks.ts`), `src/editor/themes/` (new: `model.ts`,
`importer.ts`, `tags.ts`, `scopes.ts`, `contrast.ts` and the shipped themes),
`src/settings/EditorSection.tsx`, `src/styles/tokens.css`.

## Stages

Each stage is one pull request against `experimental`, with the docs it changes. Sizes are
estimates of changed lines, tests included; a stage that grows past about 800 lines splits.

### Stage E1a: Consistent reads, stat, baseline and the shared file lock

- Objective: `FileContent`'s new fields and the whole-file read for editable text (D6, D8);
  page identity on paged reads; `files.stat` and `files.baseline` (protocol minor at merge);
  the judged open of D4 (parent first, one component at a time; the target relative to it without
  following its last component; the device check; the identity chain built upward with
  `openat(fd, "..")` on Unix and by handle on Windows), used by `files.read`'s editable read,
  `files.stat`, `files.baseline` and later `files.write`;
  `imperium-tools/src/file_locks.rs` with final-path keys (nearest existing ancestor for a missing
  file, D6), taken by `write_file` and `edit_file` after dispatch with a re-open under the lock,
  and by `session.rewind` (only once the turn and subagents are idle or stopped, never waiting on a
  tool while holding a lock) and `session.review_hunk`'s revert, in sorted key order; the
  guarded-target judgement of D7 as a function over the judged handle, calling the safety floor on
  the final path, with the identities of `<data>`, `<config>`, `workspaces.root` and the worktree
  folders recorded at daemon start; the `test-hooks` feature and its barriers.
- Depends on: item 55 stage 6a.
- Owned paths: `crates/imperium-core/src/files.rs`, `rpc/files.rs`, `src/state/` (rewind),
  `src/review.rs` (hunk revert), `Cargo.toml` (`test-hooks`), `tests/files.rs`, `tests/phone.rs`,
  `crates/imperium-tools/src/file_locks.rs` (new), `src/files.rs`, `src/lib.rs`,
  `crates/imperium-policy/src/permissions.rs` (only if the floor needs a resolved-path entry
  point), `crates/imperium-protocol/src/methods/files.rs`, snapshots and generated types,
  `docs/02-architecture.md`.
- Acceptance checks: `cargo test -p imperium-core --test files --test phone` and
  `cargo test -p imperium-tools` on the matrix:
  - the editable read is one response and its `hash` equals `sha256` of **exactly the bytes it
    returned** (the test encodes the returned text back with the reported encoding and BOM and
    hashes that); **torn read:** with the read parked at `after_open`, the test rewrites the file
    (in place, so the open handle sees the change) and releases it, and the assertion
    `hash == sha256(returned bytes)` still holds, whatever mix of versions the bytes are; stage E1b
    shows that a save based on such a read conflicts; **paged read:** a 6 MiB file rewritten
    between the first and second page reports a different `file_id`, `size` or `modified_ms` on
    the second page;
  - `eol` is `lf`, `crlf`, `mixed` (with the majority) and `none` for the four fixtures, and a lone
    CR is `mixed`; UTF-8 with BOM, UTF-16 LE and BE with BOM report their encoding and `bom`;
    invalid UTF-8 and BOM-less UTF-16 report `writable: false` with the reason;
  - `.env` reports `needs: secret`; `.imperium/config.toml`, `.claude/settings.json`,
    `.mcp.json` and `.claude-plugin/plugin.json` report `needs: harness`; `.git/config` reports
    `writable: false`; a 5 MiB file reports `writable: false`; a file with a second hard link
    reports `writable: false` with the package-store reason;
  - **the daemon's folders (N1):** with `<data>` and `workspaces.root` in a temporary folder as the
    daemon's defaults lay them out, a file in a session's scratch folder, a file in the Home
    environment's workspace and a file in a worktree under `<data>/worktrees` report
    `writable: true`; `daemon.json` reached through a bind mount of `<data>` placed under a root
    (Linux) reports `writable: false` (refused by identity), and so do `ipc/`, `secrets.enc`,
    `devices.json`, `remotes/x.json`, `mods/x`, `plugins/x` and `config.toml` reached the same way;
  - **judged open:** a bind mount (Linux) or junction to another volume (Windows) as the last
    component is refused by the device check; an inside link as the last component is judged at
    its target; with the open parked at `after_parent_open`, an ancestor renamed away still gives
    the identity chain of the directories actually opened;
  - `files.stat` of 51 files is refused, of 50 answers, of a removed file says `exists: false`;
    `files.baseline` returns the checkpoint text, `HEAD` when there is no checkpoint, `none` for a
    new file, and `agent_changed_this_session` after a scripted `edit_file`; `files.baseline` of
    `../outside.txt`, an absolute path and a link to outside is refused like `files.read`;
  - **lock keys:** `src/a.txt`, `SRC/A.TXT` (on a case-insensitive volume) and an inside link to it
    get the same key; the key survives an atomic replace; for a missing file in a missing folder,
    `new/dir/x.txt` and `NEW/dir/x.txt` (case-insensitive volume) get the same key, built from the
    nearest existing ancestor;
  - **lock holders:** two `edit_file` calls appending to one file from two tasks both land; an
    `edit_file` waiting on approval holds no lock (a parallel `files.stat` and a rewind proceed);
    an `edit_file` whose target is swapped for another inside file between computing the key and
    taking the lock (a barrier) re-opens, sees the new final path and retries with the right key;
    a `session.rewind` of files `b` and `a` and a hunk revert of `a` and `b` started together
    finish without deadlock (taken in sorted order, checked with barriers); a rewind requested
    while a tool holds one of its keys stops the turn, takes no lock until the tool has finished,
    and completes; the lock table is empty after all finish;
  - the phone is refused `files.stat` and `files.baseline`; the floor's existing tests pass.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E1b: `files.write`, the guarded targets and atomic replace

- Objective: `files.write` (protocol minor at merge) under D3 to D7: the gate (item 55's
  `app_gate.rs`, with `files.write` added as a caller and the release-build rule), the spelling
  checks on every component, the held parent, `nlink == 1` regular files only, `create` with
  `O_EXCL`, compare under the lock with the 5-second `FILE_BUSY`, the guarded-target judgement and
  confirmations with the daemon's allowed folders (N1), the re-judgement before the rename, the
  handle-relative atomic replace with metadata and group copy, `F_FULLFSYNC` on macOS and the
  parent flush, the Windows rename order (`FileRenameInfoEx` with POSIX semantics, then
  `FileRenameInfo` with `RootDirectory`, then in place), the Windows retries, the in-place write
  through the judged handle (`ReOpenFile`, `/proc/self/fd`, `openat` with `O_NOFOLLOW` and a file-id
  check on macOS; write then truncate) and its triggers (owner, group, encrypted, compressed,
  sparse, integrity stream, FAT, exFAT, SMB), leftover cleanup, encoding by `encoding` and `bom`,
  the 4 MiB caps, `FILE_CONFLICT`, `files.write_enabled`. The unsafe Windows and macOS pieces live
  in `imperium-ipc`'s opt-in modules.
- Depends on: stage E1a; item 55 stage 2b (`app_gate.rs`).
- Owned paths: `crates/imperium-core/src/files/write.rs` (new), `rpc/files.rs`, `rpc.rs`
  (dispatch), `tests/files_write.rs` (new), `tests/phone.rs`,
  `crates/imperium-ipc/src/windows/replace.rs`, `src/macos/copy_metadata.rs` (new),
  `src/windows.rs`, `src/macos.rs` and `Cargo.toml` (modules and features),
  `crates/imperium-protocol/src/methods/files.rs`, `src/jsonrpc.rs` (the error codes), snapshots
  and generated types, `crates/imperium-config/src/settings.rs` and `registry.rs`,
  `docs/02-architecture.md`, `docs/10-configuration.md`. If this grows past 800 lines, the
  metadata copy and `imperium-ipc` modules split into a stage E1c.
- Acceptance checks: `cargo test -p imperium-core --test files_write --test phone` and
  `cargo test -p imperium-ipc` on the matrix, with the gate stubbed in-process and real in one test,
  and every race driven by `test-hooks` barriers:
  - a save with the right base replaces the file; the bytes equal the text in the requested
    encoding, BOM and line endings, for each of UTF-8, UTF-8 with BOM, UTF-16 LE and BE; a text
    of 4 MiB plus one byte, encoded or as received, is refused before anything is written;
  - a save with a stale `hash`, or a stale `file_id` (the file replaced by another with the same
    content), returns `FILE_CONFLICT` with the current state and leaves the file unchanged; a save of
    a deleted file returns `deleted`; a save waiting more than 5 s on a held lock returns
    `FILE_BUSY`; a save whose base came from the torn read of stage E1a (a mixed version) returns
    `FILE_CONFLICT`, because no version on disk has that hash;
  - **escapes**, each refused with nothing created or changed outside: `../`, absolute, drive,
    UNC, `\\?\`, a stream name, `NUL.txt` and the other device names, a symbolic link (or junction
    on Windows) to outside as the target and as a parent, a `create` through a link;
  - **parent swap, deterministic:** with the save parked at `after_parent_open`, the test renames
    the parent away and puts a link to outside in its place, then releases; the new file lands in
    the original (moved) directory and nothing appears outside; the same at `before_rename`;
  - **guarded targets** (B1), each refused or asking as D7 says, with nothing written: an inside
    symbolic link to `.git/config`; a hard link to `.git/hooks/x` (refused for `nlink > 1`); a
    hard link to a file outside the root (refused for `nlink > 1`); `.GIT/config` (refused on every
    OS: on a case-insensitive volume it is `.git`; on Linux's case-sensitive file systems `.GIT` is
    a different folder from `.git`, and refusing it there is **intended over-caution**, which the
    test's name and comment say, because the floor folds case so that a repository copied from
    Windows or macOS stays guarded); `GIT~1/config` on Windows with short names enabled on the test
    volume (`fsutil 8dot3name set <vol> 0`); `.git./config` on Windows; `.env` without
    `confirmed: ["secret"]` (refused with `needs`) and with it (written); `.mcp.json` without and
    with `confirmed: ["harness"]`;
  - **the daemon's folders (N1):** with the default layout in a temporary `<data>`, a save to a file
    in a session's scratch folder, to a file in the Home environment's workspace and to a file in a
    worktree under `<data>/worktrees` succeeds; a save to `daemon.json` reached through a bind mount
    of `<data>` placed under a root (Linux) is refused by identity and `daemon.json` is unchanged;
  - **re-judgement:** with the save parked at `before_rename`, the test renames the target's parent
    into a `.git` folder of the same root and releases; the re-judgement refuses the rename and
    nothing is written there;
  - **metadata:** on Linux a file with mode `0640`, a `user.test` extended attribute and a POSIX
    ACL keeps all three; on macOS a file with `com.apple.quarantine` and an ACL entry keeps both; on
    Windows a file with an explicit DACL entry, the read-only attribute cleared and a
    `Zone.Identifier` stream keeps the DACL entry and the stream; a file whose group differs from
    the user's primary group keeps it (Linux and macOS, a supplementary group of the runner's
    user); a file owned by another user (Linux, made with the runner's `sudo`) is written in place
    with a `note`; on Windows a compressed file (`compact /c`) and a sparse file (`fsutil sparse
    setflag`) are written in place with a `note` and keep their attribute; a metadata copy made to
    fail through a test hook falls back to in-place with a `note`;
  - **in place without a path race:** with an in-place save parked after the judgement, the test
    replaces the file's name with a link to outside (Unix) or another file (Windows) and releases;
    the write goes to the judged file (Windows `ReOpenFile`, Linux `/proc/self/fd`) or is refused
    by the file-id check (macOS), and nothing outside changes; a shorter new text leaves no old
    bytes after it (the truncate ran);
  - **Windows rename order:** with `FileRenameInfoEx` made to fail as unsupported through a test
    hook, the save uses `FileRenameInfo` with `RootDirectory`; with both failing, it writes in
    place with a `note`; on an exFAT volume (a VHD the test creates and mounts with
    `New-VHD`/`Mount-VHD` or `diskpart` when the runner allows it) and on an SMB loopback share
    (`New-SmbShare` on `\\localhost\`) the save writes in place with a `note`; if the runner allows
    neither, those two are named known gaps and the hook-driven fallbacks still run;
  - **crash and cleanup:** a save stopped at `before_rename` and abandoned leaves the original
    intact; the leftover is removed when the root is next opened; a leftover-looking name that is a
    directory, a link or has two hard links is left alone;
  - on Windows a target held open without delete sharing is retried and then written in place
    with a `note`, or fails `FILE_BUSY` when it cannot be opened for writing;
  - **the gate:** a call over loopback TCP, a call from a real child process carrying
    `IMPERIUM_SPAWNED_BY_DAEMON`, a stub identity that is not the installed app, and a stub
    identity that is a debug build against a release daemon are refused with `-32005`; the phone
    is refused; with `files.write_enabled = false` every write is refused;
  - **agents and the user:** an `edit_file` and a `files.write` of the same file, ordered by
    barriers both ways, serialise (the save conflicts, or the edit applies to the saved text).
- Size: about 800 lines (E1c split possible, above).
- Risks: `FileRenameInfoEx` availability (checked); antivirus on the Windows runner (the retries).
- Status: pending
- Outcome: -

### Stage E2: The native relay

- Objective: `src-tauri/src/file_write.rs`: one Tauri command, `files_write`, relayed on the
  app's own local-socket connection (no lease), granted to `main` and `panel-*` for the app's own
  origin only; refused while a trusted main-context mod is or was loaded (item 55's latch, shared,
  not duplicated); refused when the window is connected to a different daemon than `daemon.json`
  names (item 55 threat 11); refused when a WebView2 policy key of D3 is set (the check shared with
  terminals). `src/files/bridge.ts` on the webview side is the only caller. The Tauri version
  floor test.
- Depends on: stage E1b; item 55 stage 3 (the relay pattern and the latch).
- Owned paths: `apps/desktop/src-tauri/src/file_write.rs` (new), `src-tauri/src/webview2_policy.rs`
  (new, shared with the terminal relay), `main.rs`, `build.rs` (build info), `capabilities/default.json`,
  `apps/desktop/src/files/bridge.ts`, `apps/desktop/test/filesBridge.test.ts` (new),
  `docs/06-extensibility.md`.
- Acceptance checks: `cargo test -p imperium-desktop file_write` on the matrix: the `tauri::test`
  access test refuses `files_write` from a forged mod-frame origin, a `localhost` Preview origin
  and a remote URL, and accepts the app's own origin; with the latch set every call is refused;
  with a different daemon URL the call is refused; on Windows, with `AdditionalBrowserArguments`
  or `BrowserExecutableFolder` set for the app under a test-only registry root, the call is
  refused; the version test fails on a `Cargo.lock` with `tauri` below 2.0.0-beta.20 (checked
  against a fixture lock file). `npm test`: a source scan fails if any file other than
  `src/files/bridge.ts` names `files_write`.
- Size: about 500 lines.
- Risks: changing `main.rs` and `capabilities/` runs the `presence-e2e` job (expected).
- Status: pending
- Outcome: -

### Stage E3a: Editable buffer and save

- Objective: item 55's viewer becomes editable when `writable` is true (D1, D8, D13): the
  editing extensions (history, default keymap with `indentWithTab`, search with replace,
  bracket matching, indentation detection), the read-only state with the reason when not,
  the status bar (encoding, BOM, line endings with the mixed notice and a picker, cursor
  position), Save through `bridge.ts` with the base hash, the secret-file confirmation per tab and
  the harness-file dialog with its baseline diff (D7), the dirty marker, *Leave editor* (F6), and
  the **Save** shortcut. `FILE_CONFLICT` here shows a plain "changed on disk" notice with *Reload*
  (stage E4a replaces it). The fake daemon gains `files.write` (through the IPC mock),
  `files.stat`, `files.baseline` and an on-disk fixture it can change.
- Depends on: stage E2; item 55 stages 4 and 7a.
- Owned paths: `apps/desktop/src/editor/buffer.ts`, `encoding.ts` (new),
  `src/panels/FilesPanel.tsx`, `src/files/`, `src/settings/shortcuts.ts`, `src/styles/editor.css`
  (new), `apps/desktop/package.json` and `pnpm-lock.yaml` (`@codemirror/commands`,
  `@codemirror/search` pinned per D1), `apps/desktop/test/editorBuffer.test.ts` (new),
  `apps/desktop/e2e/editor.spec.ts` (new), `apps/desktop/e2e/` fake daemon, `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test` and `npm run typecheck`: the save text for `lf`, `crlf` and
  `mixed` files (untouched lines keep their endings in `lf` and `crlf` files; `mixed` saves with the
  majority); the final newline kept; the base sent is the loaded hash; a buffer over 4 MiB in UTF-8
  is refused before sending. Browser checks against the fake (Chromium and WebKit): open, type,
  Ctrl+S, and the fake's file holds the text; the dirty dot appears on typing and goes on save;
  undo after save makes the tab dirty again; find and replace replaces every match; a `.env`
  fixture asks once per tab, then saves without asking again, and asks again in a new tab; a
  `.mcp.json` fixture asks on every save and shows the baseline diff and the agent-changed note;
  `.git/config` opens read-only with the reason and typing changes nothing; a 5 MiB file opens in
  the capped read-only view; F6 moves focus out of the editor and Esc then Tab leaves it.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E3b: Tabs, settings and themes

- Objective: tabs per session (open, switch, close, reorder by keyboard, at most 20, with the
  oldest clean tab closed first past that), *Save all*, *Close tab*, the leave-with-unsaved
  dialogs (D16), the `editor` app-store document of open tabs (paths and positions only), the
  tree's *New file* (`create: true`), Settings → Editor (D15), the `--syntax-*` and editor tokens
  for both themes (D14) with the contrast test, and forced-colours rules.
- Depends on: stage E3a; item 55 stage 7b (the tree).
- Owned paths: `apps/desktop/src/editor/tabs.ts` (new), `src/panels/FilesPanel.tsx`,
  `src/files/tree*`, `App.tsx` (close dialog), `src/layout/` (the detach prompt only),
  `src/settings/settings.ts`, `EditorSection.tsx` (new), `sections.ts`, `shortcuts.ts`,
  `src/styles/tokens.css`, `editor.css`, `forced-colors.css`, `apps/desktop/test/editorTabs.test.ts`
  (new), `tokenContrast.test.ts`, `apps/desktop/e2e/editor.spec.ts`, `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`: tab-list rules (cap, close order, restore from the stored
  document, a damaged document falls back to none); every `--syntax-*` colour at 4.5:1 on the editor
  background in both themes and all accents. Browser checks: three tabs open and switch by
  keyboard; closing a dirty tab asks and *Cancel* keeps it; switching session with a dirty tab asks;
  reopening the app (a page reload with the store kept) restores the tabs without content; **after
  typing a sentinel without saving, it is absent from `localStorage`, every IndexedDB database and
  the fake's record of app-store writes**; switching the theme recolours the open editor without
  reopening it; *New file* creates the file and refuses an existing name; auto save stops at a
  secret file not yet confirmed.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E4a: External changes and conflicts in the editor

- Objective: D9 in full: change detection (tool results, focus, visibility, the 2-second poll
  of the visible tab through `files.stat`), clean-buffer reloads as minimal changes with the
  notice and *Show changes*, the conflict banner with *Compare* (`@codemirror/merge`), *Merge*
  (three-way, `src/editor/merge3.ts`, pure), *Use file* and *Keep mine*, `FILE_CONFLICT` and
  `FILE_BUSY` on save, *Restore my save*, and the deleted-on-disk state.
- Depends on: stage E3b.
- Owned paths: `apps/desktop/src/editor/conflicts.ts`, `merge3.ts` (new),
  `src/panels/FilesPanel.tsx`, `src/editor/`, `apps/desktop/package.json` and `pnpm-lock.yaml`
  (`@codemirror/merge`), `apps/desktop/test/merge3.test.ts` and `editorConflicts.test.ts` (new),
  `apps/desktop/e2e/editorConflicts.spec.ts` (new), the fake daemon (scripted `edit_file` tool
  results), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`: `merge3` merges non-overlapping edits (both sides kept, in
  order), reports overlapping ones with their ranges, treats identical changes on both sides as
  one, and handles an empty base, a deleted file and line-ending-only differences; the reload's
  change set keeps a cursor placed after the change at the same text. Browser checks against the
  fake, with a scripted agent `edit_file` and its `tool_result`:
  - clean buffer: the text updates, the cursor stays on the same word, the notice names the agent,
    Ctrl+Z restores the old text and marks the tab dirty;
  - dirty buffer: the buffer is unchanged, the banner shows; *Merge* of a non-overlapping change
    keeps both; an overlapping change opens *Compare* at the conflict; *Use file* replaces the
    buffer; *Keep mine* asks once and then saves with the new base;
  - a save while the fake's file changed underneath returns the conflict and writes nothing; a
    `FILE_BUSY` answer retries once and then says so;
  - an external change with no tool result (the fake changes the file) is found by the poll within
    3 seconds while the window has focus, and not polled while it does not;
  - *Restore my save* after an agent's full-file write re-saves the user's text;
  - a deleted file keeps the buffer and *Save* recreates it.
- Size: about 800 lines.
- Risks: the three-way merge on very long files (it runs on line hashes; capped by the 4 MiB
  editable limit).
- Status: pending
- Outcome: -

### Stage E4b: Agents learn about user saves

- Objective: D10: the last-read records per agent instance (session id plus agent id; keyed by
  lock key; a hash up to 4 MiB, `{file_id, size, modified_ms}` above it, `inconsistent` when pages
  disagree), copied on branch and comparison forks and emptied by a restart; `write_file`'s
  refusal of a full overwrite of an existing file changed since that agent's last read, with the
  "saved by the user" wording only when the daemon's save record matches; the switch
  `harness.stale_write_check` and the eval variants' `stale_write_check` flag (default off); the daemon's
  per-session record of user saves; and the next-turn note behind `harness.user_edit_notes`,
  labelled as data, with names as JSON strings, unusual names left out and counted, and at most
  20 names. Before it merges, the research session is told that `write_file` behaves differently
  and the eval suites are rerun with the check on and off.
- Depends on: stages E1a and E1b.
- Owned paths: `crates/imperium-tools/src/files.rs`, `src/workspace.rs` and a new
  `src/last_read.rs` (the records, held per agent instance), `crates/imperium-engine/src/agent/`
  (the turn's opening note, the fork copy, the per-agent handle), `crates/imperium-core/src/eval/variant.rs`
  (the variant flag), `crates/imperium-core/src/files/write.rs` (recording saves),
  `crates/imperium-config/src/settings.rs` and `registry.rs`, the tools' and engine's tests,
  `docs/03-agent-engine.md`, `docs/05-tools-and-isolation.md`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-tools -p imperium-engine -p imperium-core` on the
  matrix:
  - a `read_file`, then a user `files.write`, then a `write_file` of the same file is refused with
    "(saved by the user in the editor)" and the file is unchanged; after a fresh `read_file` the
    `write_file` succeeds;
  - a `read_file`, then a change by a scripted `run_command` (a formatter), then a `write_file` is
    refused **without** the user wording; with `harness.stale_write_check = false` it succeeds;
  - **parallel subagents:** subagents A and B both read `f`; A writes `f`; B's full `write_file` of
    `f` is refused and B's after a fresh read succeeds; A's own second write (its record updated by
    its write) succeeds;
  - a branch forked after a read keeps the record (its write over an unchanged file succeeds, over
    a changed one is refused); a new subagent starts empty;
  - a file over 4 MiB: unchanged since the read, the write succeeds; with `modified_ms` changed, it
    is refused; a paged read whose pages disagree records `inconsistent` and the write is refused;
  - after a daemon restart (the map dropped) the first write passes, as documented;
  - a `write_file` of a new file and of an existing file never read succeed as today; an
    `edit_file` after a user save applies to the saved text;
  - **the note:** with the setting on, a turn after two user saves starts with the labelled note
    naming both files as JSON strings and no content, and the next turn has none; a saved file named
    with a right-to-left override or a newline is left out and counted; 25 saves list 20 names and
    "and 5 more"; with the setting off, no note;
  - a headless eval variant without the flag shows the old behaviour (its default is off) and one
    with it refuses as a session does; the eval runner's existing tests pass unchanged.
- Size: about 650 lines.
- Status: pending
- Outcome: -

### Stage E5: Markdown preview

- Objective: D11: the preview beside the source for `.md` and `.markdown` (and *Toggle Markdown
  preview*), for read-only files as well as editable ones; the parse worker with its time, depth and
  size bounds; `react-markdown` and `remark-gfm` pinned exactly for the transcript; the element
  allowlist and attribute stripping; prefixed ids and footnotes; links as buttons after
  `defaultUrlTransform` and the classifier; the shared link function in `src/shared/links.ts`
  (moved from item 55's `policy.ts` if that has landed); the confirmation dialog; image
  resolution through `files.read`, lazy and capped; code highlighting into React spans; scroll
  sync both ways; off above 1 MiB.
- Depends on: item 55 stages 6b (`files.locate`) and 7a (the viewer). Not on the write path.
- Owned paths: `apps/desktop/src/editor/preview/` (new, with `worker.ts`), `src/shared/links.ts`
  (new or moved), `src/terminal/policy.ts` (only to import the moved function),
  `src/panels/FilesPanel.tsx`, `src/styles/editor.css`, `apps/desktop/package.json` and
  `pnpm-lock.yaml` (exact pins, `unified`, `remark-parse`, `remark-rehype`,
  `hast-util-to-jsx-runtime`, `@lezer/highlight`), `apps/desktop/test/markdownPreview.test.ts` and
  `links.test.ts` (new), `apps/desktop/e2e/markdown.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`: the image path resolver (`./a.png`, `../img/a.png` inside the
  root, `../../x.png` climbing out refused before any call, an absolute path refused, a `%2e%2e`
  spelling refused); the link classifier for `#anchor`, relative in and out of the folders,
  `http`, `https`, `mailto`, `javascript:`, `JaVaScRiPt:` with whitespace and entities, `data:`,
  `vbscript:`, `file:`, an IDN host shown as punycode; the worker's bounds (a 10,000-deep nested
  list and a 300,000-node file each end in "too complex" within 3 s). Browser checks against the
  fake with a hostile fixture:
  - `<script>`, `<img src=x onerror>`, `<iframe>`, `<style>`, an `<a href="javascript:">` in raw
    HTML and a Markdown `[x](javascript:...)` all appear as text or inert, with no dialog and no
    request to the fake's sentinel URL;
  - **DOM scan:** after rendering the fixture, a scan of the preview's subtree finds no element
    outside the allowlist, no `<a>`, no attribute starting with `on`, no `style` attribute, no
    `src` other than a `data:` image the fake served, and no id without the tab's prefix;
  - a remote image makes no request and shows its placeholder; a local PNG shows; a `.txt` renamed
    `.png` does not; an SVG shows a placeholder; 60 local images load at most 50, and only those
    near the view load before scrolling;
  - an `https` link opens only after the confirmation and calls the opener with the exact address;
    a relative `.md` link opens that file in a tab; a footnote reference jumps to its note;
  - typing updates the preview; a click in the preview moves the cursor to that line; a read-only
    Markdown file previews.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E6: Presentation mode

- Objective: D12: `src/editor/slides.ts`, the overlay, keys, the live region, *Full screen*
  through Tauri (the permission added to `capabilities/default.json`), the presentation theme,
  reduced motion, the **Start presentation** shortcut and palette command.
- Depends on: stage E5. Not on the write path.
- Owned paths: `apps/desktop/src/editor/slides.ts`, `src/editor/present/` (new),
  `src/panels/FilesPanel.tsx`, `App.tsx` (palette command), `src/settings/shortcuts.ts`,
  `apps/desktop/src-tauri/capabilities/default.json` (`core:window:allow-set-fullscreen`),
  `src/styles/editor.css`, `apps/desktop/test/slides.test.ts` (new),
  `apps/desktop/e2e/present.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test` (`slides.test.ts`): three slides from two top-level `---`; no
  split on `---` inside a code fence, a list item or a blockquote, or under a setext heading;
  front matter dropped; `***` and `___` split; no break splits at `#` and `##`; empty slides
  dropped; the slide for a source line. Browser checks: F5 opens slide 1 with "Slide 1 of 3"
  announced; Right, Space and Page Down advance, Left goes back, End and Home jump, `2` Enter jumps;
  Esc closes and focus returns to the editor at the slide's line; Tab stays inside the overlay; a
  hostile slide (the E5 fixture) passes the same DOM scan; editing the file while presenting keeps
  the current slide; with reduced motion there is no transition; a read-only Markdown file
  presents. The `tauri::test` access test (stage E2) adds that the full-screen permission is
  granted to the app's own origin only.
- Size: about 650 lines.
- Status: pending
- Outcome: -

### Stage E7: End to end and docs

- Objective: the close-out. The Linux end-to-end run (`scripts/ci/presence-e2e/`, release app and
  daemon under Xvfb through `tauri-driver`) gains an `editor` scenario: open a session, open a file
  in the Files panel, type a sentinel, save, and read the bytes on disk; change the file on disk and
  see the reload notice; then a **real mod frame and a Preview page** each try
  `window.__TAURI_INTERNALS__.invoke("files_write", …)` and a direct `fetch` to `ipc.localhost`,
  and a process started with the spawn mark calls `files.write` on the local socket and over
  loopback TCP: every attempt fails and the file is unchanged. A Windows run of the same scenario
  through `tauri-driver` and a matching `msedgedriver` (D17), or a recorded gap.
  `scripts/ci/detect.sh` adds the editor and relay paths to the job's watched list. Docs: 02
  (methods, error codes), 03 (the next-turn note), 05 (the user's editor is outside permission
  rules; guarded targets; every writer takes the file lock; `write_file`'s stale-read refusal), 06
  (mods get no editor), 07 (the editor, preview, presentation, shortcuts, settings), 10
  (`files.write_enabled`, `harness.user_edit_notes`), 15 (the phone gets no editor), 12 (item 66
  implemented); this plan folded into the lasting docs.
- Depends on: stages E4a, E4b, E5 and E6; item 55 stage 8 (its `panes` scenario, which this
  extends).
- Owned paths: `scripts/ci/presence-e2e/`, `.github/workflows/presence-e2e.yml` (the Windows run),
  `scripts/ci/detect.sh`, `scripts/ci/test-detect.sh`, `docs/**`, `docs/12-roadmap.md`.
- Acceptance checks: the `presence-e2e` Linux job runs the `editor` scenario green, attempts
  included, and the Windows run is green or recorded as a gap with the reason; docs checks
  (`python3 scripts/ci/check-links.py .` and markdownlint) green; `cargo test --workspace` and
  `scripts/check-web.sh` green.
- Size: about 550 lines.
- Status: pending
- Outcome: -

Order: E1a, E1b, E2, E3a, E3b, E4a, E4b, E7. E5 and E6 need only item 55's viewer, so they may
land before or alongside the write-path stages; E4b may run in parallel with E2 to E4a once E1b has
landed. Revision 4's stages follow E2: E8, then E9 (after item 68 stage 1), then E10a (which
needs only E3a and may come earlier), E10b and E10c (before any server runs outside tests: E11
does not merge before E10c), E11, E12 (after E4a), E13 (any time after E9 and E10a), E14 (its
theme part after E3b, at any time) and E15 last, after E7.

**Dependencies on other items:** item 55 stages 2b and 3 (the gate and relay pattern, through
E2), 4 (browser checks), 6a and 7a (reads and the viewer, for E10a and E11); item 68 stage 1
(`imperium-limits`, for E9) and its settings pattern for a class (`limits.<class>`); the floor
rules of pull requests #304 and #330 (E8 and E10b follow their pattern; if they merge first,
the entries are added beside theirs).

### Stage E8: Project trust

- Objective: D20's trust gate on the existing list: `projects.trust` and `projects.untrust`
  (protocol minor at merge), answered only through the native relay and to a caller that passes
  `may_grant`; trust scope by the environment's project folder, identity first and canonical
  path as fallback, worktrees by git common-folder identity (D20, major 1); the one-time
  consent `lang.consent` for projects trusted before revision 4 (major 2);
  `config.set` and `config.unset` of `mcp.trusted_projects` refused to a caller that
  fails `may_grant`; the safety floor **refuses** `imperium config set|unset mcp.trusted_projects`
  from an agent's command (any case of the key, `--remote` anywhere, as
  `changes_plugin_setting` matches); `FileContent.project_trusted`; the editor's trust banner
  with *What runs?*; Settings → *Trusted projects* (list, add by folder picker, remove); the docs
  call the concept "trusted projects" (05, 10, 14), the key unchanged.
- Depends on: stage E2 (the relay file and its access test); item 55 stage 6a (`FileContent`).
- Owned paths: `crates/imperium-core/src/rpc/projects.rs` (new), `rpc/config.rs`,
  `crates/imperium-policy/src/floor.rs`, `crates/imperium-protocol/src/methods/projects.rs` (new),
  `methods/files.rs`, snapshots and generated types, `apps/desktop/src-tauri/src/file_write.rs`,
  `capabilities/default.json`, `apps/desktop/src/editor/trust.tsx` (new), `src/settings/` (a
  *Trusted projects* section), `apps/desktop/e2e/trust.spec.ts` (new),
  `docs/05-tools-and-isolation.md`, `docs/10-configuration.md`,
  `docs/14-projects-and-automation.md`.
- Acceptance checks: `cargo test -p imperium-core -p imperium-policy` on the matrix: the agent
  commands `imperium config set mcp.trusted_projects '["/x"]'`, `config set
  MCP.Trusted_Projects …` and `config --remote box unset mcp.trusted_projects` are refused in
  every permission mode, Bypass included; `projects.trust` over loopback TCP, from a child
  carrying the spawn mark and from a stub non-app identity is refused with `-32005`; from the
  stub app it adds the folder, compared by identity (a second spelling is not added twice); a
  subfolder of a trusted folder is not trusted; a phone is refused. `tauri::test`: the trust
  command refuses a mod-frame origin. Browser checks: the banner shows once per project, *Trust
  project* calls the relay, *Not now* keeps servers off and the banner does not return in the
  session.
  Revision 5 adds (majors 1 and 2):
  - **scope:** a file of a session whose environment's project folder is trusted is trusted; a
    folder reached through a junction (Windows) or a bind mount or symbolic link (Unix) to the
    trusted folder matches by identity, and a different folder later created at a trusted path
    does not; an auto-fix worktree of a trusted repository is trusted (same git common-folder
    identity) and a worktree of an untrusted repository is not; a project under a trusted
    parent folder is not trusted;
  - **re-consent:** with a project in `mcp.trusted_projects` and no consent record, opening a
    Rust file starts no server (the fake's sentinel absent) and shows the one-time consent
    banner; *Allow* records it and the server starts; a project trusted from the banner after
    revision 4 is not asked again;
  - the floor checks run against #330's rule if it has landed, and the refusal holds whichever
    lands first.
- Size: about 600 lines.
- Status: pending
- Outcome: -

### Stage E9: The LSP client in the daemon

- Objective: D18 and D21: the `imperium-lsp` crate (framing with the 16 MiB bound, lifecycle,
  capabilities, incremental and full sync, versions, cancellation after 10 s, the server-request
  policy, URI confinement, result caps, semantic-token decoding as a pure function); the
  daemon's `LspHost` per server and project folder, started only for a trusted project, a found
  server and `editor.lang.enabled`; spawning through `imperium-limits` as the `lsp` class (added
  to item 68's `Class`, L6 table and settings), with the environment scrubbed and the spawn mark
  set; the strictest OS tier of the project's open sessions (D21) and the `lsp` place in the
  watchdog's order (item 68 L11); the sync owner per URI and watched-file registrations (D18);
  `showDocument`'s focus rule; at most `max_servers`, idle stop, restart backoff and the stopped
  status;
  `didChangeWatchedFiles` from the daemon's write record; `lang.open`, `lang.change`,
  `lang.close`, `lang.status` and the `lang.diagnostics` event (protocol minor at merge); the
  fake server (D27). If it passes about 800 lines, it splits into E9a (the crate and fake
  server) and E9b (the host, caps and methods).
- Depends on: stage E8; item 68 stage 1 (`imperium-limits`) and its pattern for a class's
  settings; stage E2 (the relay).
- Owned paths: `crates/imperium-lsp/` (new), `Cargo.toml` (workspace member, `lsp-types`
  0.97.0 exact), `crates/imperium-core/src/lang/` (new), `rpc/lang.rs` (new), `rpc.rs`,
  `crates/imperium-limits/src/` (the class), `crates/imperium-config/src/settings.rs`,
  `registry.rs`, `crates/imperium-protocol/src/methods/lang.rs` (new), snapshots and generated
  types, `docs/02-architecture.md`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-lsp -p imperium-core --test lang` on the matrix,
  with the fake server:
  - the handshake sends `initialize` once with one `workspaceFolders` entry, then
    `initialized`; stopping sends `shutdown` then `exit`, and a server that ignores them is
    stopped with its unit after 5 s;
  - incremental changes reach a server that asked for them, full text one that did not; a
    diagnostics set for an older version is kept and a set for an unknown version dropped;
  - **untrusted:** a `lang.open` in an untrusted project starts nothing (the fake's sentinel
    file is absent) and `lang.status` says `untrusted`; trusting starts it; untrusting stops it;
  - **policy:** the fake's `workspace/applyEdit` is not applied (the file is unchanged) and is
    held as a pending review; `window/showDocument` for a file outside the folder and for an
    `https:` URI opens nothing; an unknown request gets `MethodNotFound`; a diagnostic whose URI
    is outside the folder is dropped and counted; a 20 MiB message ends the server; 2,000
    diagnostics for one file are cut to 1,000;
  - **caps:** a fake server that allocates past a test `lsp` cap is stopped, restarted after the
    backoff, and after the third stop in ten minutes reports the cap, the value and
    `limits.lsp.memory_mb`; its child process is stopped with it; five servers with
    `max_servers = 4` leave four running, the least recently used stopped;
  - **environment:** the fake records its environment: no daemon token, endpoint or
    `IMPERIUM_*` variable except the spawn mark; a `may_grant` check of the fake's process fails;
  - **gate:** `lang.open` over loopback TCP, from a marked child and from a phone is refused;
  - semantic tokens decode the specification's example `[2,5,3,0,3, 0,5,4,1,0, 3,2,7,2,0]` to its
    three tokens, and a delta's edits apply sorted from the back;
  - **tier:** with a session of the project at tier 1, the fake (on Linux and macOS) cannot
    write outside the project and its cache folder (a write to the home folder fails and the
    fake reports it); opening a stricter session restarts it at that tier; on Windows the status
    reports tier 0;
  - **watchdog:** with the test watchdog under pressure and an `lsp`, an `mcp` and a foreground
    `command` unit over 256 MiB, the `lsp` or `mcp` unit is stopped before the command;
  - **watched files:** a fake that registers `**/*.rs` gets `didChangeWatchedFiles` for an
    agent's write to `src/a.rs` and not for `README.md`; a registration for `/etc/**` is
    refused; a fake that registers nothing gets the open file re-synced on focus;
  - **sync owner:** two tabs on one file send one stream of versions; focusing the second
    re-sends full text with a new version;
  - `window/showDocument` with `takeFocus: true` not following a user action opens a background
    tab and does not move focus;
  - **tier with no session:** with no open session and the environment configured for tier 1,
    the fake runs at tier 1, not 0; a scheduled run on the project counts as an open session;
  - **client watching:** against the real `typescript-language-server` (installed in the job, as
    in E15), the test checks whether `tsserver` is started with `--canUseWatchEvents` and the
    server forwards its watch requests; the result is recorded in the catalogue entry, and D28
    relies on the freeze and sweeps where it is not;
  - **Windows shims:** a planted `node.bat` in the project folder and a relative `.` entry on
    `PATH` are not run when the fake server is installed as an npm `.cmd` shim: the daemon
    launches the absolute `node.exe` with the shim's script, the child's environment has
    `NoDefaultCurrentDirectoryInExePath=1`, and its `PATH` has no relative or in-project entry;
    a shim that is not in npm's form is refused.
- Size: about 800 lines (E9a and E9b split possible).
- Risks: item 68 is not merged; if E9 is ready first, the `lsp` unit waits for item 68's stage
  1 crate with the class added there, never a local job wrapper.
- Status: pending
- Outcome: -

### Stage E10a: Built-in checks and the lint view

- Objective: D20's built-in checks and D22's display: `@codemirror/lint` 6.9.7 pinned;
  `src/editor/lang/builtinChecks.ts` (Lezer error nodes as "Syntax error (built-in)" for the
  Lezer languages); `lang.check` in the daemon (JSON and JSON with comments through
  `serde_json`, TOML through `toml`, schemas through `jsonschema` 0.61.1 with default features
  off, the bundled schemas chosen by file name); squiggles, gutter, tooltip, `lintKeymap`, F8 and
  Shift+F8, the open file's problem list, the status-bar counts, the `--diag-*` tokens. Works
  with no server and in untrusted projects.
- Depends on: stage E3a; item 55 stage 7a.
- Owned paths: `crates/imperium-lsp/src/check.rs`, `schemas/`, `crates/imperium-core/src/rpc/lang.rs`,
  `Cargo.toml` (`jsonschema`), `apps/desktop/src/editor/lang/lint.ts`, `builtinChecks.ts`,
  `problems.ts` (new), `src/settings/shortcuts.ts`, `src/styles/tokens.css`, `editor.css`,
  `apps/desktop/package.json` and `pnpm-lock.yaml`, `apps/desktop/test/builtinChecks.test.ts`
  (new), `apps/desktop/e2e/lint.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: `cargo test -p imperium-lsp check`: a JSON file with a trailing comma
  reports its line and column; JSON with comments accepts comments; a TOML duplicate key is
  reported; a `package.json` whose `name` is a number fails the bundled schema; a file with
  `"$schema": "https://example.invalid/x.json"` and a schema `$ref` to a remote or `file:` URL
  make no request and read no file (a test resolver fails the test if called); 4 MiB plus one
  byte is refused. `npm test`: a TypeScript, a Rust and a Python fixture with one syntax error
  each report exactly it; a valid file reports none; the `--diag-*` colours pass the contrast
  test. Browser checks: an untrusted fixture project with an unclosed brace shows a squiggle, a
  gutter mark and one problem; F8 moves to it; fixing it clears all three; no `lang.open` is sent
  for an untrusted project (the fake records none).
- Size: about 700 lines.
- Status: pending
- Outcome: -

### Stage E10b: The catalogue, discovery, install offers and program approvals

- Objective: D19 and the approval part of D20: `catalog/servers.json` with the table's servers;
  discovery (the catalogue's places, `rustup which`, `go env GOPATH`, the user override); version
  probes run as `internal` units with a 5 s limit; every probe and install run in the neutral
  folder `<data>/lsp-work` with the B1 environment and explicit registries (D19); a project's
  toolchain treated as a project-local program; the status per language; the missing-server
  notice; the install dialog and `lang.install` (the catalogue's exact command only, as a
  `command` unit, output streamed); `lang.approve_program` and `lsp-approvals.json` (path and
  hash, asked again on change); and the safety floor's always-ask entries for install commands.
- Depends on: stage E9.
- Owned paths: `crates/imperium-lsp/catalog/`, `src/discover.rs` (new),
  `crates/imperium-core/src/lang/`, `rpc/lang.rs`, `crates/imperium-policy/src/floor.rs`,
  `crates/imperium-config/src/settings.rs`, `registry.rs`, `environment.rs`,
  `apps/desktop/src/editor/lang/install.tsx` (new), `apps/desktop/e2e/lint.spec.ts`,
  `docs/05-tools-and-isolation.md`, `docs/07-desktop-ui.md`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-lsp -p imperium-core --test lang -p imperium-policy -p imperium-config`:
  - the program approval binds as D20 says: a package folder within the bound is hashed (a
    changed file inside it asks again), one past 20,000 files binds the lockfile and the dialog
    says so; `lsp-approvals.json` written by an agent's `write_file` or `run_command` is refused
    by the floor and by D7;
  - discovery finds a fake `rust-analyzer` placed on a test `PATH` and the user override's
    program; a `node_modules/.bin/eslint` in an untrusted project is never run, and in a trusted
    one only after `lang.approve_program` (the fake records each start); changing the approved
    file's bytes asks again;
  - **no install unasked:** opening files of every catalogue language with no server installed
    starts no install process (the test's units show only probes); a `lang.install` whose
    command was altered in the request, or from a non-app caller, is refused; the approved offer
    runs exactly the catalogue's argument list as a `command` unit;
  - the floor asks every time, in every mode, for `rustup component add rust-analyzer`,
    `npm install -g pyright`, `npm i -g typescript-language-server`, `pipx install ruff`,
    `uv tool install ruff`, `go install golang.org/x/tools/gopls@latest` and
    `cargo install taplo-cli --locked`, and offers no "always allow";
  - the environment layer cannot name a server (`[editor]` server entries in
    `.imperium/config.toml` are ignored and reported) but can turn servers off for itself.
  Browser checks: the missing-server notice offers *Install*; *Install* shows the exact command;
  *Cancel* runs nothing; *Run this install* runs it once and the status turns to running.
  **B1, hostile fixture.** An untrusted project holds `.npmrc` (`registry=` a local sentinel
  server, `script-shell=` a sentinel script), `.cargo/config.toml` (`build.rustc-wrapper` a
  sentinel script), `rust-toolchain.toml` (`path =` a fake toolchain that writes a sentinel),
  `go.mod` (`toolchain go9.99.0`, which `GOTOOLCHAIN=auto` would fetch), `uv.toml` and
  `pip.conf` (index URLs at the sentinel server), and on Windows a planted `node.bat` and
  `npm.cmd` in the project folder. With that project open: `lang.status`,
  discovery for every language and an approved install of each catalogue command (run against
  fakes of `npm`, `uv`, `pipx`, `cargo`, `go` and `rustup` on the test `PATH` that record their
  working folder, arguments and environment) leave **no sentinel** and no request to the
  sentinel server; every recorded working folder is `<data>/lsp-work`; the environment has
  `GOTOOLCHAIN=local` and `RUSTUP_AUTO_INSTALL=0` and no `RUSTC_WRAPPER`; npm got
  `--registry https://registry.npmjs.org/` and `npm_config_userconfig` naming the user's file;
  on Windows the planted `node.bat` and `npm.cmd` never run and
  `NoDefaultCurrentDirectoryInExePath=1` is set.
  The same project trusted: the `path` toolchain is not used until approved, and the status
  says the default toolchain runs.
- Size: about 800 lines (the neutral-folder runner may split into its own E10b2 if it grows).
- Status: pending
- Outcome: -

### Stage E10c: Pause and resume on agents' changes, and build code off by default

- Objective: D28: the execution-relevant file list (`exec_files.rs`, with the proc-macro crate
  rule and the one-level ESLint import follow); the fingerprint per project; the pause before
  agents' file-tool writes, rewinds and hunk reverts to those files, and the re-check after
  agents' commands and before forwarding `didSave`, `didChangeWatchedFiles` and any start,
  restart or re-index; the stored previous versions (up to 1 MiB each) and the `lang.paused`
  event; the banner with names, authors and diffs; `lang.resume` (gated, `may_grant`); *Run
  project build code* per project (`lang.set_build_code`), which sets rust-analyzer's
  `cargo.buildScripts.enable`, `procMacro.enable` and `checkOnSave`, default off; rust-analyzer's
  `files.watcher = "client"` and `cargo.buildScripts.rebuildOnSave = false`. Revision 6 (R1):
  the stat sweep before every forwarded message that evaluates project code, cached per
  debounce window; the 1 s poll while agent commands or background processes of the project
  are alive; `Unit::freeze` and `Unit::thaw` in `imperium-limits` (cgroup `freeze`, `SIGSTOP`
  and `SIGCONT`, job process suspension) around each foreground agent command, then sweep and
  thaw or pause; `node_modules/**` and `.venv/**` as execution-relevant for file tools;
  `--allowLocalPluginLoads` never passed.
- Depends on: stages E9 and E10b; stage E4a for the diff view in the banner (until then the
  banner lists files and hashes). E11 does not merge before this stage.
- Owned paths: `crates/imperium-lsp/src/exec_files.rs` (new), `crates/imperium-core/src/lang/`
  (pause, fingerprint), `crates/imperium-tools/src/files.rs` (the pre-write classification hook),
  `crates/imperium-core/src/state/` (rewind) and `src/review.rs` (hunk revert),
  `rpc/lang.rs`, `crates/imperium-protocol/src/methods/lang.rs`, snapshots,
  `apps/desktop/src/editor/lang/paused.tsx` (new), `apps/desktop/e2e/lint.spec.ts`,
  `docs/05-tools-and-isolation.md`, `docs/07-desktop-ui.md`.
- Acceptance checks: `cargo test -p imperium-lsp -p imperium-core --test lang -p imperium-tools`
  with the fake server standing in for rust-analyzer, in a trusted project whose `build.rs`
  writes a sentinel when run by the fake (the fake runs `build.rs` on start and on a re-index
  request, as rust-analyzer would with build code on):
  - **B2:** an agent's scripted `edit_file` to `build.rs` pauses the server **before** the write
    lands (the fake's process is gone when the file changes); the next start request, a
    `didSave` of `src/main.rs`, a forwarded `didChangeWatchedFiles` and a re-index request all
    leave **no sentinel**; `lang.status` says paused and `lang.paused` names `build.rs`, the agent
    and the session, with the diff; after `lang.resume` from the stub app the server starts and
    the sentinel appears (the user accepted the new code); a `lang.resume` from a marked child or
    over loopback TCP is refused;
  - the same for `eslint.config.js`, `.cargo/config.toml`, `rust-analyzer.toml`, `.clangd`,
    `compile_commands.json`, `package.json`, `pyproject.toml` and a `.rs` file in a crate with
    `proc-macro = true`; an agent's change to `src/lib.rs` of an ordinary crate and to
    `README.md` does not pause;
  - a change by a scripted agent `run_command` to `build.rs` is found when the command ends and
    pauses before the next forwarded notification; a change by the user's own `files.write` save
    does not pause;
  - **build code off by default:** in a newly trusted Rust project the fake receives
    `cargo.buildScripts.enable = false`, `procMacro.enable = false` and `checkOnSave = false`;
    after `lang.set_build_code` from the stub app it receives `true` for all three; from a
    non-app caller the call is refused;
  - nothing resumes on its own: after an hour of simulated time and a daemon restart the project
    is still paused (the fingerprint is kept in `lsp-approvals.json`);
  - **R1, background command:** a background agent command that writes `eslint.config.js` two
    seconds after it starts pauses the servers within 1 s of the write and **before** the next
    `didChange` reaches the fake (the fake records the order; it receives no `didChange` after
    the write);
  - **R1, sweep before forward:** with no command running, `eslint.config.js` changed by an
    outside process (the test writes it directly) is found by the sweep before the next
    `didChange`, diagnostic pull or code action is forwarded; a run of 50 keystrokes in one
    debounce window causes one sweep;
  - **R1, node_modules:** an agent's `edit_file` to `node_modules/eslint-plugin-x/index.js`
    pauses the servers before the write lands, as for `build.rs`;
  - **R1, freeze:** during a scripted foreground agent command in the project the fake server
    is suspended (on Linux its cgroup reports `frozen 1`, on macOS its state is stopped, on
    Windows its threads are suspended) and answers nothing; after the command, with nothing
    changed, it is thawed and answers; with `build.rs` changed by the command it is paused and
    the sentinel is absent; on Windows, if the runner cannot suspend job processes, the test is
    recorded as the named gap;
  - `tsserver` is started without `--allowLocalPluginLoads` (the fake records its arguments);
  - a file larger than 1 MiB changed by a command shows its old and new size and hash in
    `lang.paused`.
  Browser checks: the banner shows the files and the diff; *Keep paused* keeps it; *Resume*
  restarts and the diagnostics stop being marked stale.
- Size: about 800 lines (the freeze in `imperium-limits` may split into its own E10d).
- Status: pending
- Outcome: -

### Stage E11: Diagnostics, hover and completion from servers

- Objective: D22 with servers: the relay's `lang_request` and the `lang://diagnostics` event;
  buffer sync from the editor (open, changes debounced by line count between 150 and 800 ms,
  close); pushed and pulled diagnostics merged with the built-in checks; the project Problems
  view through `lang.problems`; hover through D11's pipeline; completion with
  `@codemirror/autocomplete` 6.20.4 (pinned), resolve, snippets as fields; `lint_on` per
  language; stale sets mapped through changes.
- Depends on: stages E9, E10a, E10b and E10c; stage E2.
- Owned paths: `apps/desktop/src/editor/lang/` (`sync.ts`, `hover.ts`, `complete.ts`,
  `problems.ts`), `src/files/bridge.ts`, `src/panels/FilesPanel.tsx`,
  `apps/desktop/src-tauri/src/file_write.rs`, `crates/imperium-core/src/lang/`, `rpc/lang.rs`,
  `apps/desktop/package.json` and `pnpm-lock.yaml`, `apps/desktop/test/langSync.test.ts` (new),
  the fake daemon's scripted `lang.*`, `apps/desktop/e2e/lint.spec.ts`, `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`: the debounce delay for 100, 10,000 and 100,000 lines (150,
  500 and 800 ms); a change set maps a diagnostic past an insertion; snippet text becomes fields
  and a snippet holding HTML stays text. Browser checks against the fake daemon scripted as a
  server: typing an error shows the squiggle within 2 s and fixing it clears it; the Problems
  view lists two files' problems and opens the second at its line; a hover whose Markdown has
  `<img src=x onerror>` and a `javascript:` link renders them inert (the D11 DOM scan);
  completion after `.` shows the items and inserts the chosen one; `lint_on = save` sends no
  change until the save. `cargo test -p imperium-desktop file_write`: `lang_request` refuses a
  method off its allowlist and a mod-frame origin.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E12: Quick fixes, the review dialog and format on save

- Objective: D23: `lang.actions` and `lang.run_action`; the lightbulb, Ctrl+. and *Fix*; buffer
  edits as one transaction; the review dialog for other files (diffs, ticks, harness and secret
  files unticked, base hashes, `files.write` with confirmations, open tabs edited in their
  buffer, resource operations refused, the 50-file and 4 MiB caps); a server's `applyEdit` after
  a command into the same dialog; `lang.format`; `format_on_save` and `fix_on_save` with
  `explicit` and `always` and the 2 s limit.
- Depends on: stage E11; stages E1b and E4a (the write path, conflicts and the merge view).
- Owned paths: `apps/desktop/src/editor/lang/actions.ts`, `review.tsx` (new),
  `src/editor/buffer.ts` (the save hook), `crates/imperium-core/src/lang/`, `rpc/lang.rs`,
  `tests/lang.rs`, `src/settings/shortcuts.ts`, `apps/desktop/test/langActions.test.ts` (new),
  `apps/desktop/e2e/fixes.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: daemon tests with the fake server: an action whose edit touches the open
  file and two others returns one buffer edit and two review entries with their base hashes; a
  `RenameFile` operation is refused with its reason and the rest kept; 51 files are refused.
  Browser checks: Ctrl+. lists the preferred fix first; applying it changes the buffer, marks the
  tab dirty and writes nothing until Ctrl+S; Ctrl+Z undoes it in one step; the review dialog
  shows three diffs with `.mcp.json` unticked; *Apply* writes the two ticked files with their
  base; a file changed after the dialog opened conflicts and is not written; with
  `format_on_save = explicit` a save formats and then saves, an auto save does not format, and
  with `always` it does; a server that does not answer within 2 s leaves the save unformatted
  with the note; a `fix_on_save` result that also edits another file writes nothing to it (under
  `explicit` the review dialog opens after the save; under `always` the edit is dropped and
  counted in the note); an auto save of a `.mcp.json` buffer with `fix_on_save = always` applies
  the fix to the buffer and stops at the harness confirmation.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E13: The agents' diagnostics tool

- Objective: D24: `diagnostics` in `imperium-tools`, a read tool reading the daemon's
  diagnostic store and `lang.check`, never starting a server; the data framing and trims;
  `harness.diagnostics_tool` and the headless eval variant flag (default off); the tool's
  description and the agent instructions.
- Depends on: stages E9 (the store) and E10a (`lang.check`); independent of the later editor
  stages.
- Owned paths: `crates/imperium-tools/src/diagnostics.rs` (new), `src/lib.rs`, the tool
  registry, `crates/imperium-core/src/lang/`, `src/eval/variant.rs`,
  `crates/imperium-config/src/settings.rs`, `registry.rs`, the tools' tests,
  `docs/03-agent-engine.md`, `docs/05-tools-and-isolation.md`.
- Acceptance checks: `cargo test -p imperium-tools -p imperium-core`: with a fake server running
  for an open file, the tool returns its problems framed as data and trimmed to 1 KiB; for a
  file with no server it returns the built-in checks and says no server runs; it starts no
  server in a trusted project (the fake's sentinel absent); a message holding "ignore previous
  instructions" comes back inside the framing; the tool is available in Plan and Ask modes and
  writes nothing; an eval variant without the flag does not offer it; a `limit` above 200 is
  clamped; with the owning tab dirty, the server's diagnostic is returned with
  `from_unsaved_buffer: true`, and with `disk_only: true` it is left out and `lang.check` of the
  disk text is returned; during a pause (D28) the tool says the servers are paused.
- Size: about 450 lines.
- Status: pending
- Outcome: -

### Stage E14: Themes

- Objective: D25: the theme model, the grown token set with its tag table, the scope table and
  the TextMate-rule resolver, the importer (JSON with comments, the allowed keys, validation,
  the report), the contrast check with *Adjust for contrast* (OKLCH lightness), the shipped
  themes converted and committed with their licences, the app's third-party notices, the
  `editor-themes` store document, Settings → Editor's theme pickers and *Import theme*,
  `prefers-contrast: more`; and semantic tokens from servers as decorations through
  `lang.semantic_tokens`, with `semanticTokenColors` and the fallback table.
- Depends on: stage E3b (D14's tokens); stage E11 for the semantic-token part only (the rest
  may land before it).
- Owned paths: `apps/desktop/src/editor/themes/` (new), `src/editor/lang/semantic.ts` (new),
  `src/settings/EditorSection.tsx`, `src/styles/tokens.css`, `forced-colors.css`, the app's
  third-party notices file, `apps/desktop/test/themeImporter.test.ts` and
  `themeContrast.test.ts` (new), `tokenContrast.test.ts`, `apps/desktop/e2e/themes.spec.ts`
  (new), `crates/imperium-core/src/lang/` (semantic tokens), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`:
  - the resolver picks `entity.name.function.member` over `entity.name.function`, and a later
    rule on a tie, and reports a descendant selector as ignored;
  - the importer accepts a theme with comments and trailing commas; refuses a file of 1 MiB
    plus one byte; drops `"red; background:url(x)"`, `"var(--x)"` and `"#12345"` as colours and
    `"italic; color: red"` as a style, each in the report; ignores `include` and a string
    `tokenColors`; flattens `#ff000080` against the background;
  - every shipped theme passes 4.5:1 (7:1 for the high-contrast themes) for every token and
    diagnostic colour on its background and active line; *Adjust for contrast* makes a failing
    imported theme pass and changes only the failing colours;
  - the semantic fallback colours `variable.readonly` with the theme's
    `variable.other.constant` rule.
  Browser checks: choosing Dracula recolours an open editor without reopening it; *Follow app*
  switches with the app theme; an imported theme survives a reload (the store); a theme with a
  hostile value leaves no inline style outside the `--syntax-*`, `--diag-*` and editor colour
  properties (a scan of the editor root); with the fake server's semantic tokens a parameter
  takes `--syntax-parameter`; a `semanticTokenColors` rule `*.deprecated` colours a deprecated
  function and `*.readonly:rust` only Rust read-only tokens. `npm test` also checks the
  **notices**: every file under `src/editor/themes/` names its licence and source revision, and
  each shipped theme's licence text appears in the app's third-party notices; a shipped theme
  adjusted at conversion keeps its original colours for *Use as designed*.
- Size: about 800 lines (the converted theme files are data and counted apart).
- Status: pending
- Outcome: -

### Stage E15: Language tooling end to end and docs

- Objective: the close-out for revision 4. The Linux end-to-end job installs pinned servers as
  job provisioning (`rustup component add rust-analyzer`, `npm install -g` of pinned
  `typescript`, `typescript-language-server` and `vscode-langservers-extracted`, `pip install` of
  a pinned `ruff`) and runs an `editor-lint` scenario on a fixture project per language:
  untrusted, only built-in syntax errors show and no server starts (a `build.rs` and an
  `eslint.config.js` that would write sentinel files leave none); trusted, a type error appears
  as a problem, a quick fix applies to the buffer and format on save formats; the agents'
  `diagnostics` tool returns the same problem; an agent's edit of the fixture's `build.rs`
  pauses rust-analyzer and nothing runs until *Resume*; rust-analyzer's peak memory on the fixture and on
  this repository is recorded for D21's numbers. Docs: 02 (`lang.*`, `projects.*`), 05 (trust,
  the diagnostics tool, the install floor), 07 (problems, fixes, themes), 10 (settings), 12
  (item 66's text), 14 (trusted projects).
- Depends on: stages E8 to E14, E10c included; stage E7 (the `editor` scenario it extends).
- Owned paths: `scripts/ci/presence-e2e/` (with a `fixtures/lint/` tree),
  `.github/workflows/presence-e2e.yml`, `scripts/ci/detect.sh`, `scripts/ci/test-detect.sh`,
  `docs/**`, `docs/12-roadmap.md`.
- Acceptance checks: the `presence-e2e` Linux job runs `editor-lint` green, the sentinel checks
  included; docs checks green; `cargo test --workspace` and `scripts/check-web.sh` green.
- Size: about 600 lines.
- Status: pending
- Outcome: -

## Risks across stages

- **Item 55 is a prerequisite and is not started.** Every write-path stage waits on its stages 2b,
  3 and 6a; the preview waits on 6b and 7a. If item 55's interfaces change during review, the
  matching stage here follows them.
- **The gate is shared with terminals** and owned by item 55 stage 2b; a change to it runs both
  plans' tests and the presence job.
- **Unsafe code** in `imperium-ipc` (`FileRenameInfoEx`, security descriptors, streams,
  `fcopyfile`): small modules behind safe functions, each with its own tests on its OS.
- **Atomic replace on Windows** (sharing violations, antivirus, older builds): the retries and the
  in-place fallback, tested on the runner.
- **Changing `write_file`'s behaviour** (E4b) can affect evals that write files repeatedly; only
  writes over files changed under the session are refused, and the eval runner's tests guard it.
- **CodeMirror version drift** between item 55's viewer and this editor: one set of exact pins,
  changed together.
- **Protocol minors.** Stages E1a and E1b take the next free minor at their merge; merge
  `experimental`, never rebase. So do E8, E9, E10a, E10b, E10c, E11, E12 and E14.
- **Item 68's plan is merged but not built** (pull request #332). E9 waits for its stage 1;
  the `lsp` class's numbers are proposals until stages E9 and E15 measure them.
- **Servers differ.** Capabilities, configuration and quirks vary (clangd without
  `compile_commands.json`, rust-analyzer's first indexing taking minutes, gopls needing a
  module); the fake server tests the client, and E15 tests three real servers. The other
  catalogue entries are tested for discovery and start only, a named gap.
- **Trust is a new prompt.** Users who open many repositories see the banner often; it shows
  once per project and *Not now* holds for the session. If it is too noisy, a later change may
  offer trusting a folder of the user's own repositories, which this plan rejects for now.
- **Theme licences.** Every shipped theme's licence is checked at conversion and its notice
  kept; a theme whose licence changes is removed.

## Open questions

Each has the default this plan uses; the root confirmed these defaults on 2026-11-26, and the
settled questions are listed [above](#settled-decisions).

1. **Hot exit:** keep unsaved buffers across a restart? Default: no, for secrets at rest; the app
   asks before closing.
2. **Remote images in Markdown:** item 66 said they load after asking. Default here: never loaded
   in the app (the window's policy blocks them, and they are tracking beacons); a placeholder with
   *Open in browser*. Loading them would mean the daemon fetching the image and returning it as a
   `data:` URL after a per-file confirmation.
3. **Speaker notes, a presenter view and exporting slides (PDF)?** Default: not in this item.
4. **Reopen with encoding** for legacy files, editable when the round trip is byte-identical: a
   follow-up item? Default: not in this item (settled: read-only until then).
5. **Mermaid diagrams and math in the preview?** Default: no; Mermaid renders through its own
   script and SVG, math needs KaTeX's markup and fonts.
6. **Rename, delete and new folders in the tree?** Default: *New file* only.

Questions 7 to 13, added in revision 4, were answered on 2026-11-26 and are recorded under
[settled decisions](#settled-decisions). No question is open in revision 5.
