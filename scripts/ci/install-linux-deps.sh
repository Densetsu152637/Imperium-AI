#!/usr/bin/env bash
# System libraries Imperium needs to build on Ubuntu (Tauri and the Secret Service keychain).
set -euo pipefail

sudo apt-get update
sudo apt-get install -y --no-install-recommends \
  build-essential file patchelf pkg-config libssl-dev libdbus-1-dev \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev
