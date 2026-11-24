#!/usr/bin/env bash
# The presence check's end-to-end test on a Linux runner (docs/11-ci-cd.md, "Presence end-to-end").
#
# Sets up what a Linux desktop has: an X display (Xvfb), a session bus with the accessibility bus,
# the system bus's polkitd with Imperium's action file installed, and a polkit authentication
# agent (pkttyagent, answered by drive.py through a terminal) for a throwaway password given to the
# runner's own user. Then drive.py runs the release daemon and the release desktop app (through
# tauri-driver, WebKit's WebDriver) and approves plugins through the app's native dialog, which it
# answers through the accessibility bus. Run after build.sh. Logs go to $E2E_LOGS.
set -euo pipefail

root="$(cd "$(dirname "$0")/../../.." && pwd)"
logs="${E2E_LOGS:-$root/target/presence-e2e}"
mkdir -p "$logs"
action_file="$root/apps/desktop/src-tauri/linux/org.imperium.plugins.approve.policy"
installed_action=/usr/share/polkit-1/actions/org.imperium.plugins.approve.policy

for tool in Xvfb dbus-run-session pkttyagent pkaction WebKitWebDriver tauri-driver; do
  command -v "$tool" >/dev/null || { echo "missing $tool" >&2; exit 1; }
done
/usr/bin/python3 -c 'import gi; gi.require_version("Atspi", "2.0"); from gi.repository import Atspi'

# Ubuntu 24.04 keeps unprivileged user namespaces behind AppArmor; WebKit's web process sandbox
# (bubblewrap) needs them.
if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
  sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
fi

# A throwaway password for the runner's user, so polkit's auth_self has something to check. The
# runner is discarded after the job.
password="presence-e2e-$RANDOM-$RANDOM"
echo "$(id -un):$password" | sudo chpasswd

# polkitd with the action file the packages ship.
sudo install -m 0644 "$action_file" "$installed_action"
sudo systemctl restart polkit
for _ in $(seq 1 30); do
  if pkaction --action-id org.imperium.plugins.approve --verbose >"$logs/pkaction.txt" 2>&1; then
    break
  fi
  sleep 1
done
cat "$logs/pkaction.txt"
grep -q "implicit any: *auth_self$" "$logs/pkaction.txt"

Xvfb :99 -screen 0 1600x1000x24 -nolisten tcp >"$logs/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
  [ -e /tmp/.X11-unix/X99 ] && break
  sleep 0.2
done

export DISPLAY=:99
export E2E_ROOT="$root" E2E_LOGS="$logs" E2E_PASSWORD="$password"
export E2E_ACTION_FILE="$action_file" E2E_INSTALLED_ACTION="$installed_action"
# GTK's accessibility bridge, so drive.py can find and press the dialog's buttons.
export GNOME_ACCESSIBILITY=1
unset NO_AT_BRIDGE
dbus-run-session -- /usr/bin/python3 "$root/scripts/ci/presence-e2e/drive.py"
