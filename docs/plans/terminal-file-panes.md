# Plan: terminal and file panes

Revision: 2026-11-26, revision 2 (plan-gate review: findings F1 to F9 and the nits folded in);
written against `experimental` at `3d5db869` (last updated). Roadmap
[item 55](../12-roadmap.md#next-in-order). Status: planned (no stage started).

Claude's desktop app shows a terminal and a file viewer beside the conversation. Imperium gets the
same two panes: a **Terminal** panel with one or more shells in the open session's working
folder, and a **Files** panel that browses and shows the session's files, opened from links in
the transcript and from the Review panel. Both are ordinary panels of the layout (closable,
detachable, placeable in any column).

The terminal is the riskier half. It is a shell with the user's full rights, driven from a webview
that also hosts third-party mod frames and project pages. The plan's core is therefore a rule
about **who may type**: only the user's keystrokes in the desktop app reach the shell, through the
app's native side and the daemon's existing desktop-app identity check, and nothing an agent, a
mod, a page, a phone or another protocol caller can reach opens, reads or writes a terminal. The
file pane is read-only and confined to the session's folders. What the plan does **not** stop, a
command of the same user at isolation tier 0 reaching the terminal device directly, is stated as a
[residual risk](#residual-risks).

Each stage below is one pull request against `experimental` of at most about 800 changed lines
(ten stages), with the docs it changes. No stage changes the research session's files
(`crates/imperium-engine/src/agent/{tools.rs,turn.rs,compact.rs}`, `eval/record.rs`,
`eval/variant.rs`): agents get no terminal tool and no file-pane tool, so the engine is untouched.

## Goals

Each goal is boolean and has a check that runs in CI, with no manual step. "Fake daemon" is the
browser-check harness of [stage 4](#stage-4-browser-check-harness); "the matrix" is the `rust`
job of `ci.yml`, which runs `scripts/check-rust.sh` on Ubuntu 22.04, Windows and macOS. Goals 2 to
4 are about **protocol callers and code in the app's webview**, and about agents' commands at the
OS sandbox and container tiers; a tier-0 command runs as the user and is covered only as a
[residual risk](#residual-risks).

1. **A terminal opens in the session's working folder.** From the desktop app the user opens up to
   four terminals per open session; each starts the user's shell in the session's first folder
   (its worktree when the session works in one), sized to the pane. Check:
   `cargo test -p imperium-core --test terminal` on the matrix (the shell prints its working
   folder and the size it was given); the stage 8 Linux end-to-end run of the real app and daemon.
2. **Only the desktop app can use a terminal over the protocol.** Every `terminal.*` method is
   refused over loopback TCP, to a paired phone, to a local-socket caller that is not the
   `imperium-desktop` installed beside `imperium-core`, and to any caller that the daemon started
   or that carries the daemon's spawn mark. Check: stage 2b tests on the matrix, including a real
   child process with the spawn mark connecting on the local socket.
3. **No webview content but the app's own code can reach the terminal.** The Tauri commands that
   carry terminal traffic are refused to a mod frame, a Preview page and any origin other than the
   app's own, and are refused while a trusted main-context mod is loaded; in the app's code only
   the terminal's key handler sends input. Check: the stage 3 `tauri::test` access-control test on
   the matrix; the stage 5 source scan (`npm test`); the stage 8 real-app frame attempts on Linux.
4. **Nothing but the attached desktop app sees terminal bytes.** Terminal input and output never
   reach a model, the session journal, session events, hooks, mods, exports, session search,
   telemetry, logs, the app's storage or another connection. Check: the stage 2b sentinel test,
   the stage 3 relay log test and the stage 5 browser storage check.
5. **Terminal output cannot act for the user.** An OSC 52 clipboard write changes no clipboard; an
   OSC 8 or detected link opens only after the user confirmed its full address (host shown in
   punycode), and only for `http`, `https` or a `file:` path inside the session's folders (which
   opens in the Files panel); a title is shown as plain text; no sequence makes the terminal type a
   reply that carries text the output chose. Check: stage 5 `npm test` policy tests and browser
   checks.
6. **No terminal process outlives its terminal.** Closing a terminal, archiving or deleting its
   session and stopping the daemon end the shell, its background jobs and their children. Check:
   stage 1 and 2a tests on the matrix with a job-control shell running `sleep 60 &` (and its
   Windows equivalent). Known gap: on macOS, and on Linux without the subreaper, a process that
   deliberately calls `setsid` and leaves the shell's session escapes (as it does from background
   commands, [05](../05-tools-and-isolation.md#background-commands-implemented)). If the daemon
   itself crashes, Unix children survive it; Windows children die with the Job Object when the
   daemon's last handle to it closes.
7. **The file pane reads only inside the session's folders.** No path, `..`, absolute or UNC
   spelling, symbolic link, junction, Windows stream or device name, special file, other session's
   worktree, and no race that swaps a link in, makes `files.*` return bytes or names from outside
   the session's folders and scratch folder. Check: `cargo test -p imperium-core --test files` on
   the matrix.
8. **The file pane shows files safely.** Text is highlighted and drawn as text (never HTML), large
   files are capped and say so, binary files are not shown as text, images are shown only when
   their bytes are an image format the pane allows, and invisible or direction-changing characters
   are marked. Check: stage 7a `npm test` and browser checks.
9. **Links open the file pane at a line.** A `path:line` link in the transcript, a tool call's path
   and a Review panel file open the Files panel at that file and line; the Files panel opens the
   Review panel at a changed file's diff. Check: stage 8 browser checks.
10. **A paired phone gets neither pane.** Check: `cargo test -p imperium-core --test phone` (every
    `terminal.*` and `files.*` method refused with `-32005`).
11. **Every check runs in CI.** Check: the new tests are in `cargo test --workspace` (the matrix),
    `npm test` and the new browser checks of the `web` job, and the Linux end-to-end job; none
    needs a person, a display server outside the job, or a network service other than package
    installs.

## Decisions

Each records the choice, the reasons, the rejected options and when to revisit it.

- **D1. The PTY lives in the daemon.** `imperium-core` spawns and owns each terminal's pseudo
  terminal and shell. Reasons: the session's folders, worktrees, environment and lifetime are the
  daemon's, and a terminal belongs to a session (it ends when the session is archived or deleted,
  which the app may not be running to see); a terminal survives a reload of the window and a
  restart of the app while the daemon runs (the app's update relaunch included), with its
  scrollback; the daemon already ends process trees (the background-command code in
  `imperium-tools/src/processes.rs`); and the same design can later serve a remote daemon, where
  the files are. Rejected: **the Tauri process**, which needs no protocol at all but ties the shell
  to the app's life, puts it on the wrong machine when the app is connected to a remote daemon,
  and duplicates the daemon's process and session bookkeeping; **a separate PTY helper process**,
  which adds a process and an IPC boundary without removing either of the others' problems.
  Revisit if the daemon's process must stay free of terminal I/O (for example a measured stall of
  other connections under heavy output).
- **D2. Only the desktop app on the local socket, through the app's native side.**
  - The webview never talks `terminal.*` itself. The app's Rust side (a new
    `src-tauri/src/terminal.rs`, built like `plugin_approval.rs`) holds its own connection to the
    daemon on the local socket and relays: the window invokes Tauri commands, and output comes
    back on a Tauri `Channel`. The relay's connection **takes no daemon lease**: the app's native
    lease already holds the daemon, and a second one would keep it alive after the app's lease
    ends.
  - The daemon answers `terminal.*` only when the connection passes `app_identity::InstalledApp`
    (checks 1 to 4 of the plugin approval: local socket, the peer identified at accept, the
    `imperium-desktop` installed beside `imperium-core` and outside a sandbox, none of the refused
    loader or WebView2 variables) **and** `peer::may_grant` (the check `mods.install` uses for a
    trust grant: the peer and its ancestors are not the daemon or `sshd` and carry no
    `IMPERIUM_SPAWNED_BY_DAEMON` mark). Both run **once**, on the connection's first `terminal.*`
    call, and the verdict is cached on `rpc::Connection` (an identity belongs to the process that
    connected, as for approvals); later calls on that connection cost nothing. The process check
    sits behind an injectable trait (`TerminalGate`) so in-process tests pass a stub, while a
    matrix test runs the real check against a real child process.
  - As [06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented) says of
    `may_grant`, this is **not a boundary against a process trying to evade it**: one that detaches
    and clears its environment, or acts through the user's desktop session, is not caught. It stops
    the mistake and the plain case, an agent's shell or an `imperium` command reaching for a
    terminal.
  - Not required: the challenge (there is no single decision to bind), the OS presence check, and
    the Windows signed-build rule (U7). Those exist because an approval outlives the moment and
    must not be forged by a process that can rewrite the app's folder; here the threat is a caller
    that has no shell getting one, and a process that can rewrite the app's folder already runs
    code as the user.
  - **Main-context mods.** A trusted main-context mod
    ([06](../06-extensibility.md#trusted-main-context-mods-trust-implemented-loader-not-built),
    loader not built) would run in the very window that holds the terminal and could type into it.
    So the terminal commands are refused while any trusted main-context mod is loaded, the pane says
    why, and the loader's own future plan must isolate such a mod from the terminal before this
    rule can relax.
  - Rejected: **gating in the webview only** (the main window's token reaches loopback TCP, and so
    would anything that reads `daemon.json`); **plain token auth** (an agent's command with the
    token, an `imperium` command run by an agent, or a forwarded remote client would get an
    unsandboxed shell and so step around permission rules); **a native confirmation on
    `terminal_open`** (friction on every terminal, and no protection for the keystrokes after it:
    whatever could open a terminal could wait for the user to open one). Revisit when the desktop
    app can drive a remote daemon (the forwarded socket's peer is `sshd`, so the gate refuses it).
- **D3. Up to four terminals per session, sixteen per daemon, tied to the session.** A terminal
  starts when the user opens one, and ends when its shell exits (it stays listed as ended, with
  its output and exit code, until closed), when the user closes it, when its session is archived,
  deleted or stopped, when no desktop connection has been attached to it for 60 minutes, and when
  the daemon stops. Terminals are **not daemon work**: they do not keep an idle daemon alive, so
  closing the app ends them after the idle grace (about 10 s) unless the daemon is kept running
  (Settings → Daemon) or has other work. The pane's empty state, and the app's close dialog when
  terminals are running, say so. Reasons: an invisible shell that keeps the daemon up for ever is a
  surprise and a leak; the update relaunch and a window reload fall inside the grace; four is
  enough for a server, a watcher and a shell. Revisit with [open question 1](#open-questions).
- **D4. Scrollback in memory only; nothing survives a daemon restart.** Each terminal keeps its
  last 1 MiB of output in a ring buffer in the daemon for re-attaching, and nothing on disk:
  terminal output often holds secrets (`env`, tokens, `cat .env`). A daemon restart ends every
  shell; the pane then shows the terminal as gone and offers a new one. Rejected: persisting
  scrollback to `<data>` (secrets at rest, and the shell it described is dead anyway).
- **D5. Ending a terminal's processes.** A job-control shell on a PTY puts each background job in
  a process group of its own, so killing the shell's group misses `sleep 60 &`.
  - **Unix: by session.** The shell leads a new session (its sid is its pid). To end a terminal the
    daemon lists the processes whose sid is the shell's pid (`sysinfo`), sends `SIGHUP` to each of
    their process groups, waits 2 s, then `SIGKILL`s what is left. On Linux the daemon is also a
    child subreaper (`PR_SET_CHILD_SUBREAPER`, set once at start), so a process that calls `setsid`
    and is orphaned is reparented to the daemon and is ended with its terminal: the daemon
    kills each re-parented child whose environment (`/proc/<pid>/environ`) carries that terminal's
    `IMPERIUM_TERMINAL_ID`. macOS has no subreaper: a process
    that deliberately leaves the session escapes ([goal 6](#goals)).
  - **Windows: a Job Object** with kill-on-close. Where `portable-pty` lets the spawn pass a
    `PROC_THREAD_ATTRIBUTE_JOB_LIST`, the shell starts inside the job atomically; otherwise it is
    assigned right after spawn, as background commands are, and a child the shell starts in that
    short window is outside the job (it is then killed by walking the shell's process tree first,
    as `processes.rs` does).
  - **If the daemon crashes:** Unix shells and their jobs survive it (the PTY master closes, which
    sends `SIGHUP` to the foreground job only); Windows jobs die when the last handle closes, which
    the crash does.
- **D6. The shell and its environment.**
  - **Program.** `terminal.shell` (user layer only; not on the environment layer's allow-list, so a
    repository's `.imperium/config.toml` cannot choose what runs). Otherwise, on Unix the
    account's shell (`getpwuid` → `pw_shell`) when it is listed in `/etc/shells`, else `$SHELL`,
    started as a login shell (`-l`, so a macOS app gets the user's `PATH`). On Windows `pwsh.exe`
    found in its install locations (`%ProgramFiles%\PowerShell\<n>\`, read through
    `SHGetKnownFolderPath`) or its App Paths registry key, never by a `PATH` search; else
    `%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe`; else
    `%SystemRoot%\System32\cmd.exe` (not `%ComSpec%`, which the environment controls), with
    `%SystemRoot%` from `GetSystemWindowsDirectoryW`.
  - The program is always an **absolute path** chosen without the working folder or a relative
    `PATH` entry: Windows' `CreateProcess` looks in the current folder first for a bare name, and
    the working folder is a repository that may hold a planted `pwsh.exe`. A `terminal.shell` that
    names a bare program is resolved by a `PATH` search that skips empty, relative and
    repository-absolute entries (any entry inside the session's folders).
  - **Working folder:** the session workspace's root (`Workspace::root`).
  - **Environment:** the daemon's own, minus variables that make a shell or a tool run code it
    was not asked to (`ENV`, `BASH_ENV`, `ZDOTDIR`, `PROMPT_COMMAND`, `NODE_OPTIONS`, every
    `LD_*` and `DYLD_*`) and the daemon's internal `IMPERIUM_*` variables, plus
    `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=Imperium`,
    `TERM_PROGRAM_VERSION`, `IMPERIUM_SESSION_ID`, `IMPERIUM_TERMINAL_ID` and the kept spawn mark
    `IMPERIUM_SPAWNED_BY_DAEMON=1`. The user's own start-up files set their own values again.
  - It is **not** stripped of `GH_TOKEN`, `GITHUB_TOKEN` or `GH_CONFIG_DIR` (that stripping
    protects agents' commands; this shell is the user's), and it runs **outside every isolation
    tier and permission rule**, which govern agents.
  - **Consequences of the spawn mark:** a trust grant (`imperium mods install` with `trust`) is
    refused from the pane, as from any shell the daemon starts (the pane's input passes through a
    webview, and a grant needs a terminal of the machine's own); and an `imperium` command run in
    the pane takes no daemon lease (a daemon-spawned client never holds the daemon up), so it is
    not counted by `imperium daemon status` as a client.
  - Sessions whose environment has a remote runtime (container, WSL, SSH) get no terminal in this
    plan; the pane says so.
- **D7. Agents cannot see or type into a terminal.** No agent tool, MCP bridge method
  (`session.run_tool`), hook event, mod event, export or search reaches a terminal; only the user
  moves text between a terminal and the conversation, by copy and paste. The app never types into
  a terminal on anyone's behalf: there is no "run this in the terminal" action, because transcript
  text can be injected. Rejected for this plan: a "share this terminal with the agent" switch. It
  would need the output framed as untrusted data, reading as an approval-gated tool, and typing
  approved per input; it is recorded as a follow-up, not designed here.
- **D8. A paired phone gets no terminal and no file pane.** The phone's scopes (`read`, `approve`)
  do not grow. Reasons: terminal output and repository files often hold secrets, a leaked phone
  token would expose them, and a read-only phone view adds little while the phone app has no pane
  for it. The daemon refuses `terminal.*` (D2) and `files.*` to phone connections. This plan wins
  over [15](../15-mobile-and-remote.md#scope)'s "view artefacts (files …)" for the file pane; 15's
  scope now says so. Revisit with [open question 2](#open-questions).
- **D9. Escape sequences: the emulator is configured, not filtered.** The terminal is xterm.js
  (`@xterm/xterm` with the `fit`, `search`, `unicode11` and `web-links` add-ons).
  - OSC 52 is ignored because the clipboard add-on is not loaded; image protocols (sixel, iTerm)
    are ignored because the image add-on is not loaded; `windowOptions` stay all off, so no title
    or window report is typed back.
  - OSC 8 links (xterm's `linkHandler` option) and plain-text links (the `web-links` add-on, given
    the same handler) go through one function: only `http`, `https` and an in-scope `file:`; the
    confirmation shows the full address, the host in punycode for an internationalised name, and
    the visible text when it differs.
  - OSC 0 and 2 titles are cleaned (control, direction and zero-width characters removed, 80
    characters) and shown as text; the bell is visual.
  - Pasting strips ESC and C1 controls when bracketed paste is on, and asks before a multi-line
    paste when it is off.
  - The rules live in a pure module (`src/terminal/policy.ts`) tested by `npm test`. Rejected: a
    daemon-side filter of the byte stream (it needs a second terminal parser that must agree with
    the emulator's; the emulator's configuration is the boundary that matters).
- **D10. Input is keystrokes only.** Bytes reach `terminal_input` only from xterm.js' `onData` and
  `onBinary` (the user's keys and pastes, and the emulator's own answers to the shell's queries,
  such as cursor-position reports). One module (`src/terminal/keys.ts`) calls the command; a
  source scan in `npm test` fails if any other file names it. While the terminal has focus every
  key goes to the shell (Ctrl+K included, which readline uses) except copy and paste
  (Ctrl+Shift+C and V; Cmd+C and V on macOS) and **Leave terminal** (F6, rebindable), which moves
  focus to the next panel so the keyboard is never trapped.
- **D11. The file pane is a read-only viewer.** No editing, saving, renaming or deleting. Reasons:
  agents edit the same files mid-turn, and an editor needs conflict handling, encoding-preserving
  saves, checkpoints and undo that the Review panel and agents' tools already own. Actions are
  *Copy path*, *Copy relative path* and *Reveal in folder*. There is no "open with the default
  application": for `.bat`, `.lnk`, `.desktop`, `.app` or a script that runs it, and an agent can
  write such a file. **Reveal** is done by the app's native side, which asks the daemon for the
  session's roots itself and resolves `{root, path}` (never a path from the webview), then:
  Windows `SHOpenFolderAndSelectItems` (or `explorer.exe /select,` with the path quoted as one
  argument); Linux the D-Bus `org.freedesktop.FileManager1.ShowItems` call, falling back to
  opening the **parent folder** only, never `xdg-open` on the file; macOS `open -R`. Revisit with
  [open question 3](#open-questions).
- **D12. Scope and safe opening.**
  - **Roots** are exactly `Workspace::roots()` (the session's folders, with worktrees when the
    session works in them) plus `Workspace::scratch()`; not path grants (they are agents'
    permissions and may name any folder), not `with_project_worktrees` (other sessions'
    worktrees). They are canonicalised and opened once per session as `cap_std::fs::Dir` handles
    (`Dir::open_ambient_dir`), held by the file service and dropped with the session, so a later
    swap of a root's own path cannot redirect it.
  - Paths are `{root, relative}` pairs, never free-form. Before opening, a relative path is refused
    if it has a drive, UNC or `\\?\` prefix, is absolute, has a Windows stream name (`:`), or has a
    component that is a reserved device name: `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`,
    `COM1` to `COM9` and `LPT1` to `LPT9`, the superscript `COM¹` to `COM³` and `LPT¹` to `LPT³`,
    each with any extension and with trailing dots or spaces.
  - **Opening** goes through the held `Dir` (`Dir::open`), which refuses `..` past the root and
    links or junctions that leave it (`openat2` with `RESOLVE_BENEATH` on Linux, component by
    component elsewhere), so there is no check-then-open race. Files are opened non-blocking, the
    handle is `fstat`ed, and only regular files are served: a FIFO, socket or device is refused
    without a read that could block.
  - **Listing** uses the held `Dir`'s `read_dir` and `ignore::gitignore::Gitignore` matchers
    built from the `.gitignore` files read the same way, applied to names; not the `ignore` path
    walker, which opens paths by name. Entries mark links and whether they lead outside.
  - A link inside the folders that stays inside is followed. Agent deny rules do not apply: they
    say what agents may touch, and the viewer is the user's (quick open reuses `session.files`,
    which hides denied paths as mentions do; the tree shows them).
  - Rejected: `canonicalize` then `starts_with` (a race between the check and the open), and
    `Workspace::resolve` (it reads the loose spellings models write, includes grants, and checks
    before opening).
- **D13. Rendering.** CodeMirror 6, read-only (`EditorState.readOnly`, `EditorView.editable`
  false): virtual rendering of long files, line numbers, search, go to line, selection, and
  languages loaded on demand from `@codemirror/language-data` (bundled chunks, so the app's
  `default-src 'self'` holds; `style-src` already allows inline styles). Text up to 4 MiB is loaded
  (in 1 MiB reads); highlighting is off above 1 MiB or for a line over 10,000 characters; a larger
  file shows its first 4 MiB and says so. A file is binary when its first 8 KiB hold a NUL byte
  and it is not UTF-16 with a byte-order mark (the `read_file` rule); invalid UTF-8 is shown with
  U+FFFD and a note. Images are shown when the daemon finds PNG, JPEG, GIF, WebP, BMP or ICO magic
  bytes, up to 10 MiB, as a `data:` URL (`img-src data:` is allowed); SVG is shown as source.
  Markdown is shown as source. Bidirectional controls and zero-width characters are drawn as
  visible markers with a notice (the "trojan source" case). Rejected: Shiki, highlight.js and
  Prism (they produce HTML strings for `innerHTML` and render whole files without virtualising).
- **D14. Files and Review stay separate.** The Review panel owns diffs; the Files panel shows the
  current content. A file the session changed (in `session.review`'s list) shows *Changed in this
  session* with *Show diff*, which opens the Review panel at that file; a Review file row gets
  *Open file*, which opens the Files panel at its first changed line. Links in the transcript (a
  `path:line[:column]` in assistant text or inline code, and the path argument of a file tool
  call) open the Files panel at the line through `files.locate`; a link outside the session's
  folders is shown as text with a tooltip saying why it does not open. A `file:` link in terminal
  output follows the same path. Nothing in the Files panel runs or sends anything.
- **D15. Built-in panels, closed to mods.** The Terminal and Files panels are built into the app,
  not mod-SDK panels: a mod cannot wrap or replace them, and neither panel takes modules (a mod
  frame placed inside the Terminal panel could take focus and catch what the user meant to type,
  a password at a `sudo` prompt for example). No mod permission gives access to terminals or
  files. Sandboxed mod frames share the main window, so the boundary against them is the frame's
  origin, not the window (D2 and [threat 1](#threat-model)); a main-context mod is handled in D2.
  [06](../06-extensibility.md#ui-mods) and [07](../07-desktop-ui.md#panels) are updated to say so.
- **D16. Testing.** Four layers, all in CI.
  - **PTY behaviour on three OSes:** Rust tests with real shells (`/bin/sh` and `bash -i` on Linux
    and macOS, `cmd.exe` and Windows PowerShell on Windows, through ConPTY) on the matrix,
    asserting on output that contains rather than equals (ConPTY adds its own start-up sequences),
    with bounded waits.
  - **IPC access rules:** `tauri::test` (`mock_builder`, then `get_ipc_response` with a forged
    origin) on the matrix. This is the only layer that checks which origins may invoke a command.
  - **Panes in a browser against a fake daemon:** a new Playwright suite (`apps/desktop/e2e/`)
    runs the Vite build in Chromium and WebKit against a Node fake daemon (JSON-RPC over
    WebSocket, scripted sessions, files and terminals), with the Tauri IPC replaced by an init
    script so `invoke` and `Channel` reach the fake; it runs in the `web` job. Because the IPC is
    replaced, **it cannot verify IPC access rules**; it checks behaviour and rendering only.
  - **The whole chain once, on Linux:** a scenario added to the real-app end-to-end run
    (`scripts/ci/presence-e2e/`, release daemon and app under Xvfb through `tauri-driver`) types
    into a real terminal and reads the echo, and has a real mod frame and a Preview page try the
    terminal commands.

  Rejected: manual checks of any kind.

## Current state (found on `experimental` at `3d5db869`)

Implemented and reused:

- **Panels and layout.** Built-in panels are entries in `App.tsx` (`rightPanels`, with `id`,
  `title`, `body`) and in its `PANELS` list (Settings → Panels); the layout code
  (`src/layout/`) arranges, folds, stacks and detaches them (`panel-<id>` windows). The Preview and
  Browser panels ([07](../07-desktop-ui.md#preview-and-browser-panels-implemented), roadmap items
  9 and 32) are the pattern for a panel that follows the open session; the Review panel
  ([07](../07-desktop-ui.md#review-panel-implemented), item 6) is the pattern for daemon-computed
  file data. [07 panels](../07-desktop-ui.md#panels) already names "files, diff, terminal" as
  panels (aspirational).
- **The desktop-app identity.** `crates/imperium-core/src/app_identity/` identifies the
  `imperium-desktop` beside `imperium-core` on the local socket (Linux pidfd, Windows process
  handle, macOS audit token), behind the `Identify` trait (`InstalledApp` in production, a stub in
  tests); `rpc::Connection` holds the peer (`identity`). `src-tauri/src/plugin_approval.rs` is the
  native-side pattern: own windows only (`own_window`, `capabilities/default.json` for `main` and
  `panel-*`), its own local-socket connection through `imperium_client::Client`, the start-up
  `Guard` over the app's own environment (`imperium_ipc::refused_variable`, in
  `imperium-ipc/src/loader_env.rs`: `WEBVIEW2_*`, `DYLD_*`, `LD_PRELOAD`, `LD_LIBRARY_PATH`,
  `LD_AUDIT`), and a test that no capability reaches its command from another window or a remote
  page ([06](../06-extensibility.md#plugin-marketplaces)).
- **The window's policy.** `tauri.conf.json`'s content security policy is `default-src 'self'`
  with no `script-src` (so no `unsafe-inline` or `unsafe-eval` for scripts), `style-src`
  with `'unsafe-inline'`, `img-src 'self' data:`, and frames only from `imperium-mod:` and loopback.
- **Process trees.** Background commands end their whole tree (`imperium-tools/src/processes.rs`,
  `win32job` on Windows, the process group on Unix) and are killed with their session
  ([05](../05-tools-and-isolation.md#background-commands-implemented)).
- **Paths.** `Workspace` (`imperium-tools/src/workspace.rs`) knows a session's folders, worktrees
  and scratch folder; `session.files` lists and ranks files for `@` mentions with the `ignore`
  walker ([07](../07-desktop-ui.md#file-mentions-and-init-implemented)).
- **The spawn mark and the trust-grant check** (`IMPERIUM_SPAWNED_BY_DAEMON`, `peer::may_grant`;
  [06](../06-extensibility.md#who-may-grant-trust-and-approval-implemented)).
- **CI.** The `rust` job runs on three OSes; the `web` job runs `scripts/check-web.sh` (lint,
  typecheck, `node --test`, build) on Ubuntu; `presence-e2e.yml` runs the release app and daemon
  on Linux under Xvfb with `tauri-driver`, when `scripts/ci/detect.sh` sees a watched path.

Missing:

- Any PTY, terminal emulator or code viewer: no `portable-pty`, `xterm` or `codemirror` in
  `Cargo.lock` or the desktop's `package.json`; no `cap-std`.
- Any protocol method for terminals or for reading files outside agents' tools.
- A browser test harness for the desktop UI: tests are `node --test` over pure `.ts` modules, with
  no rendering, no fake daemon and no Tauri mock.
- `imperium_client::Client` handles one call at a time (`call` takes `&mut self` and queues
  notifications), so a relay that writes input while reading output needs a split client.

## Scope and non-goals

In scope: terminals in the daemon with the desktop-only gate, the native relay, the Terminal
panel, the read-only Files panel with its daemon methods, links from the transcript, the Review
and terminal output, the browser-check harness, the Linux end-to-end scenario, settings, and the
docs (02, 05, 06, 07, 10, 11, 15, 12).

Not in scope:

- Editing files in the pane, and any "open with the default application".
- Agents reading or typing into terminals; any "run in terminal" action from the transcript.
- Terminals and the file pane for remote runtimes (container, WSL, SSH) and for a remote daemon;
  phone and CLI clients for either pane.
- Persisting terminals or scrollback across a daemon restart; split panes inside one terminal;
  shell integration (command marks, cwd tracking through OSC 7 or 633).
- A file-system watcher (the pane re-reads on events and focus).
- Mods extending, wrapping, replacing or holding modules in either panel; the main-context mod
  loader (D2 refuses terminals while one is loaded).

## Threat model

Assets: the user's account (a shell has all of its rights), the files of the session's folders
and anything else the user can read, what the user types (passwords at a `sudo` or `ssh` prompt),
and the clipboard.

Untrusted parties and inputs: models and their tool calls; mods and their frames; pages in the
Preview panel and anything else in a frame of the app's window; repository content (file names,
contents, `.imperium/config.toml`, planted executables, `PATH`-like folders); terminal output
(programs print attacker-controlled text: `cat` of a file, `curl`, a test's log, `git log`);
transcript text (model output that may carry injected links); a paired phone; other programs of
the same user that hold or can read the daemon's token (an agent's command at tier 0 can read
`daemon.json`).

1. **A shell for mods, pages or agents.** A mod frame (opaque origin, `sandbox="allow-scripts"`),
   a Preview page (a `localhost` origin in an iframe) or any other origin must not start, read or
   type into a terminal. Frames share the main window's label, so the window check (`own_window`)
   is **not** a frame mitigation; the mitigations are:
   - Tauri's origin-based access control: the terminal commands' permissions are granted in
     `capabilities/default.json` to `main` and `panel-*` for the app's own origin only, with no
     `remote` URLs, so an IPC request from a frame's origin is refused. Tested by `tauri::test`
     (stage 3) and by real frames on Linux (stage 8).
   - The daemon's gate (D2): a page reaching loopback TCP, an agent's process with the token, an
     `imperium` command and a forwarded remote client all fail.
   - Terminal output goes only to connections attached to that terminal, never through the
     broadcast path that `events.subscribe` filters.
   - Neither panel takes modules (D15); terminals are refused while a main-context mod is loaded
     (D2).
2. **Input that is not the user's.** The app's own code types nothing (D7, D10); the single sender
   is checked by a source scan; the app's shortcuts do not reach the shell and the shell's keys do
   not reach the app except the listed ones; the terminal never accepts focus while a dialog of
   the app is open. A script running in the main window outside the app's own code is kept out by
   the policy's `script-src` (`'self'` only, no `unsafe-inline` or `unsafe-eval`; a regression test
   in stage 5). A compromised main window is out of scope: it already holds the daemon token.
3. **Terminal output acting for the user** (D9): OSC 52 clipboard writes (ignored), OSC 8 links
   with a misleading text or a look-alike internationalised host (the confirmation shows the full
   address in punycode and the visible text when they differ; only `http`, `https` and in-scope
   `file:`), report sequences that echo attacker text back as input (title reports and window
   operations are off; the remaining replies carry no text the output chose), image protocols
   (ignored), titles with control or direction characters (cleaned), bracketed-paste escape
   (`ESC [201~` in pasted text, stripped), and floods (see 9).
4. **The wrong program or start-up code** (D6): the shell comes from the user layer or the
   account, resolved to an absolute path without the working folder or `PATH` entries inside the
   repository; `ENV`, `BASH_ENV`, `ZDOTDIR`, `PROMPT_COMMAND`, `NODE_OPTIONS` and loader variables
   inherited by the daemon are dropped. What the user's own shell start-up files do in a
   repository (direnv, prompt hooks) is the same as in any terminal opened there; the docs say so.
5. **Path traversal and link escapes in the file pane** (D12): `..`, absolute, drive, UNC, `\\?\`,
   stream and device spellings are refused up front; `cap-std` refuses the rest at open time on
   root handles held since the session started, so a link swapped in between a listing and a read
   cannot lead outside; special files are refused after `fstat`. A hard link inside the folders to
   a file outside is read as the file it is (a hard link is a file of the folder; an agent that can
   make one could already read the target with its own tools); the docs say so.
6. **Hostile file content** (D13): drawn as text through CodeMirror, never `innerHTML`; images by
   magic bytes only; SVG and Markdown as source; size and line caps; visible markers for bidi and
   zero-width characters; file names drawn as text, direction characters marked.
7. **Hostile links in the transcript** (D14): a link opens only a read-only view, only inside the
   session's folders, through `files.locate`; nothing runs. Reveal takes `{root, path}` and the
   native side resolves it against roots it fetched itself, so no path from the webview reaches an
   opener (D11).
8. **A paired phone** (D8): no terminal, no files; tested.
9. **Resource exhaustion:** four terminals per session and sixteen per daemon; a 1 MiB ring per
   terminal; output coalesced to at most 64 KiB per notification and one notification per 16 ms
   per terminal; a connection more than 1 MiB behind is told it lagged and re-attaches from the
   ring instead of queuing without limit; input at most 64 KiB per call and 1 MiB waiting; sizes
   clamped to 1 to 1000 columns and 1 to 500 rows; `files.list` at most 2000 entries, `files.read`
   at most 1 MiB per call and 10 MiB for an image; a FIFO cannot block a read.
10. **Secrets at rest and in transit:** scrollback is in the daemon's memory only (D4) and the
    webview's (never written to its storage); terminal bytes are absent from the journal, logs,
    exports, search and telemetry (goal 4); on the wire they travel only on the desktop app's
    local-socket connection.
11. **The wrong daemon:** the relay opens terminals only on the daemon `daemon.json` names, and only
    when the window is connected to that same daemon (its URL is compared); a window connected to
    a remote daemon is told terminals are local only, so a user never types into a shell on a
    machine they did not mean.
12. **Processes left behind** (D5, goal 6): every terminal's session of processes is ended on
    close, session end, detach timeout and daemon stop; tested with a background job on the
    matrix.
13. **The app started with a debugging or module hook.** `WEBKIT_INSPECTOR_SERVER` (WebKitGTK's
    remote inspector) and `GTK_MODULES` (loads code into the app) would let another process drive
    the window that types into terminals; stage 3 adds both to `imperium_ipc::refused_variable`,
    which the plugin approval also uses (the stricter list helps both).

### Residual risks

- **Tier-0 commands reach the terminal device.** A command at isolation tier 0 runs as the user:
  on Unix it can open the terminal's `/dev/pts/<n>` and write to it (and, with `TIOCSTI` where the
  kernel still allows it, inject input); on Windows it can `AttachConsole` to the shell and
  `WriteConsoleInput`. No protocol gate stops that; tier 0 is documented as giving code the
  user's full rights. Stage 2b adds a matrix test that a command at the OS sandbox tier cannot open
  the terminal's pts device where that tier hides `/dev/pts` (bubblewrap, Seatbelt); if a tier
  does not hide it, the test records it as a finding for the sandbox owner instead of passing
  silently.
- **`may_grant` is not a boundary against an evading process** (D2): a same-user process that
  detaches from the daemon and clears its environment, then drives the desktop app's window, is
  not caught.

## Protocol additions

New `crates/imperium-protocol/src/methods/terminal.rs` and `methods/files.rs`. Each stage that adds
methods takes **the next protocol minor at its merge** (0.59 today; other pull requests will take
0.60 and 0.61 first) and updates `PROTOCOL_VERSION`, [02](../02-architecture.md#protocol),
`snapshots/methods.txt` and the generated types. Terminal bytes are base64 in JSON (the transport
is text frames only). Neither namespace is open to a paired phone or to a mod frame's bridge.

| Method or notification | Stage | Does |
| --- | --- | --- |
| `terminal.open` | 2b | `{session_id, cols, rows}` → `TerminalInfo`; the caller is attached |
| `terminal.list` | 2b | `{session_id}` → `{terminals: [TerminalInfo]}` |
| `terminal.attach` | 2b | `{terminal_id, since?}` → `{info, replay, seq, truncated}`: the ring from `since` (or all of it) and the caller attached |
| `terminal.detach` | 2b | `{terminal_id}`; the terminal keeps running |
| `terminal.input` | 2b | `{terminal_id, data}` (base64, at most 64 KiB) |
| `terminal.resize` | 2b | `{terminal_id, cols, rows}` (clamped); the last resize wins |
| `terminal.close` | 2b | `{terminal_id}`: ends its processes, removes the terminal |
| `terminal.output` (notification) | 2b | `{terminal_id, seq, data}` or `{terminal_id, lagged: true}`, to attached connections only |
| `terminal.changed` (notification) | 2b | `{info}` on title change, exit and close, to attached connections only |
| `TerminalInfo` | 2b | `{terminal_id, session_id, title, cwd, shell, cols, rows, state (running, exited), exit_code?, created_at}` |
| `files.roots` | 6 | `{session_id}` → `{roots: [{index, label, path, kind (folder, worktree, scratch)}]}` |
| `files.list` | 6 | `{session_id, root, dir, show_ignored?}` → `{entries: [{name, kind (file, dir, link, other), size?, link_outside?, ignored}], truncated}` |
| `files.read` | 6 | `{session_id, root, path, offset?, length?}` → `FileContent` |
| `files.locate` | 6 | `{session_id, path}` (absolute, or relative to the working folder) → `{root, path}`, or `-32602` with the reason |
| `FileContent` | 6 | `{kind (text, binary, image), size, modified_ms, etag, encoding?, text?, offset, next_offset?, note?, image?: {mime, data}}` |

Every `terminal.*` method is answered only to the identified desktop app (D2); otherwise
`-32005` with the reason. `files.*` is answered to any full client (the window's own loopback
connection included) and refused to a phone. Neither is offered to agents.

Config (user layer only, in the registry, Settings → Terminal):

- `terminal.enabled` (bool, default `true`): off refuses `terminal.open` and hides the panel.
- `terminal.shell` (list of strings: program and arguments; empty is the default of D6).

Not added: a scrollback, count or timeout setting (constants in D3 and D4); any environment-layer
or project key for either pane.

## Assumptions

- `portable-pty` (wezterm's crate) spawns through ConPTY on Windows 10 1809 and later and
  `openpty` on Linux and macOS, from a process with no console (the daemon the app starts is
  detached from any console). On an older Windows the pane says the terminal needs 1809 or later.
  Whether it lets a spawn pass a job list is checked in stage 1 (D5 says what happens if not).
- `cap-std` refuses escapes on all three OSes, junctions included on Windows; stage 6 tests that
  rather than trusting it.
- xterm.js 5 ignores OSC 52 without its clipboard add-on and keeps `windowOptions` off by default;
  stage 5 tests both in a browser.
- `tauri::test` can forge a request's origin for `get_ipc_response`, so the access-control test can
  run without a display; stage 3 confirms it against the pinned Tauri version.
- `@tauri-apps/api` 2.11's IPC can be replaced by an init script so `invoke` and `Channel`
  callbacks reach a fake (its `mocks` module, or a hand-written `__TAURI_INTERNALS__`); stage 4
  confirms which before building on it.
- Playwright's Chromium is close enough to WebView2, and its WebKit to WebKitGTK and WKWebView,
  for layout and behaviour checks; the Linux end-to-end run covers the real WebKitGTK.

## Relevant paths

`crates/imperium-pty/` (new), `crates/imperium-core/src/terminal.rs` and `files.rs` (new),
`crates/imperium-core/src/rpc.rs` and `rpc/` (dispatch and access),
`crates/imperium-core/src/app_identity/`, `crates/imperium-core/src/peer.rs`,
`crates/imperium-core/src/state/` (session end), `crates/imperium-core/tests/terminal.rs`,
`files.rs` and `phone.rs`, `crates/imperium-protocol/src/methods/`,
`crates/imperium-client/src/lib.rs`, `crates/imperium-ipc/src/loader_env.rs`,
`crates/imperium-config/src/settings.rs` and `registry.rs`, `apps/desktop/src-tauri/src/terminal.rs`
and `reveal.rs` (new), `main.rs`, `build.rs`, `capabilities/default.json`, `tauri.conf.json`,
`apps/desktop/src/terminal/` and `src/files/` (new), `src/panels/TerminalPanel.tsx` and
`FilesPanel.tsx` (new), `App.tsx`, `src/panels/ReviewPanel.tsx`, `src/sessions/` (transcript
links), `src/settings/shortcuts.ts`, `apps/desktop/e2e/` (new), `apps/desktop/test/`,
`scripts/check-web.sh`, `.github/workflows/ci.yml`, `scripts/ci/presence-e2e/`,
`scripts/ci/detect.sh`.

## Related docs

[07 desktop](../07-desktop-ui.md) (panels, layout, shortcuts; wins for built behaviour),
[02 protocol](../02-architecture.md#protocol), [05 tools and isolation](../05-tools-and-isolation.md)
(isolation tiers and permission rules, which the user's terminal is outside),
[06 extensibility](../06-extensibility.md) (mod frames, main-context mods, the desktop-app
identity), [15 mobile](../15-mobile-and-remote.md) (this plan wins for the file pane, D8),
[10 configuration](../10-configuration.md), [11 CI](../11-ci-cd.md), and the
[pull-request card plan](pr-card.md) and [plugin marketplace plan](plugin-marketplace.md) (same
stage format; the latter built the identity check D2 reuses). This plan wins for planned behaviour
until a stage merges and updates the lasting docs.

## Stages

Each stage is one pull request against `experimental`, with the docs it changes. Sizes are
estimates of changed lines, tests included; a stage that grows past about 800 lines splits.

### Stage 1: PTY library

- Objective: a small crate that runs a shell on a pseudo terminal on all three OSes, with no
  daemon wiring. `crates/imperium-pty` over `portable-pty`: `Pty::spawn(Spec)` (program,
  arguments, working folder, environment, columns and rows); a reader thread filling a ring buffer
  (1 MiB) addressed by a running byte offset (`seq`), with a subscriber callback; `write` through
  a writer thread with a bounded queue (1 MiB; full is an error, not a block of the caller);
  `resize`; `end` per D5 (Unix: by session with `sysinfo`, groups `SIGHUP` then `SIGKILL` after
  2 s, the Linux subreaper set by the caller; Windows: the Job Object, atomic where the spawn
  allows, the pseudo console closed on its own thread while the reader keeps draining, since
  `ClosePseudoConsole` can block until output is read); the exit code; and `resolve_shell` and
  `shell_env` per D6.
- Depends on: none.
- Owned paths: `crates/imperium-pty/**` (new), workspace `Cargo.toml`, `Cargo.lock`.
- Acceptance checks: `cargo test -p imperium-pty` on the matrix:
  - echo round trip (`contains` rather than equality because ConPTY adds start-up sequences);
  - the child sees the size it was given and a resize (`stty size` on Unix;
    `$Host.UI.RawUI.WindowSize` in Windows PowerShell); the working folder reaches the child;
  - **a background job dies:** an interactive `bash -i` (Linux, macOS) runs `sleep 60 &`, the
    test reads the job's pid, `end` runs, and the pid is gone; on Windows, `cmd` runs
    `start /b ping -n 60 127.0.0.1` and the process is gone; on Linux a child that calls `setsid`
    is gone too (subreaper);
  - the exit code is reported; a multi-byte character split across reads arrives whole; a 2 MiB
    burst keeps the last 1 MiB in the ring and reports the drop;
  - `resolve_shell` never returns a path in the working folder when `PATH` holds `.`, an empty
    entry or an absolute entry inside the session's folders and that folder holds a file named
    like the shell; on Unix it takes the account's shell over `$SHELL` and refuses one missing
    from `/etc/shells`; on Windows it ignores `ComSpec` and a `PATH` entry with `pwsh.exe`;
  - `shell_env` drops `ENV`, `BASH_ENV`, `ZDOTDIR`, `PROMPT_COMMAND`, `NODE_OPTIONS`, `LD_*`,
    `DYLD_*` and internal `IMPERIUM_*` variables and keeps the spawn mark;
  - every wait is bounded (10 s). `cargo clippy` clean.
- Size: about 800 lines.
- Risks: ConPTY differences between Windows builds (contained text and bounded waits);
  `portable-pty` uses `unsafe` internally (a dependency, allowed; the workspace's own crates stay
  `forbid(unsafe_code)`, so the subreaper call uses `nix`'s safe `prctl` wrapper and the
  `getpwuid` read `nix::unistd::User`).
- Status: pending
- Outcome: -
- Handoff notes: the crate knows nothing of sessions or protocol. Changing `Cargo.lock` runs the
  presence end-to-end job (detect lists it); expect it.

### Stage 2a: Terminal service

- Objective: the daemon owns terminals, with no protocol yet. A `TerminalService` in
  `crates/imperium-core/src/terminal.rs` holds terminals per session (four) and per daemon
  (sixteen), starts them with the session workspace's root and the D6 shell and environment,
  refuses sessions with a remote runtime or archived, and ends them on close, on the session's
  stop, archive and delete, after 60 minutes with no attachment, and at daemon shutdown (within
  the existing 5 s shutdown wait). The daemon becomes a child subreaper on Linux at start. Settings
  `terminal.enabled` and `terminal.shell` in the registry (user layer only; absent from the
  environment layer's allow-list). Terminal bytes are written nowhere but the ring and the
  subscribers (no `tracing` of data, only ids and sizes).
- Depends on: stage 1.
- Owned paths: `crates/imperium-core/src/terminal.rs` (new), the daemon start (subreaper), the
  session stop, archive and delete paths in `state/` (one call each),
  `crates/imperium-core/tests/terminal.rs` (new), `crates/imperium-config/src/settings.rs` and
  `registry.rs`, `docs/10-configuration.md`.
- Acceptance checks: `cargo test -p imperium-core --test terminal` on the matrix, calling the
  service in process: a terminal starts in the workspace root; the fifth in a session and the
  seventeenth in the daemon are refused; archive, delete, the detach timeout (on the daemon's test
  clock) and daemon shutdown each end a terminal and its background job; `terminal.shell` in an
  environment's `.imperium/config.toml` is ignored; a remote-runtime session is refused.
- Size: about 600 lines.
- Risks: shutdown ordering (terminals end before the runtime stops); the timer uses the daemon's
  clock abstraction so tests do not wait.
- Status: pending
- Outcome: -
- Handoff notes: no agent path is added (no tool, no `session.run_tool` entry, no hook event).

### Stage 2b: Terminal protocol and the gate

- Objective: `terminal.*` (protocol minor at merge) behind the D2 gate. On a connection's first
  `terminal.*` call the gate runs `InstalledApp` and `peer::may_grant` through the injectable
  `TerminalGate` and caches the verdict on `Connection`; a phone connection is refused by scope.
  Attachment is per connection; `terminal.output` and `terminal.changed` go on that connection's
  own sender, never through the broadcast path; a connection closing detaches it. Output is
  coalesced (64 KiB, 16 ms); a connection more than 1 MiB behind gets `lagged` and re-attaches
  with `since`.
- Depends on: stage 2a.
- Owned paths: `rpc.rs` and a new `rpc/terminal.rs` (dispatch, the cached verdict),
  `crates/imperium-core/src/terminal.rs` (attachments), `tests/terminal.rs`, `tests/phone.rs`,
  `crates/imperium-protocol/src/methods/terminal.rs` (new), protocol snapshots and generated
  types, `docs/02-architecture.md`, `docs/05-tools-and-isolation.md` (the residual tier-0 risk).
- Acceptance checks: `cargo test -p imperium-core --test terminal --test phone` on the matrix:
  - with a stub gate, open prints the workspace root and a resize reaches the shell;
  - refused over loopback TCP, for a phone (`-32005`), and with the stub saying no; the gate runs
    once per connection (a counting stub);
  - **a real child with the spawn mark is refused:** the test spawns a helper process with
    `IMPERIUM_SPAWNED_BY_DAEMON=1` that connects on the local socket with the real gate, and gets
    `-32005`;
  - output reaches only the attached connection (a second connection with
    `events.subscribe ["*"]` gets nothing);
  - **sentinel:** a sentinel typed into a terminal and echoed is absent from every journal file,
    every notification to a second connection, `session.export`, `session.search`, and the
    daemon's log captured at TRACE;
  - detach then attach with `since` replays exactly the missed bytes; a lagging connection gets
    `lagged`;
  - **sandbox tier:** a command run at the OS sandbox tier (where the job requires it,
    `IMPERIUM_REQUIRE_OS_SANDBOX=1`) cannot open the terminal's pts device; where the tier does
    not hide `/dev/pts`, the test prints a finding and is marked for the sandbox owner rather than
    passing silently;
  - protocol snapshot and type checks pass.
- Size: about 750 lines.
- Risks: per-connection notification plumbing touches the connection's sender (keep it to one
  new method on `Connection`).
- Status: pending
- Outcome: -
- Handoff notes: stage 3 is the only client. Touching `app_identity/` or `rpc/config.rs` runs the
  presence end-to-end job.

### Stage 3: The desktop app's relay

- Objective: the app's native side carries terminal traffic, so the webview never holds a
  terminal connection.
  - `imperium_client` gains a split mode (`Client::into_split`: a writer half for calls and a
    reader task that resolves responses by id and yields notifications) without changing `call`
    for existing users.
  - `src-tauri/src/terminal.rs`: one lazily opened local-socket connection to the daemon
    `daemon.json` names (reopened after a daemon restart; never loopback TCP; no lease), the
    start-up `Guard` reused, and the commands `terminal_open`, `terminal_attach`,
    `terminal_detach`, `terminal_input`, `terminal_resize`, `terminal_close` and `terminal_list`.
    Each takes the URL the window is connected to, refused unless it is `daemon.json`'s
    (threat 11), and is refused while a trusted main-context mod is loaded (D2). Output reaches
    each attached window through a Tauri `Channel`; a window that closes detaches. The relay logs
    ids and sizes, never data.
  - Permissions for the seven commands in `capabilities/default.json` for `main` and `panel-*`,
    the app's origin only.
  - `WEBKIT_INSPECTOR_SERVER` and `GTK_MODULES` join `imperium_ipc::refused_variable`.
- Depends on: stage 2b.
- Owned paths: `crates/imperium-client/src/lib.rs`, `crates/imperium-ipc/src/loader_env.rs`,
  `apps/desktop/src-tauri/src/terminal.rs` (new), `main.rs` (handlers), `build.rs` and
  `capabilities/default.json` (permissions), `docs/06-extensibility.md` (the refused variables),
  `docs/07-desktop-ui.md` (a "Terminal relay" note beside Plugin approval).
- Acceptance checks:
  - `cargo test -p imperium-client` (split mode: interleaved calls and notifications, a dropped
    connection fails pending calls);
  - `cargo test -p imperium-desktop terminal` on the matrix: **a `tauri::test` access-control
    test** builds the app with `mock_builder` and the real capability files and sends each
    `terminal_*` command through `get_ipc_response` from a null origin, an opaque (sandboxed
    frame) origin, `imperium-mod://`, `http://imperium-mod.localhost` and
    `http://localhost:3000`: all refused; from the app's own origin in `main`: accepted;
  - a window URL other than `daemon.json`'s is refused; with no local socket the commands say so
    and never fall back to TCP; routing sends output only to the attached window's channel (a fake
    daemon connection in process); a test log subscriber at TRACE sees no sentinel data;
  - `cargo test -p imperium-ipc` covers the two new refused variables.
- Size: about 750 lines.
- Risks: Tauri `Channel` ordering and back-pressure (sequence numbers from the daemon let the
  window detect a gap and re-attach); the relay must not block the Tauri main thread.
- Status: pending
- Outcome: -
- Handoff notes: the webview API is these seven commands; stage 5 wraps them in
  `src/terminal/bridge.ts`. Changing `main.rs`, `capabilities/` and `imperium-ipc` runs the
  presence end-to-end job.

### Stage 4: Browser-check harness

- Objective: the desktop UI can be driven in a browser against a fake daemon in CI, so the pane
  stages are checked without a person. `apps/desktop/e2e/`: a Node fake daemon (WebSocket JSON-RPC
  with `system.handshake`, sessions, `session.subscribe` history, and scriptable `files.*` and
  terminal fixtures; a fake terminal echoes input and plays scripted output), a Playwright config
  that serves the Vite build and runs Chromium and WebKit, and an init script that stands in for
  Tauri's IPC (`daemon_endpoint` answers the fake's URL and token; `terminal_*` go to the fake;
  `Channel` callbacks are delivered) through `@tauri-apps/api/mocks` if it supports channels in
  the pinned version, else a minimal `__TAURI_INTERNALS__`. A smoke test opens the app, shows a
  session and the Preview panel. `scripts/check-web.sh` runs the suite (`e2e` task) and the `web`
  job installs the browsers (`playwright install --with-deps chromium webkit`). The docs say the
  harness replaces the IPC and so checks no access rule (D16).
- Depends on: none (it may land before stage 1).
- Owned paths: `apps/desktop/e2e/**` (new), `apps/desktop/package.json` (dev dependency and `e2e`
  script), `pnpm-lock.yaml`, `scripts/check-web.sh`, the `web` job of `.github/workflows/ci.yml`,
  `docs/11-ci-cd.md`.
- Acceptance checks: the `web` job runs the suite and it passes in both browsers; the fake
  daemon's own unit test (`node --test`) checks its handshake and refusal of an unknown method;
  **positive control:** a test marked `test.fail()` asserts something the smoke page does not show
  and must fail, so the suite would notice a harness that passes everything.
- Size: about 700 lines.
- Risks: a fake daemon drifting from the real protocol (it imports the generated types from
  `@imperium/protocol` so a renamed field fails the typecheck); browser installs lengthen the
  `web` job (cache the browsers).
- Status: pending
- Outcome: -
- Handoff notes: stages 5, 7a, 7b and 8 add their scenarios here. Keep fixtures small and in
  `e2e/fixtures/`.

### Stage 5: Terminal panel

- Objective: the **Terminal** panel in the right-hand column (closed by default; opened from
  Settings → Panels, the command palette and **Toggle terminal**, Ctrl+Backquote by default,
  rebindable): a tab per terminal of the open session, *New terminal*, close, an exited
  terminal's code with *Restart*, and the empty states (no session, `terminal.enabled` off, a
  remote runtime, a remote daemon, a main-context mod loaded, Windows older than 1809, and the
  note that shells end about 10 seconds after the app closes unless the daemon is kept running).
  The app's close dialog says the same when terminals are running. xterm.js with the D9
  configuration and add-ons; theme from the app's tokens; `fit` on panel resize, sent as
  `terminal_resize`; re-attaching after a window reload replays the ring. `src/terminal/policy.ts`
  (links, titles, paste), `keys.ts` (the one input sender, D10, with F6 **Leave terminal** in the
  shortcuts registry), `bridge.ts` (the stage 3 commands). Settings → Terminal edits
  `terminal.enabled` and `terminal.shell`. Nothing terminal-related is written to `localStorage`,
  IndexedDB or the app store (`src-tauri/src/store.rs`, `src/shared/store.ts`).
- Depends on: stages 3 and 4.
- Owned paths: `apps/desktop/src/panels/TerminalPanel.tsx` (new), `src/terminal/**` (new),
  `App.tsx` (panel entry, `PANELS`, close dialog), `src/settings/shortcuts.ts`, a Settings section
  file, `src/styles/panels.css`, `apps/desktop/package.json` and `pnpm-lock.yaml` (`@xterm/*`),
  `apps/desktop/test/terminalPolicy.test.ts`, `terminalSender.test.ts` and `csp.test.ts` (new),
  `apps/desktop/e2e/terminal.spec.ts` (new), `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test` and `npm run typecheck` in `apps/desktop`:
  - policy tests: schemes allowed and refused, `file:` in and out of scope, a link whose text
    differs from its address, an IDN host shown as punycode, titles with control and bidi
    characters, bracketed paste with `ESC [201~`, multi-line paste without bracketed paste;
  - the source scan: only `keys.ts` names `terminal_input`; no file imports
    `@xterm/addon-clipboard` or `@xterm/addon-image`;
  - `csp.test.ts`: `tauri.conf.json`'s policy has no `unsafe-inline` or `unsafe-eval` in
    `script-src`, or in `default-src` while `script-src` is absent; `styleTokens.test.ts` still
    passes.
  - Browser checks against the fake: typing echoes; an OSC 52 sequence leaves a stubbed
    `navigator.clipboard` unwritten; an OSC 8 link opens only after the confirmation and calls the
    opener with the exact address; a title report request (`CSI 21 t`) sends nothing back; F6
    leaves the terminal; a reload re-attaches and shows earlier output; **after typing a sentinel,
    it is absent from `localStorage`, every IndexedDB database and the fake's record of app-store
    writes**.
- Size: about 800 lines.
- Risks: xterm.js in WebKitGTK (the DOM renderer is the default; WebGL is not used); the
  emulator's own replies are not "typed" text but must pass (the scan targets the command name,
  not the data).
- Status: pending
- Outcome: -
- Handoff notes: `policy.ts`'s link function is reused by stage 8 for `file:` links. Keep
  `TerminalPanel` free of daemon calls other than through `bridge.ts`.

### Stage 6: File access in the daemon

- Objective: `files.roots`, `files.list`, `files.read` and `files.locate` (protocol minor at
  merge) in `crates/imperium-core/src/files.rs`, under D12 and D13: root `Dir` handles opened once
  per session from `Workspace::roots()` and `Workspace::scratch()` (refused for a remote runtime or
  an archived session), the up-front spelling checks, every open through the held `Dir`,
  non-blocking with an `fstat` and regular files only, listings with `read_dir` and gitignore
  matchers (`show_ignored`) and link marks, at most 2000 entries, text reads of at most 1 MiB from
  an offset with UTF-8 handling, the binary rule, image sniffing up to 10 MiB, `etag` from size,
  modification time and a hash of the first 64 KiB, and `locate` for absolute or relative paths
  (refusing anything outside with the reason). Phone connections refused.
- Depends on: none (it may land in parallel with stages 1 to 5).
- Owned paths: `crates/imperium-core/src/files.rs` (new), `rpc.rs` and a new `rpc/files.rs`,
  `crates/imperium-core/tests/files.rs` (new), `tests/phone.rs`,
  `crates/imperium-protocol/src/methods/files.rs` (new), protocol snapshots and generated types,
  `crates/imperium-core/Cargo.toml` (`cap-std`), `Cargo.lock`, `docs/02-architecture.md`.
- Acceptance checks: `cargo test -p imperium-core --test files --test phone` on the matrix:
  - reads and lists inside work; `../`, an absolute path, a drive or UNC path, `\\?\`,
    `a.txt:stream`, `NUL.txt`, `con`, `CONIN$`, `COM¹`, `LPT³.log` and `nul.` (and `nul` with a trailing space) are refused;
  - a symbolic link to outside is refused to read and marked in the listing (Unix, and Windows
    where the runner may create links: otherwise a junction, which needs no privilege); a link
    inside to inside reads;
  - **race:** a thread swaps a directory for a link to outside in a loop while another reads, and
    never gets outside bytes; renaming a root's path after the session opened it changes nothing
    the service reads;
  - **special files:** a FIFO in the folder (Unix) is refused at once, without blocking;
  - **other sessions' worktrees:** a path in another session's worktree of the same repository is
    refused by `locate` and cannot be named as a root;
  - a 3 MiB file pages in 1 MiB reads; NUL bytes make it binary; UTF-16 with a BOM reads as text;
    a PNG renamed `.txt` is an image and a text file renamed `.png` is text; an SVG is text;
  - an ignored file is hidden unless `show_ignored`; the phone is refused; protocol checks pass.
- Size: about 800 lines.
- Risks: symbolic links on Windows runners need developer mode (use junctions there, and skip only
  the link case with a logged reason, never silently); `cap-std` behaviour on case-insensitive
  file systems (tested on macOS and Windows).
- Status: pending
- Outcome: -
- Handoff notes: `files.read` never applies agent deny rules (D12); the docs say so. Stages 7a and
  7b are the clients.

### Stage 7a: File viewer

- Objective: the **Files** panel's viewer in the right-hand column (closed by default; Settings →
  Panels, the palette, **Toggle files**), opened for now from quick open over `session.files` and
  an `openAt(root, path, line, column)` entry point: CodeMirror 6 read-only (D13) with line
  numbers, search and go to line; the large-file, binary, image and encoding states; bidi and
  zero-width markers; *Copy path* and *Copy relative path*. The open file is re-read on a
  `tool_result` of `write_file` or `edit_file` naming it, on window focus and on *Refresh*,
  skipped when the `etag` is unchanged. Follows the open session.
- Depends on: stages 4 and 6.
- Owned paths: `apps/desktop/src/panels/FilesPanel.tsx` (new), `src/files/viewer*` and
  `src/files/content.ts` (new), `App.tsx` (panel entry and `PANELS`), `src/settings/shortcuts.ts`,
  `src/styles/panels.css`, `apps/desktop/package.json` and `pnpm-lock.yaml` (`@codemirror/*`),
  `apps/desktop/test/filesView.test.ts` (new), `apps/desktop/e2e/files.spec.ts` (new),
  `docs/07-desktop-ui.md`.
- Acceptance checks: `npm test` and `npm run typecheck`: view-model tests for every
  `FileContent` kind, the caps and the markers. Browser checks against the fake: a file opens
  highlighted; `<script>` and `<img onerror>` in a file appear as text and run nothing (no dialog,
  no request to the fake's sentinel URL); a 5 MiB file shows the cap notice; a binary file shows
  the binary card; an image fixture shows as an image; quick open finds a file; an edit event
  refreshes the view.
- Size: about 650 lines.
- Risks: CodeMirror's bundle size (languages load on demand).
- Status: pending
- Outcome: -
- Handoff notes: `openAt` is the entry point stages 7b and 8 call.

### Stage 7b: File tree and reveal

- Objective: the tree per root (lazy `files.list`, *Show ignored*, link marks, file names drawn as
  text with direction characters marked), keyboard navigation between tree and viewer, and
  *Reveal in folder* (D11): a native command `reveal_file {session_id, root, path}` in
  `src-tauri/src/reveal.rs` that fetches the session's roots from the daemon itself, resolves the
  path under one, and calls `SHOpenFolderAndSelectItems` (or a quoted `explorer.exe /select,`),
  the `org.freedesktop.FileManager1.ShowItems` D-Bus call (else opening the parent folder only),
  or `open -R`.
- Depends on: stage 7a.
- Owned paths: `apps/desktop/src/files/tree*` (new), `FilesPanel.tsx`,
  `apps/desktop/src-tauri/src/reveal.rs` (new), `main.rs`, `build.rs` and
  `capabilities/default.json`, `apps/desktop/e2e/files.spec.ts`, `docs/07-desktop-ui.md`.
- Acceptance checks: browser checks: the tree lists, expands and opens files; a file named with a
  right-to-left override shows the marker; *Show ignored* reveals an ignored file; keyboard focus
  moves between tree and viewer. `cargo test -p imperium-desktop reveal` on the matrix, with the
  opener behind a recording trait: a path outside every root, a `..` path and a root index the
  daemon did not return are refused **before** the opener is called; for an accepted file the
  opener receives the folder-and-selection call (Windows, macOS) or the D-Bus call or the parent
  folder (Linux), and **no file path is ever passed to an opener as the thing to open**; the
  `tauri::test` access-control test of stage 3 covers `reveal_file` too.
- Size: about 600 lines.
- Risks: file managers without `FileManager1` on Linux (the parent-folder fallback).
- Status: pending
- Outcome: -
- Handoff notes: changing `main.rs` and `capabilities/` runs the presence end-to-end job.

### Stage 8: Links, review, end to end and docs

- Objective: the wiring between panes and the close-out.
  - Transcript links (D14) in `src/sessions/`: `path:line[:column]` in assistant text and inline
    code, and the path of file tool calls, resolved through `files.locate` and opened with
    `openAt`; Review rows get *Open file*, and the Files panel *Changed in this session* with
    *Show diff*; `file:` links in terminal output open the Files panel through `policy.ts`.
  - The Linux end-to-end run gains a `panes` scenario (release app and daemon): open a session,
    open a terminal through the real relay, type `echo` with a sentinel and read it on screen;
    open a file in the Files panel; then a **real mod frame and a Preview page** each try
    `window.__TAURI_INTERNALS__.invoke("terminal_open", …)` and a direct `fetch` to
    `ipc.localhost` for `terminal_open`, and both fail (no terminal is listed afterwards).
    `scripts/ci/detect.sh` adds the terminal and files paths to the job's watched list.
  - Docs: 02 (methods), 05 (the user's terminal is outside isolation tiers and permission rules;
    the tier-0 residual risk), 06 (mods get no terminal or files, cannot hold modules in either
    panel, and terminals are refused while a main-context mod is loaded), 07 (both panels), 10 (the
    settings), 15 (already updated by this plan's own pull request: the phone gets no terminal and no file
    pane), 11 (the browser checks and the scenario); roadmap item 55 set to
    implemented and this plan folded into the lasting docs.
- Depends on: stages 5 and 7b.
- Owned paths: `apps/desktop/src/sessions/` (link detection and rendering only),
  `src/panels/ReviewPanel.tsx`, `src/files/`, `src/terminal/policy.ts`,
  `apps/desktop/e2e/links.spec.ts` (new), `scripts/ci/presence-e2e/`, `scripts/ci/detect.sh` and
  `scripts/ci/test-detect.sh`, `docs/**`, `docs/12-roadmap.md`.
- Acceptance checks: browser checks: a transcript `src/lib.rs:12` link opens the file at line 12;
  a link outside the folders does not open and says why; a Review row opens the file at its first
  changed line; *Show diff* opens the Review panel at the file. The `presence-e2e` Linux job runs
  the `panes` scenario green, frame attempts included. Docs checks
  (`python3 scripts/ci/check-links.py .` and markdownlint) green; `cargo test --workspace` and
  `npm test` green.
- Size: about 750 lines.
- Risks: link detection false positives in prose (only paths that `files.locate` accepts become
  links, and detection runs on idle so long transcripts stay smooth).
- Status: pending
- Outcome: -
- Handoff notes: the end-to-end scenario is the one place the whole chain (webview, relay, gate,
  PTY) and real frames run together; keep it short so it stays reliable.

## Risks across stages

- **The gate is the design's load-bearing part.** It reuses the plugin approval's identity code;
  a change there affects both, and the presence end-to-end job runs on such changes.
- **ConPTY quirks** (start-up sequences, reflow on resize, blocking close): bounded waits,
  contained-text assertions and the close-on-its-own-thread rule in stage 1.
- **Webview differences** (WebView2, WebKitGTK, WKWebView): Playwright's Chromium and WebKit, plus
  the Linux real-app run; macOS and Windows real-app runs are not in CI, and on those two the
  frame refusals rest on the `tauri::test` access-control test.
- **Protocol minors.** Stages 2b and 6 take the next free minor at their merge; merge
  `experimental`, never rebase.
- **Users expect terminals to survive closing the app.** D3 ends them with the daemon; the pane's
  empty state and the close dialog say so.

## Open questions

These need the user; each has the default this plan uses until answered.

1. **Should a running terminal keep the daemon alive after the app closes** (count as daemon work,
   like a running turn)? Default: no (D3); a long build in the pane ends when the app closes
   unless the daemon is kept running. The pane's empty state, and the app's close dialog when
   terminals are running, say that shells end about 10 seconds after the app closes.
2. **Should a paired phone ever see terminal output read-only?** Default: no, and no file pane
   either (D8).
3. **Should the file pane become an editor in a later item?** Default: read-only in this plan
   (D11).
4. **Terminals and the file pane for remote runtimes** (container, WSL, SSH: `docker exec -it`,
   `wsl.exe`, `ssh -t` inside the same PTY): a follow-up roadmap item, or out of scope? Default:
   out of scope here; the pane says why.
