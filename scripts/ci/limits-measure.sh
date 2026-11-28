#!/usr/bin/env bash
# The L6 measurements of docs/plans/resource-caps.md (stage 1): the peak tasks, processes and
# memory of a real build of this workspace and of a headless Chromium session, each run in an
# uncapped cgroup unit by `limit_probe measure`. Prints MEASURE lines. CI only (Linux, sudo).
set -euo pipefail

cargo build --locked -p imperium-limits --features test-support --bin limit_probe
probe="$RUNNER_TEMP/limit_probe"
cp target/debug/limit_probe "$probe"

# A cold build in a target folder of its own, so every crate is really compiled.
CARGO_TARGET_DIR="$RUNNER_TEMP/measure-target" IMPERIUM_CI_CGROUP_MEMORY=$((12 << 30)) \
  bash scripts/ci/limits-cgroup.sh "$probe" measure -- cargo build --locked --workspace

chrome="$(command -v google-chrome || command -v google-chrome-stable || command -v chromium)"
profile="$(mktemp -d)"
# The flags the CDP browser uses (crates/imperium-web/src/browser.rs), on a real page for 20 s.
bash scripts/ci/limits-cgroup.sh "$probe" measure --secs 20 -- "$chrome" \
  --headless=new --remote-debugging-port=9222 --remote-debugging-address=127.0.0.1 \
  --user-data-dir="$profile" --no-first-run --no-default-browser-check \
  --window-size=1280,900 https://example.com
