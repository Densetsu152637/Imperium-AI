#!/usr/bin/env bash
# Keep one release per channel: delete the channel's other releases and their tags after a new
# one is published, so the update feed always has a release to point at.
# Usage: prune-releases.sh <experimental|main|main-pre> <tag just published>
#   experimental  deletes every other vX.Y.N-experimental release (and older -experimental.N names)
#   main          deletes every other main release (stable and pre-release)
#   main-pre      deletes other main pre-releases only, keeping the stable release the main
#                 update feed points at
# Other releases, such as runtime packs, are never touched.
set -euo pipefail

channel="$1" keep="$2"

gh release list --limit 200 --json tagName,isPrerelease \
  --jq '.[] | "\(.tagName) \(.isPrerelease)"' |
  while read -r tag prerelease; do
    [ "$tag" = "$keep" ] && continue
    [[ "$tag" == v* ]] || continue
    experimental=false
    if [[ "$tag" == *-experimental* ]]; then experimental=true; fi
    case "$channel" in
      experimental) [ "$experimental" = true ] || continue ;;
      main) [ "$experimental" = false ] || continue ;;
      main-pre) [ "$experimental" = false ] && [ "$prerelease" = true ] || continue ;;
      *) echo "::error::unknown channel $channel" >&2; exit 1 ;;
    esac
    echo "Replacing $tag with $keep"
    gh release delete "$tag" --yes --cleanup-tag
  done
