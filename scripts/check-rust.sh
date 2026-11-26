#!/usr/bin/env bash
# Rust checks, shared by local development and CI.
set -euo pipefail

cargo fmt --all --check

# The test-only `test-host` feature of imperium-github (any API host) must not reach a shipped build.
for package in imperium-core imperium-cli imperium-desktop; do
  # Captured first, so a failing `cargo tree` stops the script instead of reading as "not found".
  tree="$(cargo tree --locked -p "$package" --edges normal,features)"
  if grep -F 'imperium-github feature "test-host"' <<<"$tree"; then
    echo "$package enables imperium-github's test-host feature in a normal build." >&2
    exit 1
  fi
  # Nor may code mode's test hooks (forcing a confinement stage to fail).
  if grep -E 'imperium-script(-worker)? feature "test-support"' <<<"$tree"; then
    echo "$package enables a code-mode test-support feature in a normal build." >&2
    exit 1
  fi
done
# Positive control: with dev edges the feature is listed, so the grep above matches the real format.
tree="$(cargo tree --locked -p imperium-github --edges all,features)"
grep -qF 'imperium-github feature "test-host"' <<<"$tree" || {
  echo "cargo tree no longer lists imperium-github's test-host feature; fix the leak check." >&2
  exit 1
}
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# cargo test regenerates the protocol's TypeScript bindings; they must match what is checked in.
generated=packages/protocol/src/generated
untracked="$(git ls-files --others --exclude-standard -- "$generated")"
if ! git diff --quiet -- "$generated" || [ -n "$untracked" ]; then
  echo "Generated protocol types are out of date. Run cargo test and commit $generated:" >&2
  git status --short -- "$generated" >&2
  git --no-pager diff -- "$generated" >&2
  for file in $untracked; do
    echo "--- new file: $file" >&2
    cat "$file" >&2
  done
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

# Snapshots written by tests: the protocol's method names and the command-line help (the
# latter only where the test runs, on Linux). A change to either shows up here, in review.
snapshots="crates/imperium-protocol/snapshots crates/imperium-cli/snapshots"
# shellcheck disable=SC2086
untracked="$(git ls-files --others --exclude-standard -- $snapshots)"
# shellcheck disable=SC2086
if ! git diff --quiet -- $snapshots || [ -n "$untracked" ]; then
  echo "Snapshots are out of date. Run cargo test and commit what it writes:" >&2
  # shellcheck disable=SC2086
  git status --short -- $snapshots >&2
  # shellcheck disable=SC2086
  git --no-pager diff -- $snapshots >&2
  for file in $untracked; do
    echo "--- new file: $file" >&2
    cat "$file" >&2
  done
  exit 1
fi
