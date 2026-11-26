"""The worker's own limits on Linux and macOS (plan D3), set after warm-up and before the hook.

Applied by the worker, from the values the front put on its command line:

- `RLIMIT_CORE` = 0 on every Unix, so a crash leaves no core file holding the request;
- `RLIMIT_CPU`: soft = CPU used after warm-up + the limit, hard = soft + 1 s. At the soft limit
  the kernel sends `SIGXCPU`, whose handler ends the worker with the `CPU` exit code; a worker
  stuck in C code never runs that handler, so the hard limit's `SIGKILL` is the backstop;
- `RLIMIT_AS` = address space after warm-up + the limit, on Linux only. An allocation over it
  raises `MemoryError`, which the worker turns into the `MEMORY` exit code. macOS does not
  enforce `RLIMIT_AS`: there the front polls the worker's footprint (`limits_macos.py`).

`setrlimit` raises the audit event `resource.setrlimit`, so all of this must run before the
audit hook is installed. Windows has none of these: there the limits are only requested on the
command line, and stage 5's broker applies them through the worker's Job Object.
"""

import math
import resource
import signal
import sys

STATM = "/proc/self/statm"  # the one file this module reads, before the hook, with no agent data


def cpu_used() -> float:
    """CPU seconds this process has used so far (user and system)."""
    usage = resource.getrusage(resource.RUSAGE_SELF)
    return usage.ru_utime + usage.ru_stime


def address_space() -> int:
    """This process's address space in bytes (Linux: the first field of `/proc/self/statm`)."""
    with open(STATM, encoding="ascii") as statm:
        pages = int(statm.read().split()[0])
    return pages * resource.getpagesize()


def clamp(kind: int, wanted: int) -> int:
    """`wanted`, or the existing hard limit when that is lower (a limit can only be lowered)."""
    hard = resource.getrlimit(kind)[1]
    return wanted if hard == resource.RLIM_INFINITY else min(wanted, hard)


def apply(cpu_seconds: int, memory_mib: int, on_cpu_limit) -> dict:
    """Set the worker's limits and return what was applied, for the ready frame.

    `on_cpu_limit(signum, frame)` becomes the `SIGXCPU` handler; it must exit at once.
    """
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    signal.signal(signal.SIGXCPU, on_cpu_limit)
    hard = clamp(resource.RLIMIT_CPU, math.ceil(cpu_used()) + cpu_seconds + 1)
    soft = hard - 1
    resource.setrlimit(resource.RLIMIT_CPU, (soft, hard))
    applied = {"core": 0, "cpu_soft": soft, "cpu_hard": hard, "address_space": None}
    if sys.platform.startswith("linux"):
        limit = clamp(resource.RLIMIT_AS, address_space() + memory_mib * 1024 * 1024)
        resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
        applied["address_space"] = limit
    return applied
