"""The front's backstops on macOS (plan D3): read a worker's footprint and CPU time.

macOS does not enforce `RLIMIT_AS`, so the front polls each running worker's `ri_phys_footprint`
(`proc_pid_rusage` from `libproc`, part of `libSystem`) every 50 ms and kills it over its limit.
Polling is not a hard cap: between two polls a worker can allocate as fast as the machine allows,
and stage 2b measures that overshoot.

macOS sends `SIGXCPU` at the soft `RLIMIT_CPU` limit but, unlike Linux, does not kill at the hard
limit (measured in stage 2b), so a worker stuck in C code would run on to the wall clock. The
same poll reads its CPU time and the front kills it one second past the CPU limit, as Linux's
hard limit does. `ri_user_time` and `ri_system_time` are in Mach time units, converted with
`mach_timebase_info`. This module takes a PID and no agent data.
"""

import ctypes
import sys

RUSAGE_INFO_V0 = 0


class TimebaseInfo(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


class RusageInfoV0(ctypes.Structure):
    """`struct rusage_info_v0` from `<sys/resource.h>`."""

    _fields_ = [
        ("ri_uuid", ctypes.c_uint8 * 16),
        ("ri_user_time", ctypes.c_uint64),
        ("ri_system_time", ctypes.c_uint64),
        ("ri_pkg_idle_wkups", ctypes.c_uint64),
        ("ri_interrupt_wkups", ctypes.c_uint64),
        ("ri_pageins", ctypes.c_uint64),
        ("ri_wired_size", ctypes.c_uint64),
        ("ri_resident_size", ctypes.c_uint64),
        ("ri_phys_footprint", ctypes.c_uint64),
        ("ri_proc_start_abstime", ctypes.c_uint64),
        ("ri_proc_exit_abstime", ctypes.c_uint64),
    ]


if sys.platform == "darwin":
    LIBPROC = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
    LIBPROC.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    LIBPROC.proc_pid_rusage.restype = ctypes.c_int
    TIMEBASE = TimebaseInfo()
    LIBPROC.mach_timebase_info(ctypes.byref(TIMEBASE))
    SECONDS_PER_TICK = TIMEBASE.numer / TIMEBASE.denom / 1e9


def usage(pid: int) -> tuple[int, float] | None:
    """The process's physical footprint in bytes and CPU seconds, or `None` once it is gone."""
    info = RusageInfoV0()
    if LIBPROC.proc_pid_rusage(pid, RUSAGE_INFO_V0, ctypes.byref(info)) != 0:
        return None
    ticks = info.ri_user_time + info.ri_system_time
    return int(info.ri_phys_footprint), ticks * SECONDS_PER_TICK
