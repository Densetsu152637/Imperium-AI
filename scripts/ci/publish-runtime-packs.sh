#!/usr/bin/env bash
# Publish runtime packs in dist/ to the release for the pinned llama.cpp ref.
# Usage: publish-runtime-packs.sh <sha>
set -euo pipefail

sha="$1"
# shellcheck source=runtimes/llama-cpp.env
source runtimes/llama-cpp.env
tag="runtime-llama.cpp-$LLAMA_CPP_REF"
(cd dist && sha256sum -- * > SHA256SUMS)

if gh release view "$tag" > /dev/null 2>&1; then
  gh release upload "$tag" dist/* --clobber
else
  gh release create "$tag" dist/* --target "$sha" --prerelease --latest=false \
    --title "llama.cpp runtime packs ($LLAMA_CPP_REF)" \
    --notes "llama-server runtime packs built from llama.cpp $LLAMA_CPP_REF. See docs/11-ci-cd.md."
fi
