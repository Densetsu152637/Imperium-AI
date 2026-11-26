# Plan: built-in text editor with Markdown presentation

Revision: 2026-11-26, revision 2 (plan review of revision 1: B1, B2, M1 to M5, the minor findings
and nits folded in, and the root's answers to open questions 1, 2, 3 and 7 recorded as
[settled decisions](#settled-decisions)); written against `experimental` at `243602ab`. Roadmap
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
  `write_file` is refused in turn when the file changed since that session last read it.

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
  this session's last `read_file` of it** (a per-session last-read hash), and the next turn may
  carry a note listing the files the user saved ([D10](#decisions), stage E4b).
- **Legacy encodings** stay read-only; a later "reopen with encoding" is editable only for a file
  whose decode and encode round trip is byte-identical ([D8](#decisions)).
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
- **D4. Paths and opening: item 55's rules, for writing.** Paths are `{root, relative}` pairs
  only; roots are `Workspace::roots()` and `Workspace::scratch()` with identities recorded at
  creation; the same up-front spelling checks (no `..` past the root, absolute, drive, UNC,
  `\\?\`, stream or reserved device names); every open through the held root `Dir` with no link
  followed out of the root (`openat2` with `RESOLVE_BENEATH` on Linux, component by component
  elsewhere) ([D12 of item 55](terminal-file-panes.md#decisions)). For a write additionally:
  - the target, if it exists, is opened first (no write access yet), `fstat`ed, and must be a
    regular file with **exactly one link** (`nlink == 1`); a FIFO, socket or device is refused
    without opening it for writing, and a file with other hard links opens read-only with the
    reason shown (D7);
  - a link inside the folders that stays inside is followed: the save writes the link's target,
    whose parent directory is opened through the held `Dir` and kept open for the rest of the save;
  - creating a file (`create: true`) opens with `O_EXCL` (`create_new`) and creates no
    directories; its name passes the same spelling checks;
  - on Windows, every path component on **every** operation (read, stat, write, create, the
    temporary file and the rename) is refused when it ends in a dot or a space or is a short (8.3)
    name that differs from the component's long name; the opened handle's final path (D7) is the
    check of record, so a spelling that slipped past these is still judged by what it opened.

  Rejected: `Workspace::resolve_writable` (it accepts the loose spellings models write and
  includes path grants, which are agents' permissions, not the user's folders).
- **D5. Atomic replace, keeping what the file was.**
  - **The sequence.** Under the file's lock (D6), with the parent directory held open: create a
    temporary file in that directory through the held handle (`.<name>.imperium-<random>.tmp`,
    `O_EXCL`); write and `sync_all` it; copy the old file's metadata onto it, handle to handle
    (below); rename it over the target **relative to the held parent handle**; on Unix `fsync` the
    parent directory. A crash leaves either the old or the new file, never a truncated one.
  - **Handle-relative rename.** Unix: `renameat` through the held `Dir` (cap-std's `Dir::rename`),
    so a parent swapped for a link after it was opened cannot redirect the rename. Windows:
    `SetFileInformationByHandle` with `FileRenameInfoEx` on the temporary file's handle,
    `RootDirectory` set to the held parent handle, flags `FILE_RENAME_FLAG_REPLACE_IF_EXISTS` and
    `FILE_RENAME_FLAG_POSIX_SEMANTICS`, in a new opt-in unsafe module
    `crates/imperium-ipc/src/windows/replace.rs` (the workspace allows `unsafe` only there).
  - **Metadata copied** from the old file's handle to the temporary file's handle before the
    rename:
    - Linux: the mode (`fchmod`) and every extended attribute (`rustix` `flistxattr`,
      `fgetxattr`, `fsetxattr`), which carries POSIX ACLs (`system.posix_acl_access`) and the
      SELinux label (`security.selinux`);
    - macOS: `fcopyfile` with `COPYFILE_METADATA` (mode, ACL, extended attributes, the quarantine
      flag included), in a new opt-in unsafe module `crates/imperium-ipc/src/macos/copy_metadata.rs`;
    - Windows: the security descriptor (owner, group, DACL, label) through `GetSecurityInfo` and
      `SetSecurityInfo` on the handles, the basic attributes through `FileBasicInfo`, and every
      named stream (`Zone.Identifier` included) listed with `FileStreamInfo` and copied by opening
      each stream relative to the file handle, in `replace.rs`.
  - **In-place fallback.** The save writes in place instead (truncate and write through the handle
    already opened, never a path) when the file's owner is not the daemon's user (a rename would
    change the owner), or when any metadata copy fails; the save's result `note` says so, and the
    docs say a crash during an in-place write can leave a truncated file. Hard-linked files are not
    written at all (D7).
  - **Windows sharing violations** (another program or antivirus holding the target or the
    temporary file): the rename is retried after 50, 100, 200, 400 and 800 ms; still refused, the
    save falls back to in-place if the target can be opened for writing, and otherwise fails with
    `FILE_BUSY` (D6).
  - **Leftover temporary files** (from a crash) are removed when the root is next opened: only
    names matching the pattern, only regular files with `nlink == 1`, older than a day, opened and
    removed through the held `Dir`.
  - Rejected: **always in place** (a crash or full disk leaves a truncated file); **`ReplaceFileW`**
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
    Under the lock below the daemon opens the target, checks it is the same file and hashes its
    bytes; if either differs it writes nothing and fails with the new error `FILE_CONFLICT` (the
    next free code, `-32010` today), whose data is the current `{hash, file_id, size,
    modified_ms}` (or `deleted`). `etag` (item 55's size, time and partial hash) is used only to
    skip re-reads, never to accept a save.
  - **One lock per file, shared with every writer in the daemon.** A new
    `crates/imperium-tools/src/file_locks.rs` keeps an async mutex per **lock key** (weakly held,
    so the table does not grow). The key is the **final path of the opened handle** (Linux:
    `readlink` of `/proc/self/fd/<n>`; macOS: `fcntl(F_GETPATH)`; Windows:
    `GetFinalPathNameByHandleW` with `FILE_NAME_NORMALIZED`), case-folded on a case-insensitive
    volume; for a file that does not exist yet it is the parent's final path joined with the
    name. Not the requested spelling (aliases would get different locks) and not the file id (an
    atomic replace changes it). The holders:
    - `files.write`, around compare and replace; it waits at most 5 s and then fails with the new
      error `FILE_BUSY` (the next free code after `FILE_CONFLICT`);
    - `write_file` and `edit_file` (`imperium-tools/src/files.rs`), around their read and write.
      They take it **after** the permission check, any approval and the `tool.before` hook have
      finished (those run in the engine's dispatch before the tool body), so a waiting approval
      never holds a lock;
    - `session.rewind` (restoring a checkpoint) and `session.review_hunk` when it reverts, around
      each file they rewrite; an operation that rewrites several files takes all their locks first,
      **in sorted key order**, so two multi-file operations cannot deadlock.
  - A command (`run_command`, a build, another editor) can still change the file between the
    compare and the rename; that window is the time to hash and rename and is a stated
    [residual risk](#residual-risks).
  - Rejected: **compare by `etag` only** (a same-size edit within the clock's resolution keeps
    it); **advisory OS locks** (`flock` and Windows share modes are ignored by most programs and
    would block agents' tools on Windows); **paging editable files** (a page boundary can fall
    between two versions of the file).
- **D7. Guarded targets, judged on what was opened.**
  - **How a target is judged.** After D4 has opened the target (or its parent, for a create), the
    daemon takes the handle's **final path** (as for the lock key, D6) and the **file identity of
    every directory on it** from the root down, and judges those, never the requested spelling.
    The name checks reuse the safety floor's own resolved-form check
    (`imperium_policy::permissions::safety_floor` with `CallKind::Edit` on the final path, which
    already folds case on Windows and macOS and covers `.git` anywhere in the path and the floor's
    file list), so the editor and agents' tools cannot drift. Because the final path comes from the
    handle, a symbolic link inside a root to `.git/hooks/pre-commit`, a short name (`GIT~1`), a
    trailing dot (`.git.`) and a case variant (`.GIT`) are all judged as the `.git` folder they
    opened.
  - **Identity, not only names.** The daemon records at start the file identities of `<data>` and
    `<config>`, and at root open the identities of each repository's git folder and common folder
    (`git rev-parse --git-dir --git-common-dir`, through `safe_git`); a target whose directory
    chain contains one of those identities is refused whatever it is called (a bind mount or a
    folder renamed by a link).
  - **Hard refusals** (`-32005`, and the file opens read-only with the reason): anything in a
    `.git` folder or a repository's git or common folder; anything in `<data>` or `<config>`
    (mods, plugins, approvals, `daemon.json`); a file with more than one hard link (its other names
    may be in a guarded place, and an in-place write would change them all); a file the editor
    cannot decode losslessly (D8).
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
  - **Stale full overwrites are refused.** Each session keeps a **last-read hash** per file (by
    lock key, D6): `read_file` records the SHA-256 of the whole file as it was when read (a paged
    read records the hash of the file at the first page), and `write_file` and `edit_file` record
    the hash of what they wrote. `write_file` over an **existing** file whose current hash differs
    from the session's recorded one is refused with a message the model can act on ("`src/lib.rs`
    changed since you last read it (saved by the user in the editor); read it again before
    replacing it"). A file the session never read is written as today (no behaviour change for
    new files or first writes), and `edit_file` is unchanged (it re-reads and matches exact text).
    Subagents share their session's map; a writer subagent in its own worktree has its own paths.
  - **The next-turn note** (`harness.user_edit_notes`, default on): when the user saved files of the
    session in the editor since the last turn, the next turn's context starts with a short system
    note listing them ("The user edited these files in the editor: `src/lib.rs`, `README.md`"),
    names only, never content. The daemon records each successful `files.write` per session; the
    note is built from that list and cleared when sent.
  - These change `crates/imperium-tools/src/files.rs` and the engine's turn setup
    (`crates/imperium-engine/src/agent/`); they are stage E4b.
  - Rejected: **refusing every write to a file not read first** (changes behaviour for every new
    file and for evals that write before reading); **sending the user's diff to the model**
    (context cost and possibly secrets; the agent can read the file).
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
  the editor opens everything read-only; `harness.user_edit_notes` (bool, default `true`, D10).
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
- Language servers, completion, diagnostics, formatting on save, linting.
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
   judged on the final path and directory identities.
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

### Residual risks

- **Commands racing a save.** A program outside the daemon (an agent's `run_command`, a build, the
  user's other editor) that writes between the hash check and the rename loses its write to the
  user's save, or the reverse. The window is the time to hash and rename; the lock covers every
  writer inside the daemon.
- **An evading same-user process** can drive the app's window or detach from the daemon to pass
  `may_grant` (item 55's residual risk, unchanged); it can also set `HKLM` WebView2 policy only
  with an administrator's rights.
- **In-place fallback** (D5): when the owner differs or metadata cannot be copied, a crash during
  the write can leave a truncated file; the save's note says it was written in place.
- **macOS has no `RESOLVE_BENEATH`;** the component walk and the held parent handle cover it, as in
  item 55.

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
arguments, relayed (stage E2). Config: `files.write_enabled`, `harness.user_edit_notes` (D15).

Not added: a `files.changed` notification (the editor polls, D9), `files.delete`, `files.rename`.

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

## Stages

Each stage is one pull request against `experimental`, with the docs it changes. Sizes are
estimates of changed lines, tests included; a stage that grows past about 800 lines splits.

### Stage E1a: Consistent reads, stat, baseline and the shared file lock

- Objective: `FileContent`'s new fields and the whole-file read for editable text (D6, D8);
  page identity on paged reads; `files.stat` and `files.baseline` (protocol minor at merge);
  `imperium-tools/src/file_locks.rs` with final-path keys (D6), taken by `write_file` and
  `edit_file` after dispatch and by `session.rewind` and `session.review_hunk`'s revert in sorted
  key order; the guarded-target judgement of D7 as a function over an opened handle, calling the
  safety floor on the final path; the `test-hooks` feature and its barriers.
- Depends on: item 55 stage 6a.
- Owned paths: `crates/imperium-core/src/files.rs`, `rpc/files.rs`, `src/state/` (rewind),
  `src/review.rs` (hunk revert), `Cargo.toml` (`test-hooks`), `tests/files.rs`, `tests/phone.rs`,
  `crates/imperium-tools/src/file_locks.rs` (new), `src/files.rs`, `src/lib.rs`,
  `crates/imperium-policy/src/permissions.rs` (only if the floor needs a resolved-path entry
  point), `crates/imperium-protocol/src/methods/files.rs`, snapshots and generated types,
  `docs/02-architecture.md`.
- Acceptance checks: `cargo test -p imperium-core --test files --test phone` and
  `cargo test -p imperium-tools` on the matrix:
  - `hash` equals the SHA-256 of the bytes on disk and the editable read is one response;
    **torn read:** with the read parked at `after_open`, the test rewrites the file and releases
    it, and the returned text and hash still describe one version (the old one, read through the
    already-open handle, or a refusal), never a mix; **paged read:** a 6 MiB file rewritten
    between the first and second page reports a different `file_id`, `size` or `modified_ms` on
    the second page;
  - `eol` is `lf`, `crlf`, `mixed` (with the majority) and `none` for the four fixtures, and a lone
    CR is `mixed`; UTF-8 with BOM, UTF-16 LE and BE with BOM report their encoding and `bom`;
    invalid UTF-8 and BOM-less UTF-16 report `writable: false` with the reason;
  - `.env` reports `needs: secret`; `.imperium/config.toml`, `.claude/settings.json`,
    `.mcp.json` and `.claude-plugin/plugin.json` report `needs: harness`; `.git/config` and a file
    in `<data>` placed under a root report `writable: false`; a 5 MiB file reports
    `writable: false`;
  - `files.stat` of 51 files is refused, of 50 answers, of a removed file says `exists: false`;
    `files.baseline` returns the checkpoint text, `HEAD` when there is no checkpoint, `none` for a
    new file, and `agent_changed_this_session` after a scripted `edit_file`;
  - **lock keys:** `src/a.txt`, `SRC/A.TXT` (on a case-insensitive volume) and an inside link to it
    get the same key; the key survives an atomic replace;
  - **lock holders:** two `edit_file` calls appending to one file from two tasks both land; an
    `edit_file` waiting on approval holds no lock (a parallel `files.stat` and a rewind proceed);
    a `session.rewind` of files `b` and `a` and a hunk revert of `a` and `b` started together
    finish without deadlock (taken in sorted order, checked with barriers); the lock table is empty
    after all finish;
  - the phone is refused `files.stat` and `files.baseline`; the floor's existing tests pass.
- Size: about 800 lines.
- Status: pending
- Outcome: -

### Stage E1b: `files.write`, the guarded targets and atomic replace

- Objective: `files.write` (protocol minor at merge) under D3 to D7: the gate (item 55's
  `app_gate.rs`, with `files.write` added as a caller and the release-build rule), the spelling
  checks on every component, the held parent, `nlink == 1` regular files only, `create` with
  `O_EXCL`, compare under the lock with the 5-second `FILE_BUSY`, the guarded-target judgement and
  confirmations, the handle-relative atomic replace with metadata copy, `fsync` of the parent, the
  Windows retries and the in-place fallback, leftover cleanup, encoding by `encoding` and `bom`,
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
    `FILE_BUSY`;
  - **escapes**, each refused with nothing created or changed outside: `../`, absolute, drive,
    UNC, `\\?\`, a stream name, `NUL.txt` and the other device names, a symbolic link (or junction
    on Windows) to outside as the target and as a parent, a `create` through a link;
  - **parent swap, deterministic:** with the save parked at `after_parent_open`, the test renames
    the parent away and puts a link to outside in its place, then releases; the new file lands in
    the original (moved) directory and nothing appears outside; the same at `before_rename`;
  - **guarded targets** (B1), each refused or asking as D7 says, with nothing written: an inside
    symbolic link to `.git/config`; a hard link to `.git/hooks/x` (refused for `nlink > 1`); a
    hard link to a file outside the root (refused for `nlink > 1`); `.GIT/config` (refused on every
    OS: on a case-insensitive volume it is `.git`, on a case-sensitive one the floor's check is
    applied to the final path with folding); `GIT~1/config` on Windows with short names enabled on
    the test volume (`fsutil 8dot3name set <vol> 0`); `.git./config` on Windows; a bind mount or
    second link of `<data>` under a root on Linux (refused by identity); `.env` without
    `confirmed: ["secret"]` (refused with `needs`) and with it (written); `.mcp.json` without and
    with `confirmed: ["harness"]`;
  - **metadata:** on Linux a file with mode `0640`, a `user.test` extended attribute and a POSIX
    ACL keeps all three; on macOS a file with `com.apple.quarantine` and an ACL entry keeps both; on
    Windows a file with an explicit DACL entry, the read-only attribute cleared and a
    `Zone.Identifier` stream keeps the DACL entry and the stream; a file owned by another user
    (Linux, made with the runner's `sudo`) is written in place with a `note`; a metadata copy made
    to fail through a test hook falls back to in-place with a `note`;
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

- Objective: D10: the per-session last-read hash map (`read_file`, `write_file` and `edit_file`
  record; keyed by lock key), `write_file`'s refusal of a full overwrite of an existing file
  changed since the session's last read, the daemon's per-session list of user saves, and the
  next-turn note behind `harness.user_edit_notes`.
- Depends on: stages E1a and E1b.
- Owned paths: `crates/imperium-tools/src/files.rs`, `src/workspace.rs` (the map's home on the
  session's workspace), `crates/imperium-engine/src/agent/` (the turn's opening note),
  `crates/imperium-core/src/files/write.rs` (recording saves), `crates/imperium-config/src/settings.rs`
  and `registry.rs`, the tools' and engine's tests, `docs/03-agent-engine.md`,
  `docs/05-tools-and-isolation.md`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-tools -p imperium-engine -p imperium-core` on the
  matrix: a `read_file`, then a user `files.write`, then a `write_file` of the same file is refused
  with the message naming the change and the file is unchanged; after a fresh `read_file` the
  `write_file` succeeds; a `write_file` of a new file and of an existing file the session never
  read succeed as today; an `edit_file` after a user save applies to the saved text; a subagent
  shares its session's map; with the setting on, a turn after two user saves starts with a note
  naming both files and no content, and the next turn has none; with it off, no note. The
  headless eval runner's existing tests pass unchanged (no eval writes over a file changed under
  it).
- Size: about 500 lines.
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
landed.

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
  `experimental`, never rebase.

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
