#!/usr/bin/env bash
# Publish dist/ as a GitHub Release: publish-release.sh <tag> <prerelease:true|false>
set -euo pipefail

tag="$1" prerelease="$2"
(cd dist && sha256sum -- * > SHA256SUMS)

flags=(--title "$tag" --generate-notes --verify-tag)
if [ "$prerelease" = true ]; then flags+=(--prerelease); else flags+=(--latest); fi
gh release create "$tag" "${flags[@]}" dist/*
