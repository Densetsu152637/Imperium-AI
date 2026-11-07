#!/usr/bin/env bash
# TypeScript/React checks, shared by local development and CI. Run after `pnpm install`.
set -euo pipefail

for task in lint typecheck test build; do
  pnpm -r --if-present run "$task"
done
