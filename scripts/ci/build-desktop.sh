#!/usr/bin/env bash
# Build desktop bundles: build-desktop.sh <tauri-target> <bundles> <version>
# Copies installers into dist/.
set -euo pipefail

target="$1" bundles="$2" version="$3"

# MSI accepts only numeric versions, so pre-release builds ship NSIS only.
if [[ "$version" == *-* ]]; then
  bundles="$(tr ',' '\n' <<< "$bundles" | grep -vx msi | paste -sd, -)"
fi

# Tauri treats an empty signing variable as configured; drop unset secrets.
for var in TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
  APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
  APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
  if [ -z "${!var:-}" ]; then unset "$var"; fi
done

pnpm --filter @imperium/desktop exec tauri build \
  --target "$target" --bundles "$bundles" --config "{\"version\":\"$version\"}"

mkdir -p dist
find "target/$target/release/bundle" -type f \( -name '*.AppImage' -o -name '*.deb' \
  -o -name '*.rpm' -o -name '*.msi' -o -name '*-setup.exe' -o -name '*.dmg' \
  -o -name '*.sig' -o -name '*.app.tar.gz' \) -exec cp {} dist/ \;

# The macOS update archive is named after the product only; give it the version and target.
for archive in dist/*.app.tar.gz; do
  [ -e "$archive" ] || continue
  arch="${target%%-apple-darwin}"
  renamed="dist/Imperium_${version}_${arch}.app.tar.gz"
  mv "$archive" "$renamed"
  if [ -e "$archive.sig" ]; then mv "$archive.sig" "$renamed.sig"; fi
done
