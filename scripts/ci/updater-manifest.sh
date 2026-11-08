#!/usr/bin/env bash
# Write dist/latest.json, the desktop updater's feed, from the signed installers in dist/.
# Usage: updater-manifest.sh <version> <tag>. Writes nothing when no installer is signed.
set -euo pipefail

version="$1" tag="$2"
base="https://github.com/${GITHUB_REPOSITORY:?}/releases/download/$tag"

# The installer each updater platform downloads, by file-name suffix.
platforms=(
  "linux-x86_64:_amd64.AppImage"
  "linux-aarch64:_aarch64.AppImage"
  "windows-x86_64:_x64-setup.exe"
  "windows-aarch64:_arm64-setup.exe"
  "darwin-x86_64:_universal.app.tar.gz"
  "darwin-aarch64:_universal.app.tar.gz"
)

entries='{}'
for platform in "${platforms[@]}"; do
  key="${platform%%:*}" suffix="${platform#*:}"
  for sig in dist/*"$suffix".sig; do
    [ -e "$sig" ] || continue
    file="$(basename "${sig%.sig}")"
    entries="$(jq --arg key "$key" --arg url "$base/$file" --rawfile signature "$sig" \
      '.[$key] = {url: $url, signature: ($signature | rtrimstr("\n"))}' <<< "$entries")"
  done
done

if [ "$entries" = '{}' ]; then
  echo "::warning::No signed installers; the release has no updater feed."
  exit 0
fi

jq -n --arg version "$version" --arg notes "Imperium $tag" \
  --arg date "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson platforms "$entries" \
  '{version: $version, notes: $notes, pub_date: $date, platforms: $platforms}' > dist/latest.json
jq -r '.platforms | keys | join(", ") | "Updater feed for " + .' dist/latest.json
