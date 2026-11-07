#!/usr/bin/env bash
# Rust checks, shared by local development and CI.
set -euo pipefail

cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
