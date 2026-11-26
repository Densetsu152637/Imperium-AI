"""Per-worker Job Objects on Windows, from the front (`ctypes` on `kernel32`, no agent data).

Each worker gets its own job, assigned right after it is started and long before it reads its
call (it is still importing SymPy), with:

- `JOB_OBJECT_LIMIT_PROCESS_MEMORY`: committed memory;
- `JOB_OBJECT_LIMIT_PROCESS_TIME`: user-mode CPU time (the job ends the process when it is out);
- `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: closing the job's handle (the front ending, for one)
  kills the worker;
- `JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION`: no error dialog and no Windows Error Reporting
  hold on a crashed worker.

The job starts with conservative limits (the call's limits plus a warm-up allowance) and is
armed when the worker reports ready: CPU used so far + the CPU limit, and committed memory so far
+ the memory limit, the same baseline as Linux (plan D3). Arming only lowers the limits.

This is the front creating processes, which plan D3 rejects for production (inside the
AppContainer the front cannot start processes, and stage 5's broker owns the jobs). It is used
by the tests on Windows, so every test worker there runs limited, and by a front started with
`IMPERIUM_CAS_WINDOWS_JOBS=1` until stage 5.
"""

import ctypes
import sys
from ctypes import wintypes

JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS = 9
LIMIT_PROCESS_TIME = 0x0002
LIMIT_PROCESS_MEMORY = 0x0100
LIMIT_JOB_MEMORY = 0x0200
LIMIT_DIE_ON_UNHANDLED_EXCEPTION = 0x0400
LIMIT_KILL_ON_JOB_CLOSE = 0x2000
PROCESS_ACCESS = 0x0001 | 0x0010 | 0x0100 | 0x1000  # terminate, VM read, set quota, query
TICKS_PER_SECOND = 10_000_000  # FILETIME and the time limits count 100 ns ticks
WARM_UP_CPU_SECONDS = 30  # allowance before arming: SymPy's import and the warm-up
WARM_UP_MEMORY = 512 * 1024 * 1024
CPU_EXIT = 1816  # ERROR_NOT_ENOUGH_QUOTA: the job's exit status for a process out of CPU time


class BasicLimits(ctypes.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", ctypes.c_int64),
        ("PerJobUserTimeLimit", ctypes.c_int64),
        ("LimitFlags", wintypes.DWORD),
        ("MinimumWorkingSetSize", ctypes.c_size_t),
        ("MaximumWorkingSetSize", ctypes.c_size_t),
        ("ActiveProcessLimit", wintypes.DWORD),
        ("Affinity", ctypes.c_size_t),
        ("PriorityClass", wintypes.DWORD),
        ("SchedulingClass", wintypes.DWORD),
    ]


class IoCounters(ctypes.Structure):
    _fields_ = [(name, ctypes.c_uint64) for name in ("r", "w", "o", "rb", "wb", "ob")]


class ExtendedLimits(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", BasicLimits),
        ("IoInfo", IoCounters),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


class MemoryCounters(ctypes.Structure):
    """`PROCESS_MEMORY_COUNTERS_EX`."""

    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        *(
            (name, ctypes.c_size_t)
            for name in (
                "PeakWorkingSetSize",
                "WorkingSetSize",
                "QuotaPeakPagedPoolUsage",
                "QuotaPagedPoolUsage",
                "QuotaPeakNonPagedPoolUsage",
                "QuotaNonPagedPoolUsage",
                "PagefileUsage",
                "PeakPagefileUsage",
                "PrivateUsage",
            )
        ),
    ]


if sys.platform == "win32":
    KERNEL32 = ctypes.WinDLL("kernel32", use_last_error=True)
    KERNEL32.CreateJobObjectW.restype = wintypes.HANDLE
    KERNEL32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    KERNEL32.OpenProcess.restype = wintypes.HANDLE
    KERNEL32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    KERNEL32.SetInformationJobObject.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
    ]
    KERNEL32.QueryInformationJobObject.argtypes = [
        wintypes.HANDLE,
        ctypes.c_int,
        ctypes.c_void_p,
        wintypes.DWORD,
        ctypes.c_void_p,
    ]
    KERNEL32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    KERNEL32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.c_void_p] * 4
    KERNEL32.K32GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD]
    KERNEL32.CloseHandle.argtypes = [wintypes.HANDLE]
    KERNEL32.GetCurrentProcess.restype = wintypes.HANDLE


def check(ok) -> None:
    if not ok:
        raise ctypes.WinError(ctypes.get_last_error())


def set_limits(job, flags: int, cpu_ticks: int, process_memory: int, job_memory: int = 0):
    info = ExtendedLimits()
    info.BasicLimitInformation.LimitFlags = flags
    info.BasicLimitInformation.PerProcessUserTimeLimit = cpu_ticks
    info.ProcessMemoryLimit = process_memory
    info.JobMemoryLimit = job_memory
    check(
        KERNEL32.SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
            ctypes.byref(info),
            ctypes.sizeof(info),
        )
    )


class WorkerJob:
    """One worker's job and process handle, from its start until the front has its status."""

    FLAGS = (
        LIMIT_PROCESS_TIME
        | LIMIT_PROCESS_MEMORY
        | LIMIT_KILL_ON_JOB_CLOSE
        | LIMIT_DIE_ON_UNHANDLED_EXCEPTION
    )

    def __init__(self, pid: int, cpu_seconds: int, memory_mib: int):
        self.cpu_seconds, self.memory = cpu_seconds, memory_mib * 1024 * 1024
        self.cpu_limit = (cpu_seconds + WARM_UP_CPU_SECONDS) * TICKS_PER_SECOND
        self.memory_limit = self.memory + WARM_UP_MEMORY
        self.job = KERNEL32.CreateJobObjectW(None, None)
        check(self.job)
        self.process = KERNEL32.OpenProcess(PROCESS_ACCESS, False, pid)
        try:
            check(self.process)
            set_limits(self.job, self.FLAGS, self.cpu_limit, self.memory_limit)
            check(KERNEL32.AssignProcessToJobObject(self.job, self.process))
        except OSError:
            self.close()
            raise

    def cpu_ticks(self) -> int:
        times = [ctypes.c_uint64() for _ in range(4)]  # creation, exit, kernel, user
        check(KERNEL32.GetProcessTimes(self.process, *(ctypes.byref(t) for t in times)))
        return times[3].value

    def committed(self) -> int:
        counters = MemoryCounters()
        counters.cb = ctypes.sizeof(counters)
        check(
            KERNEL32.K32GetProcessMemoryInfo(
                self.process, ctypes.byref(counters), ctypes.sizeof(counters)
            )
        )
        return counters.PrivateUsage

    def arm(self) -> None:
        """Set the call's limits from the post-warm-up baseline; never raise a limit."""
        self.cpu_limit = min(self.cpu_limit, self.cpu_ticks() + self.cpu_seconds * TICKS_PER_SECOND)
        self.memory_limit = min(self.memory_limit, self.committed() + self.memory)
        set_limits(self.job, self.FLAGS, self.cpu_limit, self.memory_limit)

    def exceeded(self, status: int) -> str | None:
        """Which limit ended the worker, if one did: `cpu` or `memory`."""
        try:
            if status == CPU_EXIT or self.cpu_ticks() >= self.cpu_limit:
                return "cpu"
            info = ExtendedLimits()
            check(
                KERNEL32.QueryInformationJobObject(
                    self.job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION_CLASS,
                    ctypes.byref(info),
                    ctypes.sizeof(info),
                    None,
                )
            )
        except OSError:
            return None
        # A failed allocation near the limit: the worker exits `MEMORY` itself, or C code aborts.
        if status != 0 and info.PeakProcessMemoryUsed >= self.memory_limit - 16 * 1024 * 1024:
            return "memory"
        return None

    def close(self) -> None:
        """Close both handles; closing the job kills the worker if it still runs."""
        for handle in (self.process, self.job):
            if handle:
                KERNEL32.CloseHandle(handle)
        self.process = self.job = None


def cap_this_process(job_memory: int) -> object:
    """Put the current process (and the processes it starts) in a job whose total committed
    memory is capped: for the test run, so a runaway test fails the run, not the machine. The
    returned handle must stay open for the cap to hold."""
    job = KERNEL32.CreateJobObjectW(None, None)
    check(job)
    set_limits(job, LIMIT_JOB_MEMORY, 0, 0, job_memory)
    check(KERNEL32.AssignProcessToJobObject(job, KERNEL32.GetCurrentProcess()))
    return job
