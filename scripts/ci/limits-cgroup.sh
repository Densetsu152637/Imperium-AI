#!/usr/bin/env bash
# Runs a command inside a delegated cgroup v2 subtree, for the imperium-limits tests and
# measurements (docs/plans/resource-caps.md, stage 1). CI only: it needs sudo.
#
# /sys/fs/cgroup/imperium-ci gets a hard ceiling of its own (IMPERIUM_CI_CGROUP_MEMORY, 6 GiB by
# default, and 16,384 tasks), so a cap that fails to hold cannot take the runner down. The command
# runs in its `runner` leaf; units go under `limits`, which IMPERIUM_TEST_CGROUP names. The
# subtree is handed to the runner's user, as systemd's Delegate= would.
set -euo pipefail

root=/sys/fs/cgroup/imperium-ci
memory="${IMPERIUM_CI_CGROUP_MEMORY:-$((6 << 30))}"

sudo mkdir -p "$root/runner" "$root/limits"
echo "$memory" | sudo tee "$root/memory.max" >/dev/null
echo 0 | sudo tee "$root/memory.swap.max" >/dev/null 2>&1 || true
echo 16384 | sudo tee "$root/pids.max" >/dev/null
# This shell moves first: a cgroup that hands controllers to its children cannot hold processes.
echo "$$" | sudo tee "$root/runner/cgroup.procs" >/dev/null
for controller in memory pids cpu; do
  echo "+$controller" | sudo tee "$root/cgroup.subtree_control" >/dev/null 2>&1 || true
done
sudo chown -R "$(id -u):$(id -g)" "$root"
echo "cgroup $root: controllers [$(cat "$root/cgroup.subtree_control")], memory.max $(cat "$root/memory.max")" >&2

export IMPERIUM_TEST_CGROUP="$root/limits"
exec "$@"
