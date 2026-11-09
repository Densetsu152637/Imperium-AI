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
| `runtime-packs.yml` | Changes to `runtimes/`, manual                       | llama.cpp runtime packs per hardware backend  |

### Checks (`ci.yml`)

Jobs run only when the code they check exists (detected by `scripts/ci/detect.sh`), and the
always-running `CI gate` job fails if any job failed, so it can be the single required check.

| Job         | Runs when            | Does                                                          |
| ----------- | -------------------- | ------------------------------------------------------------- |
| `docs`      | Always               | markdownlint, relative-link check                             |
| `workflows` | Always               | actionlint                                                    |
| `rust`      | `Cargo.toml` exists  | fmt, clippy (`-D warnings`), tests on Linux, Windows, macOS   |
| `web`       | `package.json` exists | pnpm install (frozen), lint, typecheck, test, build          |
| `python`    | `sidecars/media/pyproject.toml` exists | uv sync (locked), ruff, pytest              |

## Release channels

There are two channels, each with its own branch, workflow, and update feed.

| Channel      | Source                              | Version                      | Release                                                        |
| ------------ | ----------------------------------- | ---------------------------- | -------------------------------------------------------------- |
| Main         | Tag `vX.Y.Z` on `main`              | `X.Y.Z`                      | `vX.Y.Z (main)`, marked latest                                 |
| Main (pre)   | Tag `vX.Y.Z-beta.N` etc.            | `X.Y.Z-beta.N`               | `vX.Y.Z-beta.N (main)` pre-release; not offered by the updater |
| Experimental | Each push to the `experimental` branch | `X.Y.<run>-experimental` | `vX.Y.<run>-experimental` pre-release (for example `v0.1.9-experimental`) |

Each channel keeps one release. After a new release is published,
`scripts/ci/prune-releases.sh` deletes the channel's older releases and their tags: a new
experimental build replaces the last one, a stable main release replaces every other main
release, and a main pre-release replaces only the last pre-release (so the stable release the
main update feed points at stays). Deleting after publishing means the feed never points at
nothing. Runtime-pack releases are never touched.

`X.Y.Z` is the version in `apps/desktop/src-tauri/tauri.conf.json`. Experimental builds keep its
`X.Y` and use the experimental workflow's run number as the patch, so every experimental build
is newer than the last. `release.yml` ignores `-experimental` tags.

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
window opens, and adds or removes the login entry to match. A build follows the channel it was released on
until the user changes it; switching from experimental to main moves to the next main release newer than the
installed build (the updater never downgrades).

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

- Each desktop bundle is a complete install: `build-desktop.sh` builds `imperium-core` and the
  `imperium` CLI for the target and ships them beside the app (Tauri `externalBin`; the app's
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
- MSI and RPM reject the hyphen in pre-release versions, so pre-release and experimental builds
  ship NSIS on Windows and AppImage and deb on Linux.
- Every release includes `SHA256SUMS`.

## Runtime packs

Desktop bundles do not include inference runtimes. `runtime-packs.yml` builds `llama-server`
and its libraries from the llama.cpp ref pinned in `runtimes/llama-cpp.env`, one pack per
platform and backend, and publishes them to a `runtime-llama.cpp-<ref>` release. The daemon
downloads the right pack on demand and verifies its checksum.

| Pack                | OS / arch       | Backend                         |
| ------------------- | --------------- | ------------------------------- |
| `linux-x64-cpu`     | Linux x86_64    | CPU                             |
| `linux-x64-vulkan`  | Linux x86_64    | Vulkan (NVIDIA, AMD, Intel)     |
| `linux-x64-cuda12`  | Linux x86_64    | CUDA 12.9 (Pascal and newer)    |
| `linux-x64-cuda13`  | Linux x86_64    | CUDA 13 (Turing and newer)      |
| `linux-x64-rocm`    | Linux x86_64    | ROCm / HIP (RDNA2+, CDNA)       |
| `linux-arm64-cpu`   | Linux aarch64   | CPU                             |
| `windows-x64-cpu`   | Windows x86_64  | CPU                             |
| `windows-x64-vulkan`| Windows x86_64  | Vulkan                          |
| `windows-x64-cuda12`| Windows x86_64  | CUDA 12.9                       |
| `windows-x64-cuda13`| Windows x86_64  | CUDA 13                         |
| `windows-arm64-cpu` | Windows arm64   | CPU                             |
| `macos-arm64-metal` | macOS arm64     | Metal                           |
| `macos-x64-cpu`     | macOS x86_64    | CPU                             |

Each pack contains a `manifest.json` (pack id, backend, OS, arch, llama.cpp ref, build
flags) and, for CUDA packs, the redistributable CUDA runtime libraries. vLLM is never packaged
(see [04](04-models-and-routing.md#runtimes)).

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
