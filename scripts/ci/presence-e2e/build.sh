#!/usr/bin/env bash
# Builds what the presence end-to-end test drives: the release daemon and CLI, and the release
# desktop app with its interface built in (tauri build, no installer). Release builds, because the
# test asserts the kinds a release daemon records (`counted`, `dialog-only`); a debug daemon records
# only `dev`. All three land in target/release, so the daemon finds the app beside itself.
set -euo pipefail

# The protocol package's index is generated, not committed; the interface imports it.
pnpm --filter @imperium/protocol run generate
cargo build --release --locked -p imperium-core -p imperium-cli
pnpm --filter @imperium/desktop exec tauri build --no-bundle
ls -l target/release/imperium-core target/release/imperium target/release/imperium-desktop
