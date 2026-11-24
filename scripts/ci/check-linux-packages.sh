#!/usr/bin/env bash
# Checks that the Linux packages in dist/ carry the polkit action of plugin approvals' presence
# check (docs/06-extensibility.md, "Who may approve a plugin"), byte for byte as in the source tree.
# The .deb is always built; the .rpm only for versions without a pre-release part.
set -euo pipefail

source_file=apps/desktop/src-tauri/linux/org.imperium.plugins.approve.policy
packaged=usr/share/polkit-1/actions/org.imperium.plugins.approve.policy
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

debs=(dist/*.deb)
[ -e "${debs[0]}" ] || { echo "::error::no .deb was built" >&2; exit 1; }
for deb in "${debs[@]}"; do
  rm -rf "$work/deb" && mkdir "$work/deb"
  dpkg-deb -x "$deb" "$work/deb"
  cmp "$source_file" "$work/deb/$packaged" ||
    { echo "::error::$deb does not carry /$packaged" >&2; exit 1; }
  echo "$deb carries /$packaged"
done

for rpm in dist/*.rpm; do
  [ -e "$rpm" ] || continue
  command -v rpm2cpio >/dev/null || sudo apt-get install -y --no-install-recommends rpm2cpio cpio
  rm -rf "$work/rpm" && mkdir "$work/rpm"
  (cd "$work/rpm" && rpm2cpio "$OLDPWD/$rpm" | cpio -idm --quiet)
  cmp "$source_file" "$work/rpm/$packaged" ||
    { echo "::error::$rpm does not carry /$packaged" >&2; exit 1; }
  echo "$rpm carries /$packaged"
done
