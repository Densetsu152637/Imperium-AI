#!/usr/bin/env bash
# Python sidecar checks, shared by local development and CI.
# Usage: check-python.sh [media|cas]   (media by default, so the media job is unchanged)
set -euo pipefail

sidecar="${1:-media}"
cd "$(dirname "$0")/../sidecars/$sidecar"

case "$sidecar" in
  media)
    uv sync --locked
    uv run ruff check .
    uv run pytest
    ;;
  cas)
    # --no-build: the lock holds dependencies only (`package = false`), so no build backend runs.
    uv sync --locked --no-build
    uv run ruff check .
    uv run ruff format --check .
    uv run pytest
    ;;
  *)
    echo "unknown sidecar: $sidecar (expected media or cas)" >&2
    exit 2
    ;;
esac
