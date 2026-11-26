# 11 — CI/CD and releases

All workflows live in `.github/workflows/`. Logic lives in `scripts/ci/`, so the same commands
run locally and in CI. Third-party actions are pinned to full commit SHAs; Dependabot proposes
updates.

## Workflows

| Workflow            | Trigger                                              | Purpose                                       |
| ------------------- | ---------------------------------------------------- | --------------------------------------------- |
| `ci.yml`            | Pull requests, pushes to `main` and `experimental`, merge queue | Required checks; single gate job `CI gate` |
| `build.yml`         | Called by release workflows only                     | Builds desktop bundles and CLI archives       |
| `release.yml`       | Tags `v*` (for example `v0.4.0`, `v0.5.0-beta.1`)    | Main-channel GitHub Release                   |
| `experimental.yml`  | Pushes to `experimental`, manual run on it           | Experimental-channel GitHub pre-release       |
| `presence-e2e.yml`  | Called by `ci.yml` when plugin approval changes, manual run | Plugin approval end to end with the OS presence check |
| `advisories.yml`    | Weekly, manual run, pull requests that change it | `cargo deny check advisories` over the whole lock file, and an issue for each new GitHub security advisory on a Monty crate |
| `github-live.yml`   | Pushes to `experimental`, weekly, manual run, same-repository pull requests that change it | Read-only check of the GitHub client against the real API (informational) |

### Checks (`ci.yml`)

Jobs run only when the code they check exists (detected by `scripts/ci/detect.sh`), and the
always-running `CI gate` job fails if any job failed, so it can be the single required check.

| Job         | Runs when            | Does                                                          |
| ----------- | -------------------- | ------------------------------------------------------------- |
| `docs`      | Always               | markdownlint, relative-link check                             |
| `workflows` | Always               | actionlint                                                    |
| `rust`      | `Cargo.toml` exists  | fmt, clippy (`-D warnings`), tests on Linux, Windows, macOS   |
| `deny`      | `Cargo.toml` exists  | `cargo deny check` (advisories, licences, sources; `deny.toml`) and the Monty pin check (`scripts/ci/check-monty-pin.sh`) |
| `script-worker` | `Cargo.toml` exists | The script worker and its driver: clippy and tests on Linux x86-64, Linux aarch64, macOS and Windows (on Windows through the daemon's confined launch, with the folder labels), then a release build that logs cold start, binary size and resident memory (`MEASURE` lines in the step summary) |
| `web`       | `package.json` exists | pnpm install (frozen), lint, typecheck, test, build          |
| `python`    | `sidecars/media/pyproject.toml` exists | uv sync (locked), ruff, pytest              |
| `python-cas` | `sidecars/cas/pyproject.toml` exists | uv sync (locked, no build), ruff check and format, pytest on Linux, Windows, macOS |

On Linux the `rust` job's tests also exercise how the daemon identifies the desktop app for plugin
approvals ([06](06-extensibility.md#plugin-marketplaces)): pidfds from the socket (`SO_PEERPIDFD`,
kernel 6.5 or later; the runner's kernel is newer) and the namespace check, whose test makes a
`sleep` in another mount and pid namespace with `unshare`, unprivileged where user namespaces are
allowed, else through the runner's passwordless `sudo`. Where neither works that test fails on CI
(the `CI` variable is set) and prints a `skipped:` line elsewhere. On Windows they exercise the
held process handle (an exited peer, a process created after the accept, the image path) and
U7: a test build without the compile-time `IMPERIUM_SIGNED_BUILD` constant counts nothing, and
a child test run with that variable set, a file of that name beside the binary and
`ProgramFiles` pointed at its folder counts nothing either. No workflow sets the constant yet:
it belongs to the Windows Authenticode signing step of roadmap item 59, which does not exist.
On macOS they connect a `python3` child over a Unix socket and check its audit token: the code
path, `WEBVIEW2_*` and `DYLD_*` variables in its environment, an exited peer, and a peer started under
`sandbox-exec`; a missing `python3` or `sandbox-exec` fails on CI and skips elsewhere.

The presence check's unit tests run in the same job: an injected provider's answers (passed is
`counted`, unavailable `dialog-only`, refused nothing), a presence field a client adds to
`plugins.approve` being dropped, polkit's answers mapped, and on Linux a real polkit query for an
action that is not installed, which must be unavailable. On macOS and Windows the real provider
runs only on CI (the `CI` variable), since elsewhere it would prompt whoever runs the tests.

### Presence end-to-end (`presence-e2e.yml`)

`ci.yml` calls it as its `presence-e2e` job when the change touches any of: in `imperium-core`,
`app_identity/`, `plugin_records.rs`, `rpc/config.rs`, `server.rs` and `plugin_install.rs`;
`imperium-extensions`' `plugins.rs`; `imperium-ipc`; the app's `plugin_approval.rs`, `main.rs`,
`capabilities/`, `tauri.conf.json` and `linux/` (the polkit action); `scripts/ci/presence-e2e/`,
`scripts/ci/detect.sh` and `scripts/ci/install-linux-deps.sh`; `presence-e2e.yml` and `ci.yml`; and
`Cargo.lock`. `scripts/ci/detect.sh` compares the change with its base (the pull request's base,
the merge group's base or the pushed-over commit) without rename detection, so moving a watched
file lists its old path and still runs the test, and it runs the test when the base cannot be
read. `scripts/ci/test-detect.sh`, run by the `detect` job first, checks that on a scratch
repository: a rename of a watched file, an edit of one and an unknown base run it; an unrelated
edit does not. The `CI gate` job needs it, so it **gates merges** whenever it runs;
otherwise it is skipped and the gate passes. It can also be run by hand. It runs only then because
its Linux job builds the release daemon, CLI and desktop app.

- **Linux** (`ubuntu-24.04`): `scripts/ci/presence-e2e/build.sh` builds the three release
  binaries into `target/release` (release, since only a release daemon records `counted` and
  `dialog-only`; a debug one records `dev`), and `run.sh` sets up what a desktop has: Xvfb, a
  session bus with the accessibility bus, polkitd with the action file installed, and a throwaway
  password for the runner's user. `drive.py` then starts the daemon on a scratch home, adds a
  folder marketplace, and drives the app through `tauri-driver` (WebKit's WebDriver), calling the
  app's own `plugin_approve` from its main window. It answers the native dialog by pressing its
  buttons through AT-SPI, reads the dialog's text, and answers polkit through `pkttyagent`
  registered for the daemon's process, in a terminal of its own. Scenarios: approve with polkit
  passing (`counted`; the dialog names the plugin and shows the hook and hash, and polkit's
  prompt is the action's message); *Cancel* on page 2 of a
  plugin with 91 hooks (pages titled "page i of n", nothing installed); polkit refusing a wrong
  password (installed, withheld, nothing recorded); no authentication agent (`dialog-only`); the
  action file removed while an agent runs (`dialog-only`, polkit never asked); and the app started
  with `LD_PRELOAD` (refused with its name, no dialog, nothing sent); and the **Plugins window's
  trust UI**, driven in the same app session through WebDriver (clicks and reads of the page's own
  elements): open the window, the plugin's detail and trust view (the hook and the hash listed),
  *Approve and install* (the native dialog, polkit, `counted`), off and on for the environment (the
  user's view unchanged) and for the user, an update from the marketplace shown in the trust view and
  taken with *Update without approving* (withheld, not approved), *Approve* from the row (`counted`
  again), uninstall and *Undo*. Each one checks the command's answer or the page, and `imperium
  plugin list`. The logs (daemon, CLI, agents, tauri-driver) are
  kept as the `presence-e2e-logs` artifact. With `live` the job runs the
  [live scenario](#live-official-marketplace-presence-liveyml) alone.
- **macOS and Windows** (`headless`): runs `imperium-ipc`'s presence tests with their output
  shown, so the log records what LocalAuthentication and Windows Hello answer on a runner with
  nobody at it; neither may pass, and Windows must say Hello is unavailable.

### Live official marketplace (`presence-live.yml`)

The scenarios above use a folder marketplace. This one uses the real
`anthropics/claude-plugins-official`, the built-in default. It calls `presence-e2e.yml` with
`live: true`, which runs the Linux job (the macOS and Windows jobs are skipped) with
`E2E_LIVE=1`, so `drive.py` runs its `live()` scenario instead of the folder ones, on a fresh
home:

1. The official marketplace is listed as built in and read from nothing (no download yet).
2. `imperium plugin marketplace refresh` clones it. **U1 check:** the clone's origin is
   `https://github.com/anthropics/claude-plugins-official`, it holds
   `.claude-plugin/marketplace.json` whose `name` is `claude-plugins-official`, the daemon's
   commit is the clone's `HEAD` and its plugin count is the manifest's. The commit is recorded
   (the remote's `HEAD` is logged beside it, since it may move during the run).
3. `imperium plugin browse` lists it; the live source types are counted and logged (what the
   parser leaves out is a follow-up in the [plan](plans/plugin-marketplace.md#stage-11-agents-docs-and-close-out)).
4. The first of `explanatory-output-style` and `learning-output-style` (small plugins that
   live in the marketplace's own repository and run one hook; `E2E_LIVE_PLUGINS` overrides the
   list) that is still offered with a hook is inspected: the output must show the marketplace's
   commit and a content hash.
5. It is installed and approved through the app's native dialog (which must show that hash)
   and polkit, as in the folder scenarios, and `imperium plugin list` must show it on with
   `origin: marketplace; approval: counted` and the marketplace's commit.

The evidence (the marketplace's commit, plugin, content hash, approval kind and the source
type counts) is written to `live-evidence.json` in the `presence-e2e-logs` artifact and to
the job summary.

**First run** (2026-11-24, the pull request that added it, Actions run 45348457592): the
marketplace was at commit `5cec6716767283f4675a05f5f8855b5b49c81783` (the remote's `HEAD` at the
end of the run was the same), 317 plugins; `explanatory-output-style@claude-plugins-official`
(content hash `39ad85b735b90390d00c45390dccf68ad64bfd98a8531366ed46ec03c9041a62`) was inspected,
installed and approved through the native dialog and polkit, and `imperium plugin list` showed it
`origin: marketplace; approval: counted` at that commit. Source types counted: 55 `path`, 159
`url`, 4 `url` with a `path`, 98 `git-subdir`, 1 `command`.

**It does not gate merges.** It needs the network and a repository this one does not control, so
a failure can be the marketplace's (a renamed plugin, a moved repository, GitHub being slow),
not a regression. It is not called by `ci.yml`; it runs on a weekly schedule (Monday 03:17 UTC),
by hand (`workflow_dispatch`), and on a pull request only when the harness or the workflow
itself changes, where it shows as its own check. A scheduled failure is a prompt to look at the
marketplace, not at a pull request.

### GitHub live check (`github-live.yml`)

The GitHub client in `imperium-github` is tested in CI against a fake server
(`imperium-github-fake`), which only proves the client agrees with the fake. This job runs
`crates/imperium-github/tests/live.rs` (`cargo test -p imperium-github --test live -- --ignored`)
against the real API with the workflow's `GITHUB_TOKEN`, to catch GraphQL documents or REST
shapes GitHub does not accept. It asserts, on `Densetsu152637/Imperium-AI`:

- `pr_status` on merged pull request #266: state `MERGED`, a 40-hex head SHA, a node id,
  checks that parse, known `mergeable` and `reviewDecision` values, and that the `rateLimit`
  cost and remaining points were parsed.
- `review_threads` on fixture pull request #271, which is closed, never merged, and keeps its
  branch `fixture/review-thread`: one change to a scratch doc with one review comment. The test
  fails (it does not skip) if the pull request has no threads, so do not delete or edit it.
- `repo_merge_settings` equals the repository's real flags, pinned in the test (squash, merge
  commit and rebase on, auto-merge off; update the pin if the repository settings change). A
  token without push access is not sent the flags (the first run found this); the client
  reports that as an error, and the check accepts it as a pass with a note.
- `collaborator_permission` for the owner: write access (the test prints `BRANCH=ok`), or, if the
  workflow token is refused, a 403 carrying GitHub's "Resource not accessible by integration"
  (`BRANCH=403`); any other refusal fails. The branch that ran is in the job summary.
- The mutations `EnableAutoMerge`, `DisableAutoMerge` and `ReplyToThread`, sent with bogus ids
  so nothing changes: each must fail as "could not resolve" or "not accessible", not as a
  parse or validation error.
- `failing_prs` (`Viewer` then `SearchPrs`): the login resolves and the search parses. The
  workflow token (an installation token) has no user `viewer` (GitHub answers with the app, or
  "not accessible"), so there the test accepts that clean refusal and prints `VIEWER=refused`; with the personal token it must
  print `VIEWER=ok`.
- On every error, the token does not appear.

**Optional personal token.** The workflow token cannot read the merge flags or the owner's
permission. If the repository secret `LIVE_READ_PAT` exists, pushes, the weekly run and
`workflow_dispatch` use it instead (never a pull request), and then the owner must be able to
write and the merge flags must be readable and equal the pinned ones. It may be a fine-grained
personal access token limited to this repository with Metadata: read and Pull requests: read;
creating it is the maintainer's choice, nothing in the repository does so.

Permissions are `contents: read` and `pull-requests: read`. It never runs for a pull request
from a fork (the job's `if` checks the head repository) and has no `pull_request_target`
trigger. The job is `continue-on-error: true`, so a failure shows but does not fail the run;
remove that line after it has passed twice. A failure is a prompt to look at GitHub's schema or
the pinned values, not at a pull request.

## Release channels

There are two channels, each with its own branch, workflow, and update feed.

| Channel      | Source                              | Version                      | Release                                                        |
| ------------ | ----------------------------------- | ---------------------------- | -------------------------------------------------------------- |
| Main         | Tag `vX.Y.Z` on `main`              | `X.Y.Z`                      | `vX.Y.Z (main)`, marked latest                                 |
| Main (pre)   | Tag `vX.Y.Z-beta.N` etc.            | `X.Y.Z-beta.N`               | `vX.Y.Z-beta.N (main)` pre-release; not offered by the updater |
| Experimental | Each push to the `experimental` branch | `X.Y.<run>-experimental` | `vX.Y.<run>-experimental` pre-release (for example `v0.2.9-experimental`) |

Each channel keeps one release. After a new release is published,
`scripts/ci/prune-releases.sh` deletes the channel's older releases and their tags: a new
experimental build replaces the last one, a stable main release replaces every other main
release, and a main pre-release replaces only the last pre-release (so the stable release the
main update feed points at stays). Deleting after publishing means the feed never points at
nothing. Runtime-pack releases are never touched.

`X.Y.Z` is the version in `apps/desktop/src-tauri/tauri.conf.json`. Experimental builds keep its
`X.Y` and use the experimental workflow's run number as the patch, so every experimental build
is newer than the last. `release.yml` ignores `-experimental` tags.

The version is **0.2.0**, set when the last roadmap item (the final cleanup, item 46) was merged;
experimental builds are `0.2.<run>-experimental`. It is declared in the Cargo workspace
(`Cargo.toml`, which every crate and the Tauri shell inherit, and the path dependencies'
versions), `apps/desktop/package.json` and `apps/desktop/src-tauri/tauri.conf.json`, and the
desktop and phone clients send it in their handshake. `PROTOCOL_VERSION` is separate and moves
with the protocol ([02](02-architecture.md)); the mods, `packages/protocol` and `packages/mod-sdk` keep their own versions.

### Desktop updates

The desktop app updates itself. It checks on a schedule, downloads a newer build in the
background, and installs it when the app quits; the title bar offers **Restart to update** to
install it at once. **Settings → Updates** (also opened by the version button in the title bar)
shows the installed version, when it last checked and when it checks next, and has two buttons:
**Check for updates** (checks now; a newer build downloads and waits for a restart) and **Check
and install** (checks now and, when a newer build exists, downloads it, installs it, and
restarts). It also chooses the channel (main or experimental) and the schedule:

- **Check automatically:** every day (the default), every 7 days, or every 28 days. The interval
  runs from the last *successful* check, whose time is saved (`update.json` in the app's config
  folder), so it holds across restarts: an app opened after the interval has passed checks about
  20 seconds after it starts. A failed check is retried after an hour. Waits are measured against
  the clock in steps of at most 15 minutes, so time the computer spent asleep counts.
- **Check for updates when the computer starts:** registers Imperium to start when the user signs
  in, with `--background-update`. Started that way the app opens no window and does not start
  the daemon: it checks the channel once, installs a newer build if there is one (stopping the
  bundled daemon first, and without starting the new version), and exits. The login entry is
  made with [`tauri-plugin-autostart`](https://crates.io/crates/tauri-plugin-autostart) (2.5,
  for Tauri 2): a per-user `Run` registry value on Windows, a LaunchAgent on macOS, an XDG
  autostart entry on Linux, the same mechanisms `imperium daemon autostart` uses. It is named
  "Imperium update check", so it never replaces the `Imperium` entry that command makes for the
  daemon. Development builds neither register nor check on their own.

Both are app settings, kept in the interface's `settings` document (`updateIntervalDays`,
`checkUpdatesAtLogin`); the interface passes changes to the native side
(`update_set_schedule`), and the app reads the document again at every start, before any
window opens, and adds or removes the login entry to match.

**Channels and switching.** A build belongs to the channel its version names (a `-experimental`
suffix means experimental, anything else main), so the installed build's channel is known after
a restart without being stored; the chosen channel (`channel` in `update.json`) defaults to it.
Within its own channel the updater never downgrades: only a higher version is offered. After the
user picks the other channel, the next check targets that channel's newest release even when its
version number is lower than the installed one (a main build `0.2.0` is lower than
`0.2.163-experimental`), in both directions. That build is offered, not downloaded: Settings →
Updates says it will install the *channel* build *version* (and that it is an older version
number when it is) and asks for confirmation (`update_switch_install`). Nothing is installed on
quit or by a login check while a switch waits. Once the switch is installed the running build
carries the new channel's version, the chosen and installed channels match again, and the
no-downgrade rule resumes. When the chosen channel has no release (or its release has no
`latest.json`) the status says so. The signature check and the feed handling are the same for a
switch.

Each release carries `latest.json`, written by `scripts/ci/updater-manifest.sh` from the signed
installers: the AppImage on Linux, the NSIS installer on Windows, and the `.app.tar.gz` on
macOS (deb, rpm, and MSI installs do not self-update). The app finds its channel's newest
release through the GitHub releases API and checks the installer's signature against the
public key in `tauri.conf.json`. The matching private key is the `TAURI_SIGNING_PRIVATE_KEY`
secret; a build without it publishes no `latest.json`, and installs simply see no update.
Losing the key means installed apps can no longer update, so keep a copy outside GitHub.

## Platform bundles

| Target            | Runner             | Desktop bundles                | CLI archive                            |
| ----------------- | ------------------ | ------------------------------ | -------------------------------------- |
| Linux x86_64      | `ubuntu-22.04`     | AppImage, deb, rpm             | `imperium-linux-x64.tar.gz`            |
| Linux aarch64     | `ubuntu-22.04-arm` | AppImage, deb, rpm             | `imperium-linux-arm64.tar.gz`          |
| Windows x86_64    | `windows-latest`   | MSI, NSIS (`.exe`)             | `imperium-windows-x64.tar.gz`          |
| Windows arm64     | `windows-11-arm`   | NSIS                           | `imperium-windows-arm64.tar.gz`        |
| macOS universal   | `macos-latest`     | `.app`, DMG                    | `imperium-macos-universal.tar.gz`      |

- Each desktop bundle is a complete install: `build-desktop.sh` builds `imperium-core`, the
  `imperium` CLI and code mode's script worker `imperium-script` (which the daemon finds beside
  itself; [05](05-tools-and-isolation.md#programs-run_script-the-interpreter-worker-implemented-in-part))
  for the target and ships them beside the app (Tauri `externalBin`; the app's
  own executable is `imperium-desktop` so the CLI's name stays free), with the bundled
  `policy/` workflow pack and `pets/` as resources. When no daemon answers, the app starts the
  bundled one in the background with `IMPERIUM_RESOURCE_DIR` pointing at those resources and
  its log in the app's log folder; it keeps running after the window closes. Before an update
  installs, the app stops that daemon (only if it is the bundled one), and the Windows
  installer's hooks (`src-tauri/windows/hooks.nsh`) stop it before replacing or removing files.
  An update over an existing install also replaces the bundled `mods`, `pets` and `policy` folders
  whole and removes the pre-rename `imperiumd.exe`, so nothing an older version shipped lingers;
  the user's settings and data are left alone.
  On Windows the CLI is in the install folder; on Linux the deb puts it on `PATH`.
- Linux builds use Ubuntu 22.04 for an older glibc baseline.
- The deb and rpm install polkit's action for plugin approvals' presence check
  (`/usr/share/polkit-1/actions/org.imperium.plugins.approve.policy`, from
  `apps/desktop/src-tauri/linux/`); `scripts/ci/check-linux-packages.sh` fails the Linux bundle job
  if a built package lacks it or carries another copy. The AppImage cannot install it, so its
  approvals are `dialog-only`.
- MSI and RPM reject the hyphen in pre-release versions, so pre-release and experimental builds
  ship NSIS on Windows and AppImage and deb on Linux.
- Every release includes `SHA256SUMS`.

## Runtime packs

Desktop bundles do not include inference runtimes. The app installs llama.cpp's own release
build for the ref pinned in `runtimes/llama-cpp.env` from
[ggml-org/llama.cpp](https://github.com/ggml-org/llama.cpp/releases), verified against the
SHA-256 digests GitHub publishes for each asset
([04](04-models-and-routing.md#runtimes)); raising `LLAMA_CPP_REF` is all a llama.cpp update
needs on the release side, once the new ref's builds are checked locally.

There is no runtime-pack pipeline of our own: an earlier `runtime-packs.yml` never built
Windows CUDA or macOS packs and was removed once the app switched to llama.cpp's own builds.

vLLM is never packaged (see [04](04-models-and-routing.md#runtimes)).

## Mobile (later)

When `apps/mobile` exists, `build.yml` gains Android (APK/AAB) and iOS (TestFlight) jobs.
Signing material is stored as repository secrets.

## Secrets

Optional; builds succeed unsigned when they are absent (without the updater key, releases have
no update feed).

| Secret                                                    | Used for                       |
| --------------------------------------------------------- | ------------------------------ |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Updater signatures     |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | macOS code signing |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`             | macOS notarisation             |
| Windows signing (Azure Trusted Signing, to be decided)    | Windows code signing           |

## Repository settings

`main` is protected (applied 2026-11-07):

- Changes land through pull requests; direct pushes are rejected, including for admins.
- The `CI gate` check must pass. Branches need not be up to date with `main` before merging.
- Linear history; no force pushes or deletion; review conversations must be resolved.
- GitHub requires no approving review, because the sole maintainer cannot approve their own
  pull requests. The maintainer's explicit go-ahead is the approval, and the maintainer merges
  or authorises the merge.

`experimental` is the default branch (since 2026-11-08), so pull requests target it; every
merge into it publishes an experimental release. `main` only takes `experimental`: the
`source branch` job (part of `CI gate`) fails any pull request into `main` from another branch.
Promoting is a pull request from `experimental` into `main`, then a `vX.Y.Z` tag on `main` for a
main release.
