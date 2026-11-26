"""A test-only worker that spins on the CPU once it has the call (plan stage 2b).

With `--ignore-xcpu` first on its command line, the soft limit's `SIGXCPU` is ignored, which
stands in for a worker stuck in C code that never runs the handler: only the hard limit's
`SIGKILL` (one second later) can stop it.
"""

import signal
import sys

from imperium_cas import worker

if sys.argv[1:2] == ["--ignore-xcpu"]:
    del sys.argv[1]
    worker.exit_on_cpu_limit = signal.SIG_IGN


def spin(request: dict) -> dict:
    count = 0
    while True:
        count += 1


if __name__ == "__main__":
    worker.run(spin, sys.argv[1:])
