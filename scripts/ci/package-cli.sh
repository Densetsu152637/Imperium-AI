#!/usr/bin/env bash
# Build and archive the CLI and daemon: package-cli.sh <platform-id> <rust-targets>
# <rust-targets> is comma-separated; two macOS targets are merged into a universal binary.
set -euo pipefail

id="$1"
IFS=',' read -ra targets <<< "$2"
name="imperium-$id"
stage="dist/stage/$name"
mkdir -p "$stage"

for t in "${targets[@]}"; do
  cargo build --release --locked -p imperium-cli -p imperiumd --target "$t"
done

ext=""
if [[ "${targets[0]}" == *windows* ]]; then ext=".exe"; fi
for bin in imperium imperiumd; do
  if [ "${#targets[@]}" -gt 1 ]; then
    inputs=()
    for t in "${targets[@]}"; do inputs+=("target/$t/release/$bin"); done
    lipo -create -output "$stage/$bin" "${inputs[@]}"
  else
    cp "target/${targets[0]}/release/$bin$ext" "$stage/"
  fi
done
cp LICENSE README.md "$stage/"
# The bundled default workflow pack and pets are found beside the daemon.
cp -R policy pets "$stage/"

tar -czf "dist/$name.tar.gz" -C dist/stage "$name"
rm -rf dist/stage
