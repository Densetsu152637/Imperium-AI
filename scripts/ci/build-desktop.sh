#!/usr/bin/env bash
# Build desktop bundles: build-desktop.sh <tauri-target> <bundles> <version>
# Copies installers into dist/.
set -euo pipefail

target="$1" bundles="$2" version="$3"

# MSI and RPM reject the hyphen in pre-release versions, so those builds ship NSIS, AppImage,
# and deb only.
if [[ "$version" == *-* ]]; then
  bundles="$(tr ',' '\n' <<< "$bundles" | grep -vxE 'msi|rpm' | paste -sd, -)"
fi

# Tauri treats an empty signing variable as configured; drop unset secrets.
for var in TAURI_SIGNING_PRIVATE_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
  APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD APPLE_SIGNING_IDENTITY \
  APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
  if [ -z "${!var:-}" ]; then unset "$var"; fi
done

# The protocol package's index is generated, not committed; the interface imports it.
pnpm --filter @imperium/protocol run generate

# The installer carries the daemon and CLI beside the app, which starts the daemon itself.
# Tauri expects them as binaries/<name>-<target>[.exe]. A universal macOS build compiles the app
# once per architecture and checks each one's sidecars, so it needs the per-architecture binaries
# as well as the merged universal one.
ext=""
if [[ "$target" == *windows* ]]; then ext=".exe"; fi
rust_targets=("$target")
if [ "$target" = universal-apple-darwin ]; then
  rust_targets=(aarch64-apple-darwin x86_64-apple-darwin)
fi
for t in "${rust_targets[@]}"; do
  cargo build --release --locked -p imperium-cli -p imperiumd --target "$t"
done
sidecars=apps/desktop/src-tauri/binaries
mkdir -p "$sidecars"
for bin in imperium imperiumd; do
  out="$sidecars/$bin-$target$ext"
  if [ "${#rust_targets[@]}" -gt 1 ]; then
    inputs=()
    for t in "${rust_targets[@]}"; do
      inputs+=("target/$t/release/$bin")
      cp "target/$t/release/$bin" "$sidecars/$bin-$t"
    done
    lipo -create -output "$out" "${inputs[@]}"
  else
    cp "target/$target/release/$bin$ext" "$out"
  fi
done

# Updater artifacts (signed installers and the macOS .app.tar.gz) need the signing key.
updater=false
if [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then updater=true; fi

bundle="{\"createUpdaterArtifacts\":$updater,\"externalBin\":[\"binaries/imperiumd\",\"binaries/imperium\"]}"
pnpm --filter @imperium/desktop exec tauri build --target "$target" --bundles "$bundles" \
  --config "{\"version\":\"$version\",\"bundle\":$bundle}"

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
