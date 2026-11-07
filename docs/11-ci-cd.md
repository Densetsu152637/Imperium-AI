# 11 — CI/CD and releases

All workflows live in `.github/workflows/`. Logic lives in `scripts/ci/`, so the same commands
run locally and in CI. Third-party actions are pinned to full commit SHAs; Dependabot proposes
updates.

## Workflows

| Workflow            | Trigger                                              | Purpose                                       |
| ------------------- | ---------------------------------------------------- | --------------------------------------------- |
| `ci.yml`            | Pull requests, pushes to `main`, merge queue         | Required checks; single gate job `CI gate`    |
| `build.yml`         | Called by release workflows only                     | Builds desktop bundles and CLI archives       |
| `release.yml`       | Tags `v*` (for example `v0.4.0`, `v0.5.0-beta.1`)    | Stable or pre-release GitHub Release          |
| `experimental.yml`  | Nightly, pushes to `experimental`, manual            | Rolling `experimental` pre-release            |
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

| Channel      | Source                     | Version                                   | Release                     |
| ------------ | -------------------------- | ----------------------------------------- | --------------------------- |
| Stable       | Tag `vX.Y.Z`               | `X.Y.Z`                                   | GitHub Release, marked latest |
| Pre-release  | Tag `vX.Y.Z-beta.N` etc.   | `X.Y.Z-beta.N`                            | GitHub pre-release          |
| Experimental | `experimental` branch or nightly `main` | `X.Y.Z-experimental.<date>.<sha>` | Rolling `experimental` pre-release, replaced on each build |

The desktop updater has a matching channel setting.

## Platform bundles

| Target            | Runner             | Desktop bundles                | CLI archive                            |
| ----------------- | ------------------ | ------------------------------ | -------------------------------------- |
| Linux x86_64      | `ubuntu-22.04`     | AppImage, deb, rpm             | `imperium-linux-x64.tar.gz`            |
| Linux aarch64     | `ubuntu-22.04-arm` | AppImage, deb, rpm             | `imperium-linux-arm64.tar.gz`          |
| Windows x86_64    | `windows-latest`   | MSI, NSIS (`.exe`)             | `imperium-windows-x64.tar.gz`          |
| Windows arm64     | `windows-11-arm`   | NSIS                           | `imperium-windows-arm64.tar.gz`        |
| macOS universal   | `macos-latest`     | `.app`, DMG                    | `imperium-macos-universal.tar.gz`      |

- Linux builds use Ubuntu 22.04 for an older glibc baseline.
- MSI requires numeric versions, so pre-release and experimental Windows builds ship NSIS only.
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

Optional; builds succeed unsigned when they are absent.

| Secret                                                    | Used for                       |
| --------------------------------------------------------- | ------------------------------ |
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Updater signatures     |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | macOS code signing |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`             | macOS notarisation             |
| Windows signing (Azure Trusted Signing, to be decided)    | Windows code signing           |

## Repository settings to apply

- Branch protection on `main`: require `CI gate`, linear history.
- Create the `experimental` branch when experimental work begins.
