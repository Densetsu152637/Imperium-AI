#!/usr/bin/env bash
# Media sidecar checks, shared by local development and CI.
set -euo pipefail

cd "$(dirname "$0")/../sidecars/media"
uv sync --locked
uv run ruff check .
uv run pytest
