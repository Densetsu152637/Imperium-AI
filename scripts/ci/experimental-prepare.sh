#!/usr/bin/env bash
# Compute the experimental release's version: X.Y.Z-experimental.<run number>, where X.Y.Z is the
# desktop app's version. The run number only grows, so each build updates the one before it.
set -euo pipefail

out="${GITHUB_OUTPUT:-/dev/stdout}"
conf=apps/desktop/src-tauri/tauri.conf.json

if [ "${GITHUB_REF_NAME:-}" != experimental ]; then
  echo "::error::Experimental releases are built from the experimental branch, not ${GITHUB_REF_NAME:-this ref}."
  exit 1
fi

base="$(jq -r .version "$conf")"
{
  echo "sha=$(git rev-parse HEAD)"
  echo "version=${base%%-*}-experimental.${GITHUB_RUN_NUMBER:?}"
} >> "$out"
