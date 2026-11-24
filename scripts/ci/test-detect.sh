#!/usr/bin/env bash
# Tests detect.sh's change detection for the presence end-to-end test on a scratch repository:
# renaming a watched file away, editing one, and an unreadable base must run it; an unrelated edit
# must not. Run by ci.yml's detect job, and runnable locally.
set -euo pipefail

detect="$(cd "$(dirname "$0")" && pwd)/detect.sh"
repo="$(mktemp -d)"
trap 'rm -rf "$repo"' EXIT
cd "$repo"
git init -q
git config user.email ci@example.invalid
git config user.name ci
git config core.autocrlf false
mkdir -p crates/imperium-core/src/app_identity docs
printf 'pub fn check() {}\n' > crates/imperium-core/src/app_identity/mod.rs
printf '# notes\n' > docs/notes.md
git add -A
git commit -qm base
base="$(git rev-parse HEAD)"

presence() {
  local out
  out="$(mktemp)"
  DIFF_BASE="$1" GITHUB_OUTPUT="$out" bash "$detect"
  sed -n 's/^presence=//p' "$out"
  rm -f "$out"
}

expect() {
  local what="$1" want="$2" got="$3"
  if [ "$got" != "$want" ]; then
    echo "FAILED: $what: presence=$got, expected $want" >&2
    exit 1
  fi
  echo "ok: $what: presence=$got"
}

printf 'more\n' >> docs/notes.md
git commit -qam unrelated
expect "an unrelated edit" false "$(presence "$base")"

git mv crates/imperium-core/src/app_identity/mod.rs crates/imperium-core/src/moved.rs
git commit -qm rename
expect "a rename of a watched file out of the watched paths" true "$(presence "$(git rev-parse HEAD~1)")"

printf 'pub fn other() {}\n' >> crates/imperium-core/src/moved.rs
git commit -qam edit
printf 'x\n' > Cargo.lock
git add Cargo.lock
git commit -qm lock
expect "an edit of a watched file" true "$(presence "$(git rev-parse HEAD~1)")"
expect "an unreadable base" true "$(presence deadbeefdeadbeefdeadbeefdeadbeefdeadbeef)"
expect "no base" true "$(presence "")"
