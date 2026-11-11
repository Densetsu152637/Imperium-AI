#!/usr/bin/env bash
# System libraries Imperium needs to build on Ubuntu (Tauri and the Secret Service keychain).
set -euo pipefail

# A slow or stuck package mirror must fail fast and retry rather than hang the job until
# its timeout: each attempt is bounded, apt retries its own downloads, and the whole step is
# tried three times.
apt_options=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=30
  -o Acquire::https::Timeout=30
  -o Dpkg::Lock::Timeout=120
)
packages=(
  build-essential file patchelf pkg-config libssl-dev libdbus-1-dev
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev
)

for attempt in 1 2 3; do
  if sudo timeout 300 apt-get "${apt_options[@]}" update &&
    sudo timeout 600 apt-get "${apt_options[@]}" install -y --no-install-recommends "${packages[@]}"; then
    exit 0
  fi
  echo "apt attempt ${attempt} failed or timed out; retrying" >&2
  sleep $((attempt * 10))
done
echo "could not install the Linux build dependencies after 3 attempts" >&2
exit 1
