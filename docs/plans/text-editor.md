# Plan: built-in text editor with Markdown presentation

Revision: 2026-11-26, revision 1 (first draft, not yet reviewed); written against `experimental` at
`243602ab`. Roadmap [item 66](../12-roadmap.md#next-in-order). Status: planned (no stage started).

The user asked for "an in-built text editor that can also do markdown presentation". The
**Files** panel of [item 55](terminal-file-panes.md) is a read-only viewer; this plan turns it into
an editor for the person using the app: tabs, unsaved-change markers, find and replace, saves
through the daemon with the same root confinement as item 55's reads, conflict handling when an
agent (or anything else) changes an open file, a live Markdown preview, and a presentation mode
that shows a Markdown file as slides.

The plan's core is two rules:

- **Only the user's editor writes, and only inside the session's folders.** The new write method
  is answered only to the identified desktop app on the local socket, relayed by the app's native
  side (the gate of item 55's terminals, [D2](terminal-file-panes.md#decisions)). An agent's process
  that holds the daemon token cannot use it to step around the permission rules that govern its own
  `write_file` and `edit_file`. Agents get no editor tool and cannot drive the editor.
- **Neither side's edit is lost silently.** Every save carries the hash of the text it was based
  on; the daemon refuses a save whose base no longer matches the file, and the editor shows the
  difference and lets the user merge, keep theirs or take the file's.

Each stage below is one pull request against `experimental` of at most about 800 changed lines,
with the docs it changes. No stage changes the research session's files
(`crates/imperium-engine/src/agent/{tools.rs,turn.rs,compact.rs}`, `eval/record.rs`,
`eval/variant.rs`).

## Goals

Each goal is boolean and has a check that runs in CI, with no manual step. "The fake daemon" and
"browser checks" are item 55's harness ([stage 4](terminal-file-panes.md#stage-4-browser-check-harness):
Playwright in Chromium and WebKit against a Node fake daemon, run by `scripts/check-web.sh` in the
`web` job); "the matrix" is the `rust` job of `ci.yml` on Ubuntu, Windows and macOS.

1. **The user edits and saves a file of the session.** Typing, undo, find and replace, and
   Ctrl+S (Cmd+S) save the file; the bytes on disk are the buffer in the file's own encoding, byte
   order mark and line endings. Check: stage E3a browser checks; stage E1b
   `cargo test -p imperium-core --test files_write` (encoding and line-ending round trips); the
   stage E7 Linux end-to-end run.
2. **Writes stay inside the session's folders.** No spelling, link, junction, race or root swap
   makes `files.write` create, replace or truncate a file outside the session's folders and scratch
   folder, or write into a refused path ([D7](#decisions)). Check: stage E1b tests on the matrix.
3. **Only the desktop app writes.** `files.write` is refused over loopback TCP, to a paired phone,
   to a caller that is not the installed `imperium-desktop`, to anything the daemon started, and to
   a mod frame or Preview page in the app's window. Check: stage E1b gate tests, stage E2
   `tauri::test` access test, stage E7 real frames on Linux.
4. **No edit is lost silently.** A save based on stale text is refused with the current state; an
   agent's change to an open file with no unsaved edits reloads the buffer and says so; with unsaved
   edits it never overwrites the buffer and never lets a save overwrite the agent's change without
   the user choosing to. Check: stage E1b conflict tests; stage E4 browser checks with scripted
   agent edits.
5. **Markdown renders safely.** No raw HTML, script, event handler, `javascript:` or `data:` link,
   remote request or file outside the session's folders is reachable from a rendered Markdown file.
   Check: stage E5 `npm test` and browser checks (no dialog, no request to the fake's sentinel URL).
6. **Presentation mode shows a Markdown file as slides.** Split on top-level `---`, navigable by
   keyboard, full window or full screen, announced to screen readers. Check: stage E6 `npm test`
   (splitter) and browser checks.
7. **The editor is accessible and themed.** Every control works by keyboard, focus can always leave
   the editor, syntax colours meet 4.5:1 on the editor background in both themes and all accents.
   Check: `test/tokenContrast.test.ts` extended in stage E3b; browser checks for focus.
8. **A paired phone gets no editor; mods cannot drive it.** Check: `tests/phone.rs` (`files.write`
   and `files.stat` refused with `-32005`); stage E2 access test; stage E7 frame attempts.
9. **Every check runs in CI.** No manual check, display server outside the job, or network service
   other than package installs.

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
  - **The window's policy.** Monaco needs web workers for its language services, which means a
    `worker-src` (and in practice `blob:`) added to `tauri.conf.json`'s `default-src 'self'`
    policy, and its loader evaluates modules in ways the policy's `script-src` (no `unsafe-eval`)
    resists. CodeMirror runs on the main thread with no workers and no eval.
  - **Webviews.** CodeMirror uses `contenteditable` with native selection and input, so IME,
    spell check and the platform's screen reader work in WebView2, WebKitGTK and WKWebView alike.
    Monaco draws its own text layer over a hidden textarea and needs its accessibility mode for
    screen readers; WebKitGTK is its least tested host.
  - **Virtualised rendering** of long files, which item 55 relies on, is built in to both.

  Rejected: Monaco (above); **a `<textarea>`** (no highlighting, no virtualising, no search);
  **Ace** (older model, global state, no virtualised `contenteditable`). Revisit if the editor
  must offer language-server features (completion, go to definition), which Monaco brings more of;
  that would be a new roadmap item.
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
    ([D2 of item 55](terminal-file-panes.md#decisions)). The check lives in one place
    (`crates/imperium-core/src/app_gate.rs`, the `TerminalGate` trait generalised to `AppGate`);
    whichever of item 55 stage 2b and stage E1b lands first creates it.
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
- **D4. Paths and opening: item 55's rules, for writing.** Paths are `{root, relative}` pairs
  only; roots are `Workspace::roots()` and `Workspace::scratch()` with identities recorded at
  creation; the same up-front spelling checks (no `..` past the root, absolute, drive, UNC,
  `\\?\`, stream or reserved device names); every open through the held root `Dir` with no link
  followed out of the root (`openat2` with `RESOLVE_BENEATH` on Linux, component by component
  elsewhere) ([D12 of item 55](terminal-file-panes.md#decisions)). For a write additionally:
  - the target, if it exists, must be a regular file after `fstat` (a FIFO, socket or device is
    refused without opening it for writing);
  - a link inside the folders that stays inside is followed: the save writes the link's target,
    resolved through the held `Dir`, in the target's directory;
  - creating a file (`create: true`) opens with `O_EXCL` (`create_new`) and creates no
    directories; its name passes the same spelling checks, and a name the root's file system would
    alias (trailing dots or spaces on Windows) is refused.

  Rejected: `Workspace::resolve_writable` (it accepts the loose spellings models write and
  includes path grants, which are agents' permissions, not the user's folders).
- **D5. Atomic replace, keeping what the file was.** A save writes a temporary file in the
  target's own directory through the held `Dir` (`.<name>.imperium-<random>.tmp`, created with
  `O_EXCL`), writes and flushes it (`sync_all`), copies the old file's permission bits (Unix mode;
  on Windows the read-only attribute is a refusal, not something to clear), then renames it over the
  target (`renameat` within the same `Dir`; on Windows `MoveFileExW` with replace-existing through
  `std::fs::rename`). A crash leaves either the old or the new file, never a truncated one; the
  temporary file is removed on any failure, and leftovers older than a day matching the pattern are
  removed when the root is next opened. Exceptions, written in place (truncate and write) instead:
  a file with more than one hard link (renaming would split it from its other names) and a file
  whose rename is refused because another program holds it open without delete sharing (Windows);
  both say so in the save's result note. Rejected: **always in place** (a crash or full disk
  leaves a truncated file); **always rename** (breaks hard links). Ownership, extended attributes
  and ACLs other than the mode are not copied; the docs say so. Revisit if users report lost ACLs.
- **D6. Saves compare identity and content hash; one lock per file in the daemon.**
  - `files.read` (item 55) gains, for text up to the editable cap (D8), `hash`: the SHA-256 of
    the file's whole bytes (`sha2` is in the lock already) and `file_id` (device and inode, or
    volume serial and file index, through `same-file`).
  - `files.write` carries `base: {hash, file_id}` (or `null` with `create: true`). Under the lock
    below the daemon opens the target, checks it is the same file and hashes its bytes; if either
    differs it writes nothing and fails with the new error `FILE_CONFLICT` (the next free code,
    `-32010` today), whose data is the current `{hash, file_id, size, modified_ms}` (or `deleted`).
    `etag` (item 55's size, time and partial hash) is used only to skip re-reads, never to accept
    a save: a same-size edit within the clock's resolution keeps the `etag`.
  - **One lock per file, shared with agents' file tools.** `write_file`, `edit_file` and
    `files.write` all run in the daemon's process. A new `imperium-tools/src/file_locks.rs` keeps an
    async mutex per canonical path (weakly held, so the table does not grow); `write_file` and
    `edit_file` (in `imperium-tools/src/files.rs`, not a research-owned file) take it around their
    read and write, and `files.write` takes it around compare and replace. A user's save and an
    agent's edit of the same file therefore never interleave inside the daemon. A command
    (`run_command`, a build, another editor) can still change the file between the compare and the
    rename; that window is microseconds and is a stated [residual risk](#residual-risks).
  - Rejected: **compare by `etag` only** (above); **advisory OS locks** (`flock` and Windows share
    modes are ignored by most programs and would block agents' tools on Windows).
- **D7. Refused write targets.** Item 66 lists them; a refused file opens read-only with the
  reason shown, and `files.write` refuses it with `-32005` even if the app asks:
  - anything with a `.git` component in the relative path, and any root-relative path inside a
    linked worktree's main `.git` folder (the safety floor's `.git` rule);
  - the daemon's `<data>` and `<config>` folders and everything in them (mods, plugins,
    approvals, `daemon.json`), wherever a root happens to contain them;
  - the files that hold secrets in the safety floor's list (`.env`, `.env.*`, `*.pem`, `*.key`,
    `id_rsa*`, `id_ed25519*`, `id_ecdsa*`, `.npmrc`, `.pypirc`, `.netrc`), shared with the floor's
    list in `imperium-policy` so the two cannot drift ([open question 1](#open-questions));
  - harness folders, taken to mean the project's agent-configuration folders `.imperium/` and
    `.claude/` (hooks, skills, commands, agents, MCP and environment config;
    [open question 2](#open-questions)).

  Agent deny rules do not apply: they say what agents may touch, and the editor is the user's, as
  item 55's reads are.
- **D8. Encodings, line endings and size.**
  - **Editable encodings:** UTF-8 with or without a byte order mark, and UTF-16 LE or BE with a
    byte order mark (`encoding_rs` is in the lock already). A file is editable only when it decodes
    without loss: invalid UTF-8, BOM-less UTF-16 and legacy encodings open read-only with a note
    ([open question 7](#open-questions)). The save encodes the text back with the same encoding
    and BOM; the daemon, not the webview, encodes, and `files.write` takes `encoding` and `bom`.
  - **Line endings:** `files.read` reports `eol` (`lf`, `crlf`, `mixed` with the majority, or
    `none`). The buffer uses `\n` internally (CodeMirror's default) and the save joins lines with the
    file's ending (`doc.sliceString(0, length, eol)`), so untouched lines keep their bytes. A
    `mixed` file is saved with its majority ending; the status bar says "Mixed line endings: saving
    as CRLF" before the first save, and the ending can be changed there. A lone CR is treated as
    `mixed`. The final newline is kept as it was; nothing is trimmed or inserted on save.
  - **Size:** editable up to 4 MiB (item 55's load cap), read in 1 MiB pages as item 55 does and
    saved in one call; highlighting off above 1 MiB or for a line over 10,000 characters (item 55);
    the Markdown preview off above 1 MiB. Larger files open in item 55's capped read-only view and
    say why. Binary and image files are never editable.
- **D9. Agent edits and user edits: reload clean buffers, never overwrite dirty ones.**
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
  - **An agent that overwrites a file the user just saved** (its `write_file` built from text it
    read earlier) is a clean-buffer reload as above, and the notice offers *Restore my save*: the
    editor keeps the user's last saved text in memory per tab and re-saves it on request, with the
    agent's text as the base. Agents are not told about user saves in this item
    ([open question 3](#open-questions)); they read the file from disk, so their next read sees
    the user's text.
  - **The Review panel** shows the working tree against the checkpoint, so the user's saves appear
    there as changes in the session, not attributed to the agent; checkpoints taken before the
    next turn capture them, so a rewind undoes them too
    ([14](../14-projects-and-automation.md#checkpoints-and-rewinding)). The docs say so.
  - **Deleted or renamed on disk:** the tab says the file is gone and keeps the buffer;
    *Save* recreates it (a create, `O_EXCL`), *Close* discards.
  - Rejected: **locking a file while it is open in the editor** (agents would fail mid-turn for a
    tab left open); **always reloading** (loses unsaved work); **last writer wins** (the item's
    requirement).
- **D10. Buffers live in memory only.** Unsaved text is never written to `localStorage`,
  IndexedDB or the app store (secrets, and the same rule as item 55's terminals). The list of open
  tabs per session (`{root, path, line, column}`, no content) is kept in a new `editor` document of
  the app store so tabs reopen after a restart. Closing a dirty tab, switching the Files panel's
  session (tabs belong to their session and are kept while the app runs), detaching or re-attaching
  the Files panel, reloading the window and closing the app each ask *Save*, *Don't save* or
  *Cancel*, listing the dirty files. Rejected: hot exit (drafts restored after a restart;
  [open question 4](#open-questions)); carrying buffers across a detach (a new webview; asking is
  simpler and rare).
- **D11. Markdown preview: React elements, never HTML strings.**
  - The preview renders with `react-markdown` 10.1.0 and `remark-gfm` 4.0.1, already in the app
    for the transcript; this plan pins both exactly (today `^10.1.0` and `^4.0.1`). No `rehype-raw`:
    raw HTML in the file is shown as escaped text (react-markdown's default), so `<script>`,
    `<iframe>`, `<img onerror>` and `<style>` never become elements. An `allowedElements` list
    (headings, paragraphs, emphasis, lists, task-list checkboxes, tables, blockquotes, code, `hr`,
    `a`, `img`, `br`, `del`) is a second net.
  - **Links** (`components.a`): rendered as a button-styled anchor with no `href` navigation.
    `#anchor` scrolls the preview; a relative path to a file inside the session's folders opens it
    in the editor (through `files.locate`); `http`, `https` and `mailto` open only after a
    confirmation that shows the full address, the host in punycode and the visible text when it
    differs, through the one link function item 55 builds (`src/terminal/policy.ts`, moved to
    `src/shared/links.ts` by whichever stage needs it second), then `openExternal`. Everything else
    (`javascript:`, `data:`, `file:` outside the folders, `vbscript:`, unknown schemes) is shown as
    text with a tooltip saying why it does not open.
  - **Images** (`components.img`): only a relative path inside the same root as the Markdown file,
    resolved against the file's folder and normalised in a pure function (a path that climbs out of
    the root is refused before any call), fetched with `files.read`, and shown only when the daemon
    found image magic bytes (item 55's rule, up to 10 MiB) as a `data:` URL (`img-src 'self' data:`
    already allows it). SVG is not shown as an image (it can carry script and remote references); it
    shows as a placeholder. A remote image is **never loaded**: the window's policy blocks remote
    images anyway, and a remote image in a file the agent wrote is a tracking beacon. It shows as a
    placeholder with its alt text and host and *Open in browser* behind the link confirmation.
    This is stricter than item 66's "remote images load only after asking"
    ([open question 5](#open-questions)); the roadmap text is updated to match.
  - **Code blocks** are highlighted with `@lezer/highlight`'s `highlightCode` into React spans
    (languages from `@codemirror/language-data`), never `innerHTML`.
  - **Updates and scroll:** the preview re-renders 150 ms after typing stops; each block carries
    its source line (`node.position`), so the preview follows the editor's scroll and a click on a
    preview block moves the cursor there.
  - Not supported: Mermaid, math, embedded HTML, footnote tooltips. Mermaid runs its own renderer
    and math needs KaTeX's markup; both are [open question 8](#open-questions).
  - Rejected: **`marked` or `markdown-it` with DOMPurify** (HTML strings into `innerHTML`, one
    sanitiser between a file and the DOM); **`rehype-sanitize` with `rehype-raw`** (lets some HTML
    through, to no benefit in a local editor).
- **D12. Presentation mode.**
  - **Splitting** (`src/editor/slides.ts`, pure, tested by `npm test`): the file is parsed with the
    same `remark` pipeline and split at **top-level thematic breaks** (`---`, `***`, `___`), using
    node positions to slice the source, so a `---` inside a code fence, a list or a blockquote and a
    setext heading's underline (`Title` over `---`) do not split. A leading YAML front-matter block
    (`---` on line 1 to the next `---` or `...` within the first 100 lines) is dropped. A file with
    no top-level break splits before each level 1 and level 2 heading (item 66's "or headings"). An
    empty slide is dropped.
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
  - Not in this item: speaker notes, a presenter view on a second screen, export to PDF or PPTX,
    slide transitions ([open question 6](#open-questions)).
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
    (`Mod+Shift+S`; not `Mod+Alt+S`, which is AltGr on many layouts), **Close tab** (`Mod+W`, only while the editor has focus; Tauri's default
    `CloseRequested` is not bound to it), **Toggle Markdown preview** (`Mod+Shift+V`),
    **Start presentation** (`F5`), **Leave editor** (`F6`).
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
  focus change; default off). Auto save still goes through the conflict rules: a conflict stops it
  and shows the banner. Daemon-side, user layer only (in the registry, not on the environment
  layer's allow-list): `files.write_enabled` (bool, default `true`): off refuses `files.write` and
  the editor opens everything read-only.
- **D16. Built-in, closed to mods and the phone.** The Files panel stays a built-in panel that
  takes no modules ([D15 of item 55](terminal-file-panes.md#decisions)); no mod permission reaches
  buffers or `files_write`. A paired phone is refused `files.write` and `files.stat` as it is every
  `files.*` method ([D8 of item 55](terminal-file-panes.md#decisions)).
- **D17. Testing.** Item 55's four layers: Rust tests on the matrix for the daemon's write path and
  gate; `tauri::test` for the relay's access rules; Playwright browser checks against the fake
  daemon for the editor, conflicts, preview and presentation (the IPC is replaced there, so access
  rules are not checked in that layer); and one real-app scenario on Linux in `presence-e2e`.
  Rejected: manual checks of any kind.

## Current state (found on `experimental` at `243602ab`)

- **Item 55 is planned, not started.** There is no `files.*` method, no `crates/imperium-core/src/files.rs`,
  no `cap-std`, no CodeMirror in `apps/desktop/package.json`, no `apps/desktop/e2e/` harness and no
  native relay. This plan builds on its stages 2b (the gate), 3 (the relay pattern), 4 (browser
  checks), 6a and 6b (roots, reads, listing, locate) and 7a and 7b (viewer and tree).
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
  with `tokio::fs::write` (truncate and write), with no lock and no hash.
- **Crates already in the lock:** `sha2`, `same-file`, `encoding_rs`, `similar`, `tempfile`; no
  `notify`.
- **Protocol** is 0.62; error codes run to `-32009` (`DAEMON_STOPPING`).

## Scope and non-goals

In scope: editing, saving and creating files in the Files panel; conflict handling with agents
and other programs; the Markdown preview; presentation mode; settings, shortcuts, themes; the
daemon's write path and gate; the native relay; browser checks; the Linux end-to-end scenario; the
docs (02, 05, 06, 07, 10, 12, 15).

Not in scope:

- Agents driving the editor in any way; any agent tool for it.
- Rename, move, delete and folder creation (the tree's *New file* is the only structural action);
  "Save as" outside the file's folder.
- Language servers, completion, diagnostics, formatting on save, linting.
- Remote runtimes (container, WSL, SSH), a remote daemon, the phone, the CLI.
- Persisting unsaved buffers; a file-system watcher; collaborative editing.
- Speaker notes, presenter view and exporting slides.

## Threat model

Assets: the files of the session's folders and anything the user can write; secrets in those
files; the user's unsaved work.

Untrusted parties and inputs: models and their tool calls; agents' processes that can read the
daemon token at tier 0; mods and their frames; Preview pages; repository content (file names,
Markdown with HTML, links and images, files agents wrote); a paired phone.

1. **A write that bypasses agents' permissions.** An agent's command calls `files.write` with the
   token: refused by the gate (D3), tested with a real child process carrying the spawn mark, and
   over loopback TCP.
2. **A mod frame or Preview page invoking the write.** Refused by Tauri's origin-based access
   control (D3), tested by `tauri::test` and by real frames on Linux; refused while a main-context
   mod is or was loaded.
3. **Writes outside the folders.** Spellings, links, junctions, a root swapped for a link, a link
   swapped in during a save, special files, other sessions' worktrees (D4), refused paths (D7).
   The temporary file is created inside the target's directory through the held `Dir`, so it
   cannot be redirected either.
4. **Hostile Markdown** (D11): raw HTML shown as text, an element allowlist, no `href`
   navigation, scheme allowlist with confirmation, no remote images, SVG not rendered, local images
   by magic bytes only and only inside the file's root, code highlighted into React spans.
5. **Lost work** (D9, D10): stale saves refused by hash; dirty buffers never overwritten; the
   per-file lock between user saves and agents' tools; leaving with unsaved edits asks.
6. **Secrets at rest** (D10): buffers in memory only; tab lists hold paths, not content; secret
   files are not writable (D7).
7. **Resource exhaustion:** saves at most 4 MiB of text; `files.stat` at most 50 files per call;
   the poll is one visible tab every 2 s; the preview is debounced and off above 1 MiB.

### Residual risks

- **Commands racing a save.** A program outside the daemon (an agent's `run_command`, a build, the
  user's other editor) that writes between the hash check and the rename loses its write to the
  user's save, or the reverse. The window is the time to hash and rename; the lock covers every
  writer inside the daemon.
- **An evading same-user process** can drive the app's window or detach from the daemon to pass
  `may_grant` (item 55's residual risk, unchanged).
- **Metadata on atomic replace:** ownership, extended attributes and ACLs other than the mode are
  not carried to the new file (D5).

## Protocol additions

In `crates/imperium-protocol/src/methods/files.rs` (created by item 55 stage 6a). Each stage that
adds methods or fields takes **the next protocol minor at its merge** (0.62 today; other pull
requests may take the next ones first) and updates `PROTOCOL_VERSION`,
[02](../02-architecture.md#protocol), `snapshots/methods.txt` and the generated types.

| Method, field or error | Stage | Does |
| --- | --- | --- |
| `FileContent` gains `hash?`, `file_id?`, `bom?`, `eol?`, `writable`, `write_refusal?` | E1a | the SHA-256 of the whole file and its identity (text up to 4 MiB), the BOM and line endings found, and whether `files.write` would accept it, with the reason when not |
| `files.stat` | E1a | `{session_id, files: [{root, path}]}` (at most 50) → `{files: [{root, path, exists, size?, modified_ms?, file_id?, hash?}]}`; `hash` only when `with_hash` and the file is text up to 4 MiB |
| `files.write` | E1b | `{session_id, root, path, base: {hash, file_id} \| null, create?, text, encoding (utf8, utf16le, utf16be), bom}` → `{hash, file_id, size, modified_ms, note?}`; desktop app only (D3) |
| `FILE_CONFLICT` (`-32010`, or the next free code) | E1b | `files.write`'s base no longer matches; `data` is the current `{hash, file_id, size, modified_ms}` or `{deleted: true}` |

`files.write` is answered only to the identified desktop app (D3), otherwise `-32005` with the
reason; `files.stat` is answered to any full client, like the other reads, and refused to a phone.
Neither is offered to agents. Native (Tauri) command: `files_write`, the same arguments, relayed
(stage E2). Config: `files.write_enabled` (D15).

Not added: a `files.changed` notification (the editor polls, D9), `files.delete`, `files.rename`.

## Assumptions

- Item 55 stages 2b, 3, 4, 6a, 6b, 7a and 7b land first, with the interfaces their plan names
  (`AppGate` or `TerminalGate`, the relay pattern, the fake daemon's scriptable `files.*`,
  `openAt`, the tree). If one changes, the dependent stage here follows it.
- `cap-std`'s `Dir::rename` and `open_with` (`create_new`, `write`) refuse escapes the same way its
  `open` does, on all three OSes; stage E1b tests that rather than trusting it.
- `std::fs::rename` replaces an existing file on Windows (`MoveFileExW` with
  `MOVEFILE_REPLACE_EXISTING`) and fails when another process holds it without delete sharing; stage
  E1b tests the in-place fallback on the Windows runner.
- `@codemirror/merge` 6.12.3 exports `diff` usable without a merge view, for minimal reload changes
  and the three-way merge; stage E4 confirms.
- Tauri 2's `setFullscreen` works from a panel window as well as `main`; stage E6 confirms.

## Relevant paths

`crates/imperium-core/src/files.rs` and `src/files/write.rs` (new), `src/app_gate.rs`,
`src/rpc/files.rs`, `crates/imperium-core/tests/files_write.rs` (new) and `tests/phone.rs`,
`crates/imperium-tools/src/file_locks.rs` (new) and `src/files.rs`, `crates/imperium-policy`
(the shared secret-file list), `crates/imperium-protocol/src/methods/files.rs` and
`src/jsonrpc.rs`, `crates/imperium-config/src/settings.rs` and `registry.rs`,
`apps/desktop/src-tauri/src/file_write.rs` (new), `main.rs`, `build.rs`,
`capabilities/default.json`, `apps/desktop/src/editor/` (new: `buffer.ts`, `tabs.ts`,
`conflicts.ts`, `merge3.ts`, `encoding.ts`, `preview/`, `slides.ts`, `present/`),
`src/panels/FilesPanel.tsx`, `src/files/`, `src/shared/links.ts`, `App.tsx`,
`src/settings/settings.ts`, `shortcuts.ts` and a new `EditorSection.tsx`,
`src/styles/tokens.css`, `panels.css`, `forced-colors.css` and a new `editor.css`,
`apps/desktop/test/`, `apps/desktop/e2e/`, `scripts/ci/presence-e2e/`, `scripts/ci/detect.sh`.

## Stages

Each stage is one pull request against `experimental`, with the docs it changes. Sizes are
estimates of changed lines, tests included; a stage that grows past about 800 lines splits.

### Stage E1a: Hashes, stat and the shared file lock

- Objective: `FileContent` gains `hash`, `file_id`, `bom`, `eol`, `writable` and `write_refusal`
  (D6, D7, D8); `files.stat` (protocol minor at merge); `imperium-tools/src/file_locks.rs`, taken
  by `write_file` and `edit_file` around their read and write (their behaviour is otherwise
  unchanged); the refused-path rules as a pure function shared with stage E1b, reading the secret
  list from `imperium-policy` (moved there from the floor so both use one list).
- Depends on: item 55 stage 6a.
- Owned paths: `crates/imperium-core/src/files.rs`, `rpc/files.rs`, `tests/files.rs`,
  `tests/phone.rs`, `crates/imperium-tools/src/file_locks.rs` (new), `src/files.rs`, `src/lib.rs`,
  `crates/imperium-policy` (the secret list), `crates/imperium-protocol/src/methods/files.rs`,
  snapshots and generated types, `docs/02-architecture.md`.
- Acceptance checks: `cargo test -p imperium-core --test files --test phone` and
  `cargo test -p imperium-tools` on the matrix: `hash` equals the SHA-256 of the bytes on disk;
  `eol` is `lf`, `crlf`, `mixed` (with the majority) and `none` for the four fixtures, and a lone CR
  is `mixed`; UTF-8 with BOM, UTF-16 LE and BE with BOM report their encoding and `bom`, invalid
  UTF-8 and BOM-less UTF-16 report `writable: false` with the reason; `.env`, `.git/config`,
  `.imperium/config.toml`, a file inside `<data>` placed under a root, and a 5 MiB file report
  `writable: false` with the reason; `files.stat` of 51 files is refused, of 50 answers, of a
  removed file says `exists: false`; **lock:** two `edit_file` calls on one file from two tasks,
  each appending, both land (no lost update), and the lock table is empty after both finish; the
  phone is refused `files.stat`; the floor's existing tests still pass with the moved list.
- Size: about 600 lines.
- Status: pending
- Outcome: -

### Stage E1b: `files.write` and the gate

- Objective: `files.write` (protocol minor at merge) under D3 to D7: the `AppGate` check (created
  here if item 55 stage 2b has not landed, else reused), the up-front spelling checks, the held
  `Dir`, regular files only, `create` with `O_EXCL`, compare under the lock, atomic replace with the
  mode kept, the hard-link and Windows-sharing in-place fallbacks, leftover temporary files
  cleaned on root open, encoding by `encoding` and `bom`, `FILE_CONFLICT`, refused paths,
  `files.write_enabled`.
- Depends on: stage E1a; item 55 stage 2b's gate if it lands first.
- Owned paths: `crates/imperium-core/src/files/write.rs` (new), `src/app_gate.rs`,
  `rpc/files.rs`, `rpc.rs` (dispatch), `tests/files_write.rs` (new), `tests/phone.rs`,
  `crates/imperium-protocol/src/methods/files.rs`, `src/jsonrpc.rs` (the error code), snapshots
  and generated types, `crates/imperium-config/src/settings.rs` and `registry.rs`,
  `docs/02-architecture.md`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-core --test files_write --test phone` on the matrix,
  with the gate stubbed in-process and real in one test:
  - a save with the right base replaces the file; the bytes equal the text in the requested
    encoding, BOM and line endings, for each of UTF-8, UTF-8 with BOM, UTF-16 LE and BE;
  - a save with a stale `hash`, or a stale `file_id` (the file replaced by another with the same
    content), returns `FILE_CONFLICT` with the current state and leaves the file unchanged; a save of
    a deleted file returns `deleted`;
  - **escape attempts**, each refused with no file created or changed outside: `../`, absolute,
    drive, UNC, `\\?\`, a stream name, `NUL.txt` and the other device names, a symbolic link (or
    junction on Windows) to outside as the target and as a parent, a `create` through a link, and a
    **race** that swaps a parent for a link to outside in a loop while saves run;
  - a link inside to inside writes its target; a FIFO target is refused without blocking (Unix);
    a file with two hard links is written in place and both names see the new text; the mode
    `0640` survives a save (Unix); a read-only file is refused (Windows attribute, Unix mode);
  - a crash simulated after the temporary write and before the rename leaves the original intact,
    and the leftover is removed when the root is next opened;
  - on Windows a target held open without delete sharing is written in place with a `note`;
  - every D7 path is refused with `-32005` and the reason, even with a correct base;
  - **the gate:** a call over loopback TCP, a call from a real child process carrying
    `IMPERIUM_SPAWNED_BY_DAEMON`, and a call from a stub identity that is not the installed app are
    refused with `-32005`; the phone is refused; with `files.write_enabled = false` every write is
    refused;
  - **agents and the user:** an `edit_file` and a `files.write` of the same file started together
    serialise (the later one sees the earlier one's result: the save conflicts or the edit applies
    to the saved text), never interleave.
- Size: about 800 lines.
- Risks: `cap-std`'s rename on Windows across a reparse point (tested); antivirus holding a new
  temporary file on the Windows runner (the in-place fallback covers it, with a note).
- Status: pending
- Outcome: -

### Stage E2: The native relay

- Objective: `src-tauri/src/file_write.rs`: one Tauri command, `files_write`, relayed on the
  app's own local-socket connection (no lease), granted to `main` and `panel-*` for the app's own
  origin only; refused while a trusted main-context mod is or was loaded (item 55's latch, shared,
  not duplicated); refused when the window is connected to a different daemon than `daemon.json`
  names (item 55 threat 11). `src/files/bridge.ts` on the webview side is the only caller.
- Depends on: stage E1b; item 55 stage 3 (the relay pattern and the latch).
- Owned paths: `apps/desktop/src-tauri/src/file_write.rs` (new), `main.rs`, `build.rs`,
  `capabilities/default.json`, `apps/desktop/src/files/bridge.ts`,
  `apps/desktop/test/filesBridge.test.ts` (new), `docs/06-extensibility.md`.
- Acceptance checks: `cargo test -p imperium-desktop file_write` on the matrix: the `tauri::test`
  access test refuses `files_write` from a forged mod-frame origin, a `localhost` Preview origin
  and a remote URL, and accepts the app's own origin; with the latch set every call is refused;
  with a different daemon URL the call is refused. `npm test`: a source scan fails if any file
  other than `src/files/bridge.ts` names `files_write`.
- Size: about 450 lines.
- Risks: changing `main.rs` and `capabilities/` runs the `presence-e2e` job (expected).
- Status: pending
- Outcome: -

### Stage E3a: Editable buffer and save

- Objective: item 55's viewer becomes editable when `writable` is true (D1, D8, D13): the
  editing extensions (history, default keymap with `indentWithTab`, search with replace,
  bracket matching, indentation detection), the read-only state with the reason when not,
  the status bar (encoding, BOM, line endings with the mixed notice and a picker, cursor
  position), Save through `bridge.ts` with the base hash, the dirty marker, *Leave editor* (F6), and
  the **Save** shortcut. `FILE_CONFLICT` here shows a plain "changed on disk" notice with *Reload*
  (stage E4 replaces it). The fake daemon gains `files.write` (through the IPC mock), `files.stat`
  and an on-disk fixture it can change.
- Depends on: stage E2; item 55 stages 4 and 7a.
- Owned paths: `apps/desktop/src/editor/buffer.ts`, `encoding.ts` (new),
  `src/panels/FilesPanel.tsx`, `src/files/`, `src/settings/shortcuts.ts`, `src/styles/editor.css`
  (new), `apps/desktop/package.json` and `pnpm-lock.yaml` (`@codemirror/commands`,
  `@codemirror/search` pinned per D1), `apps/desktop/test/editorBuffer.test.ts` (new),
  `apps/desktop/e2e/editor.spec.ts` (new), `apps/desktop/e2e/` fake daemon, `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test` and `npm run typecheck`: the save text for `lf`, `crlf` and
  `mixed` files (untouched lines keep their endings in `lf` and `crlf` files; `mixed` saves with the
  majority); the final newline kept; the base sent is the loaded hash. Browser checks against the
  fake (Chromium and WebKit): open, type, Ctrl+S, and the fake's file holds the text; the dirty dot
  appears on typing and goes on save; undo after save makes the tab dirty again; find and replace
  replaces every match; a `.env` fixture opens read-only with the reason and typing changes
  nothing; a 5 MiB file opens in the capped read-only view; F6 moves focus out of the editor and
  Esc then Tab leaves it.
- Size: about 750 lines.
- Status: pending
- Outcome: -

### Stage E3b: Tabs, settings and themes

- Objective: tabs per session (open, switch, close, reorder by keyboard, at most 20, with the
  oldest clean tab closed first past that), *Save all*, *Close tab*, the leave-with-unsaved
  dialogs (tab close, session switch, detach and re-attach, window reload, app close; D10), the
  `editor` app-store document of open tabs (paths and positions only), the tree's *New file*
  (`create: true`), Settings → Editor (D15), the `--syntax-*` and editor tokens for both themes
  (D14) with the contrast test, and forced-colours rules.
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
  reopening it; *New file* creates the file and refuses an existing name.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E4: External changes and conflicts

- Objective: D9 in full: change detection (tool results, focus, visibility, the 2-second poll
  of the visible tab through `files.stat`), clean-buffer reloads as minimal changes with the
  notice and *Show changes*, the conflict banner with *Compare* (`@codemirror/merge`), *Merge*
  (three-way, `src/editor/merge3.ts`, pure), *Use file* and *Keep mine*, `FILE_CONFLICT` on save,
  *Restore my save*, and the deleted-on-disk state.
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
  - a save while the fake's file changed underneath returns the conflict and writes nothing;
  - an external change with no tool result (the fake changes the file) is found by the poll within
    3 seconds while the window has focus, and not polled while it does not;
  - *Restore my save* after an agent's full-file write re-saves the user's text;
  - a deleted file keeps the buffer and *Save* recreates it.
- Size: about 800 lines.
- Risks: the three-way merge on very long files (it runs on line hashes; capped by the 4 MiB
  editable limit).
- Status: pending
- Outcome: -

### Stage E5: Markdown preview

- Objective: D11: the preview beside the source for `.md` and `.markdown` (and *Toggle Markdown
  preview*), `react-markdown` and `remark-gfm` pinned exactly, the element allowlist, the link and
  image components, the shared link function in `src/shared/links.ts` (moved from item 55's
  `policy.ts` if that has landed), the confirmation dialog, image resolution through `files.read`,
  code highlighting into React spans, debounced rendering, scroll sync both ways, off above 1 MiB.
- Depends on: stage E3a; item 55 stage 6b (`files.locate`).
- Owned paths: `apps/desktop/src/editor/preview/` (new), `src/shared/links.ts` (new or moved),
  `src/terminal/policy.ts` (only to import the moved function), `src/panels/FilesPanel.tsx`,
  `src/styles/editor.css`, `apps/desktop/package.json` and `pnpm-lock.yaml` (exact pins,
  `@lezer/highlight`), `apps/desktop/test/markdownPreview.test.ts` and `links.test.ts` (new),
  `apps/desktop/e2e/markdown.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test`: the image path resolver (`./a.png`, `../img/a.png` inside the
  root, `../../x.png` climbing out refused before any call, an absolute path refused, a `%2e%2e`
  spelling refused); the link classifier for `#anchor`, relative in and out of the folders,
  `http`, `https`, `mailto`, `javascript:`, `JaVaScRiPt:` with whitespace and entities, `data:`,
  `vbscript:`, `file:`, an IDN host shown as punycode. Browser checks against the fake with a
  hostile fixture: `<script>`, `<img src=x onerror>`, `<iframe>`, `<style>`, an `<a href=
  "javascript:">` in raw HTML and a Markdown `[x](javascript:...)` all appear as text or inert,
  with no dialog and no request to the fake's sentinel URL; a remote image makes no request and
  shows its placeholder; a local PNG shows; a `.txt` renamed `.png` does not; an SVG shows a
  placeholder; an `https` link opens only after the confirmation and calls the opener with the
  exact address; a relative `.md` link opens that file in a tab; typing updates the preview; a
  click in the preview moves the cursor to that line.
- Size: about 750 lines.
- Status: pending
- Outcome: -

### Stage E6: Presentation mode

- Objective: D12: `src/editor/slides.ts`, the overlay, keys, the live region, *Full screen*
  through Tauri (the permission added to `capabilities/default.json`), the presentation theme,
  reduced motion, the **Start presentation** shortcut and palette command.
- Depends on: stage E5.
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
  hostile slide (the E5 fixture) is as inert as the preview; editing the file in the source while
  presenting keeps the current slide; with reduced motion there is no transition. The `tauri::test`
  access test (stage E2) adds that the full-screen permission is granted to the app's own origin
  only.
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
  loopback TCP: every attempt fails and the file is unchanged. `scripts/ci/detect.sh` adds the
  editor and relay paths to the job's watched list. Docs: 02 (methods, error code), 05 (the user's
  editor is outside permission rules; the refused paths; agents' tools take the file lock), 06 (mods
  get no editor), 07 (the editor, preview, presentation, shortcuts, settings), 10
  (`files.write_enabled`), 15 (the phone gets no editor), 12 (item 66 implemented); this plan folded
  into the lasting docs.
- Depends on: stages E4, E5 and E6; item 55 stage 8 (its `panes` scenario, which this extends).
- Owned paths: `scripts/ci/presence-e2e/`, `scripts/ci/detect.sh`, `scripts/ci/test-detect.sh`,
  `docs/**`, `docs/12-roadmap.md`.
- Acceptance checks: the `presence-e2e` Linux job runs the `editor` scenario green, attempts
  included; docs checks (`python3 scripts/ci/check-links.py .` and markdownlint) green;
  `cargo test --workspace` and `scripts/check-web.sh` green.
- Size: about 450 lines.
- Status: pending
- Outcome: -

Order: E1a, E1b, E2, E3a, E3b, E4, E5, E6, E7. E5 may run in parallel with E3b and E4 once E3a
has landed; E1a may start as soon as item 55 stage 6a has.

## Risks across stages

- **Item 55 is a prerequisite and is not started.** Every stage here waits on one of its stages;
  if item 55's interfaces change during review, the matching stage here follows them.
- **The gate is shared with terminals.** A change to `AppGate` affects both; the presence job runs
  on such changes.
- **Atomic replace on Windows** (sharing violations, antivirus, `MoveFileExW` semantics): the
  in-place fallback and a test on the Windows runner.
- **CodeMirror version drift** between item 55's viewer and this editor: one set of exact pins,
  changed together.
- **Protocol minors.** Stages E1a and E1b take the next free minor at their merge; merge
  `experimental`, never rebase.

## Open questions

These need the user; each has the default this plan uses until answered.

1. **Should the user be able to edit secret files (`.env`, keys) in the editor?** Item 66 refuses
   them, and so does this plan: they open read-only, saying to edit them in another editor. The
   user's own editor is not an agent; allowing them behind a per-save confirmation is the
   alternative.
2. **What are "harness folders", and should they be editable?** Default: `.imperium/` and
   `.claude/` in the project (agent hooks, skills, commands, agents, MCP and environment config),
   read-only. They change what agents run, which is why item 66 lists them; the alternative is
   editable with a confirmation naming the risk.
3. **Should agents be told when the user saved a file they read earlier** (a note at the next turn,
   or a full-file `write_file` that asks when the file changed since the agent read it)? Default:
   no; it needs changes in the research session's engine files. The editor shows an agent's
   overwrite and offers *Restore my save*.
4. **Hot exit:** keep unsaved buffers across a restart? Default: no, for secrets at rest; the app
   asks before closing.
5. **Remote images in Markdown:** item 66 says they load after asking. Default here: never loaded
   in the app (the window's policy blocks them, and they are tracking beacons); a placeholder with
   *Open in browser*. Loading them would mean the daemon fetching the image and returning it as a
   `data:` URL after a per-file confirmation.
6. **Speaker notes, a presenter view and exporting slides (PDF)?** Default: not in this item.
7. **Legacy encodings** (Latin-1, Windows-1252, Shift-JIS, BOM-less UTF-16): editable through a
   "reopen with encoding" choice? Default: read-only.
8. **Mermaid diagrams and math in the preview?** Default: no; Mermaid renders through its own
   script and SVG, math needs KaTeX's markup and fonts.
9. **Rename, delete and new folders in the tree?** Default: *New file* only.
