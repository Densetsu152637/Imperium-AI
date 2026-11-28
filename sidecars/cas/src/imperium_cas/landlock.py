"""The front's Landlock execute rule (plan D4, R2-3), applied before anything else runs.

`imperium_cas.__main__` calls `confine()` as its first statement, before any other import and
before stdin is read. On Linux with Landlock (ABI 1 or later) the ruleset handles
`LANDLOCK_ACCESS_FS_EXECUTE` and allows it only for the interpreter's own executable
(`realpath(sys.executable)`, inside the managed environment) and for its ELF loader (the kernel
opens it for execution too, so without it the interpreter could not start a worker). Workers
inherit the ruleset.

What this does and does not do: it blocks a direct `execve` of a shell or anything else in
`/usr/bin`. It does not cover the ELF loader itself: `ld.so <binary>` maps and runs any ELF file
the process can read, without an `execve` of that file. So this rule is defence in depth; the
boundary on Linux is the `bwrap` profile (plan D4, stage 4), which binds only the environment
root and the system library folders.

- Landlock present but the ruleset fails: exit with `FAILED` before stdin is read.
- Landlock present but the interpreter's installation is `/`, `/usr` or `/usr/local` (a system
  Python, not the managed one): exit with `FAILED`, because the CAS must run its own interpreter.
- Landlock absent (kernel before 5.13, or disabled): carry on; `confinement` is `no-landlock`.
- Not Linux: `confinement` is `not-linux` (Seatbelt or the AppContainer confines the server).

Raw syscalls through `ctypes`, no agent data. This module imports only `ctypes`, `os` and `sys`.
"""

import ctypes
import os
import sys

SYS_CREATE_RULESET = 444  # the same numbers on every Linux architecture
SYS_ADD_RULE = 445
SYS_RESTRICT_SELF = 446
CREATE_RULESET_VERSION = 1
ACCESS_FS_EXECUTE = 1
RULE_PATH_BENEATH = 1
PR_SET_NO_NEW_PRIVS = 38
PT_INTERP = 3

FAILED = 71  # the exit status when Landlock is present but the ruleset could not be applied
SYSTEM_PREFIXES = frozenset({"/", "/usr", "/usr/local"})  # a system Python: fail closed

# What `confine()` decided; the stdin reader refuses to read before it is set.
STATE = {"confinement": None}


class RulesetAttr(ctypes.Structure):
    _fields_ = [("handled_access_fs", ctypes.c_uint64)]


class PathBeneathAttr(ctypes.Structure):
    _pack_ = 1
    _fields_ = [("allowed_access", ctypes.c_uint64), ("parent_fd", ctypes.c_int32)]


if sys.platform.startswith("linux"):
    LIBC = ctypes.CDLL(None, use_errno=True)
    LIBC.syscall.restype = ctypes.c_long


def syscall(number: int, *args) -> int:
    result = LIBC.syscall(ctypes.c_long(number), *args)
    if result < 0:
        errno = ctypes.get_errno()
        raise OSError(errno, os.strerror(errno))
    return result


def abi() -> int:
    """The Landlock ABI version; `OSError` when Landlock is absent or disabled."""
    return syscall(SYS_CREATE_RULESET, None, ctypes.c_size_t(0), ctypes.c_uint32(1))


def elf_interpreter(path: str) -> str | None:
    """The `PT_INTERP` path of a 64-bit little-endian ELF file, or `None` (static, or not ELF)."""
    fd = os.open(path, os.O_RDONLY | os.O_CLOEXEC)
    try:
        header = os.pread(fd, 64, 0)
        if header[:4] != b"\x7fELF" or header[4] != 2 or header[5] != 1:
            return None
        offset = int.from_bytes(header[32:40], "little")
        size = int.from_bytes(header[54:56], "little")
        count = int.from_bytes(header[56:58], "little")
        for index in range(count):
            entry = os.pread(fd, size, offset + index * size)
            if int.from_bytes(entry[0:4], "little") == PT_INTERP:
                where = int.from_bytes(entry[8:16], "little")
                length = int.from_bytes(entry[32:40], "little")
                return os.pread(fd, length, where).rstrip(b"\0").decode()
        return None
    finally:
        os.close(fd)


def exec_roots() -> list[str]:
    """What stays executable: the interpreter's own file and its ELF loader.

    Raises `ValueError` when the interpreter is a system one (installed at `/`, `/usr` or
    `/usr/local`), so `confine()` fails closed."""
    prefix = os.path.realpath(sys.base_prefix)
    if prefix in SYSTEM_PREFIXES:
        raise ValueError(f"the interpreter is installed at {prefix}, not in the CAS environment")
    executable = os.path.realpath(sys.executable)
    roots = [executable]
    loader = elf_interpreter(executable)
    if loader is not None:
        roots.append(os.path.realpath(loader))
    return roots


def add_rule(ruleset: int, path: str) -> None:
    parent = os.open(path, os.O_PATH | os.O_CLOEXEC)
    try:
        rule = PathBeneathAttr(ACCESS_FS_EXECUTE, parent)
        syscall(SYS_ADD_RULE, ruleset, RULE_PATH_BENEATH, ctypes.byref(rule), ctypes.c_uint32(0))
    finally:
        os.close(parent)


def restrict_self(ruleset: int) -> None:
    if LIBC.prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0:
        errno = ctypes.get_errno()
        raise OSError(errno, os.strerror(errno))
    syscall(SYS_RESTRICT_SELF, ruleset, ctypes.c_uint32(0))


def apply(roots: list[str]) -> None:
    attr = RulesetAttr(ACCESS_FS_EXECUTE)
    ruleset = syscall(
        SYS_CREATE_RULESET, ctypes.byref(attr), ctypes.c_size_t(ctypes.sizeof(attr)), 0
    )
    try:
        for root in roots:
            add_rule(ruleset, root)
        restrict_self(ruleset)
    finally:
        os.close(ruleset)


def confine() -> str:
    """Apply the execute rule where Landlock exists; record and return the `confinement`."""
    if not sys.platform.startswith("linux"):
        STATE["confinement"] = "not-linux"
        return STATE["confinement"]
    try:
        abi()
    except OSError:
        STATE["confinement"] = "no-landlock"
        return STATE["confinement"]
    try:
        apply(exec_roots())
    except (OSError, ValueError) as exc:
        sys.stderr.write(f"imperium_cas: Landlock is present but the ruleset failed: {exc}\n")
        sys.stderr.flush()
        raise SystemExit(FAILED) from None
    STATE["confinement"] = "landlock"
    return STATE["confinement"]


def require_confined() -> str:
    """For the stdin reader: exit unless `confine()` ran first."""
    if STATE["confinement"] is None:
        sys.stderr.write("imperium_cas: stdin read before the Landlock ruleset\n")
        sys.stderr.flush()
        raise SystemExit(FAILED)
    return STATE["confinement"]
