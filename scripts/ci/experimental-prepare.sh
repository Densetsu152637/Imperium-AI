#!/usr/bin/env bash
# Decide whether to build an experimental release and compute its version.
# Usage: experimental-prepare.sh <event-name>
set -euo pipefail

event="$1"
out="${GITHUB_OUTPUT:-/dev/stdout}"
conf=apps/desktop/src-tauri/tauri.conf.json
sha="$(git rev-parse HEAD)"

if [ ! -f "$conf" ]; then
  echo "::notice::No desktop app yet; skipping experimental build."
  echo "build=false" >> "$out"
  exit 0
fi

# Nightly runs skip when the rolling release already points at this commit.
current="$(git ls-remote origin refs/tags/experimental | cut -f1)"
if [ "$event" = schedule ] && [ "$current" = "$sha" ]; then
  echo "::notice::Experimental release is already at $sha."
  echo "build=false" >> "$out"
  exit 0
fi

base="$(jq -r .version "$conf")"
{
  echo "build=true"
  echo "sha=$sha"
  echo "version=${base%%-*}-experimental.$(date -u +%Y%m%d).${sha:0:7}"
} >> "$out"
