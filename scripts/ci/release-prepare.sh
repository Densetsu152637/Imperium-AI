#!/usr/bin/env bash
# Derive release outputs from a tag: release-prepare.sh <tag>
set -euo pipefail

tag="$1"
out="${GITHUB_OUTPUT:-/dev/stdout}"
if [ ! -f apps/desktop/src-tauri/tauri.conf.json ]; then
  echo "::error::No desktop app to release yet (apps/desktop/src-tauri/tauri.conf.json is missing)."
  exit 1
fi

version="${tag#v}"
prerelease=false
if [[ "$version" == *-* ]]; then prerelease=true; fi
{
  echo "version=$version"
  echo "prerelease=$prerelease"
} >> "$out"
