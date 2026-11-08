#!/usr/bin/env bash
# Rust checks, shared by local development and CI.
set -euo pipefail

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# cargo test regenerates the protocol's TypeScript bindings; they must match what is checked in.
generated=packages/protocol/src/generated
untracked="$(git ls-files --others --exclude-standard -- "$generated")"
if ! git diff --quiet -- "$generated" || [ -n "$untracked" ]; then
  echo "Generated protocol types are out of date. Run cargo test and commit $generated:" >&2
  git status --short -- "$generated" >&2
  exit 1
fi

# The same for the tool schemas the protocol crate exports; the prompts next to them are
# hand-written files, so this also catches stray or modified files there.
contract=crates/imperium-protocol/schemas
untracked="$(git ls-files --others --exclude-standard -- "$contract")"
if ! git diff --quiet -- "$contract" || [ -n "$untracked" ]; then
  echo "Exported tool schemas are out of date. Run cargo test and commit $contract:" >&2
  git status --short -- "$contract" >&2
  exit 1
fi
