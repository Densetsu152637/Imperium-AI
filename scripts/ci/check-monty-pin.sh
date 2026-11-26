#!/usr/bin/env bash
# The Monty pin must agree everywhere it is written: runtimes/monty.env, the exact `=x.y.z`
# requirements in the two crates that use Monty, and the versions and crates.io checksums in
# Cargo.lock. Run from the repository root.
set -euo pipefail

# shellcheck source=/dev/null
source runtimes/monty.env

fail() {
  echo "Monty pin: $*" >&2
  exit 1
}

# The 14-day rule is checked when a release is adopted; here only that the recorded date is sane.
[[ "$MONTY_PUBLISHED" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]] || fail "MONTY_PUBLISHED is not a date"

for manifest in crates/imperium-script-worker/Cargo.toml crates/imperium-script/Cargo.toml; do
  # Every monty* dependency line must be an exact pin at MONTY_VERSION.
  while IFS= read -r line; do
    grep -qF "\"=${MONTY_VERSION}\"" <<<"$line" || fail "$manifest: not pinned to =${MONTY_VERSION}: $line"
  done < <(grep -E '^monty(-[a-z]+)? *=' "$manifest")
done

for crate in monty monty-proto monty-types monty-alloc; do
  var="MONTY_CHECKSUM_${crate//-/_}"
  expected="${!var:-}"
  [ -n "$expected" ] || fail "runtimes/monty.env has no $var"
  block="$(awk -v name="$crate" '
    $0 == "[[package]]" { inpkg = 1; n = ""; v = ""; c = ""; next }
    inpkg && /^name = / { gsub(/"/, "", $3); n = $3 }
    inpkg && /^version = / { gsub(/"/, "", $3); v = $3 }
    inpkg && /^checksum = / { gsub(/"/, "", $3); c = $3 }
    inpkg && n == name && v != "" && c != "" { print v " " c; exit }
  ' Cargo.lock)"
  [ -n "$block" ] || fail "Cargo.lock has no $crate with a checksum"
  [ "${block%% *}" = "$MONTY_VERSION" ] || fail "Cargo.lock has $crate ${block%% *}, not $MONTY_VERSION"
  [ "${block##* }" = "$expected" ] || fail "Cargo.lock checksum of $crate differs from runtimes/monty.env"
done

echo "Monty pin: ${MONTY_VERSION} agrees across runtimes/monty.env, both manifests and Cargo.lock."
