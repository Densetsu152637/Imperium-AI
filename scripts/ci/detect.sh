#!/usr/bin/env bash
# Report which parts of the repository exist, and whether the change touches plugin approval's
# presence check (which the presence end-to-end test then gates), as GitHub Actions step outputs.
# DIFF_BASE is the commit the change is compared with; empty or unreadable means "run it".
set -euo pipefail

out="${GITHUB_OUTPUT:-/dev/stdout}"
has() { if [ -e "$1" ]; then echo true; else echo false; fi; }
{
  echo "rust=$(has Cargo.toml)"
  echo "web=$(has package.json)"
  echo "python=$(has sidecars/media/pyproject.toml)"
  echo "desktop=$(has apps/desktop/src-tauri/tauri.conf.json)"
} >> "$out"

# The paths the presence end-to-end test covers (.github/workflows/presence-e2e.yml): the approval
# flow, what it is built and run with, and this detection itself.
presence_paths='^('
presence_paths+='\.github/workflows/presence-e2e\.yml|\.github/workflows/ci\.yml|'
presence_paths+='scripts/ci/presence-e2e/|scripts/ci/detect\.sh|scripts/ci/install-linux-deps\.sh|'
presence_paths+='crates/imperium-core/src/app_identity/|crates/imperium-core/src/plugin_records\.rs|'
presence_paths+='crates/imperium-core/src/rpc/config\.rs|crates/imperium-core/src/server\.rs|'
presence_paths+='crates/imperium-core/src/plugin_install\.rs|crates/imperium-extensions/src/plugins\.rs|'
presence_paths+='crates/imperium-ipc/|'
presence_paths+='apps/desktop/src-tauri/src/plugin_approval\.rs|apps/desktop/src-tauri/src/main\.rs|'
presence_paths+='apps/desktop/src-tauri/capabilities/|apps/desktop/src-tauri/tauri\.conf\.json|'
presence_paths+='apps/desktop/src-tauri/linux/|Cargo\.lock'
presence_paths+=')'
presence=true
base="${DIFF_BASE:-}"
if [ -n "$base" ] && [ "$base" != 0000000000000000000000000000000000000000 ] &&
  { git cat-file -e "$base^{commit}" 2>/dev/null || git fetch -q --no-tags --depth=1 origin "$base" 2>/dev/null; }; then
  # Without rename detection, a moved file lists its old path too, so moving watched code away
  # still runs the test.
  if git diff --name-only --no-renames "$base" HEAD | grep -qE "$presence_paths"; then
    presence=true
  else
    presence=false
  fi
fi
echo "presence=$presence" >> "$out"
