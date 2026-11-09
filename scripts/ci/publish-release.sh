#!/usr/bin/env bash
# Publish dist/ as a main-channel GitHub Release: publish-release.sh <tag> <prerelease:true|false>
set -euo pipefail

tag="$1" prerelease="$2"
bash scripts/ci/updater-manifest.sh "${tag#v}" "$tag"
(cd dist && sha256sum -- * > SHA256SUMS)

flags=(--title "$tag (main)" --generate-notes --verify-tag)
if [ "$prerelease" = true ]; then flags+=(--prerelease); else flags+=(--latest); fi
gh release create "$tag" "${flags[@]}" dist/*

# One main release at a time: a stable release replaces the last main release; a main
# pre-release replaces only the last pre-release.
if [ "$prerelease" = true ]; then
  bash scripts/ci/prune-releases.sh main-pre "$tag"
else
  bash scripts/ci/prune-releases.sh main "$tag"
fi
