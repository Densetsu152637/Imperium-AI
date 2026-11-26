"""The front's memory backstop on macOS (plan D3): read a worker's physical footprint.

macOS does not enforce `RLIMIT_AS`, so the front polls each running worker's `ri_phys_footprint`
(`proc_pid_rusage` from `libproc`, part of `libSystem`) every 50 ms and kills it over its limit.
Polling is not a hard cap: between two polls a worker can allocate as fast as the machine allows,
and stage 2b measures that overshoot. This module takes a PID and no agent data.
"""

import ctypes
import sys

RUSAGE_INFO_V0 = 0


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


def footprint(pid: int) -> int | None:
    """The process's physical footprint in bytes, or `None` once it is gone."""
    info = RusageInfoV0()
    if LIBPROC.proc_pid_rusage(pid, RUSAGE_INFO_V0, ctypes.byref(info)) != 0:
        return None
    return int(info.ri_phys_footprint)
