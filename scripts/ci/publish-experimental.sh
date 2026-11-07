#!/usr/bin/env bash
# Replace the rolling "experimental" pre-release with dist/: publish-experimental.sh <sha>
set -euo pipefail

sha="$1"
(cd dist && sha256sum -- * > SHA256SUMS)

gh release delete experimental --yes --cleanup-tag || true
gh release create experimental dist/* --target "$sha" --prerelease --latest=false \
  --title "Experimental build" \
  --notes "Rolling experimental build of $sha. May be unstable; unsigned unless signing secrets are configured."
