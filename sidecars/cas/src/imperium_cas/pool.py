"""The front's side of the single-use workers (plan D3). It never imports SymPy or `worker`.

A `Pool` runs one call at a time, in the order the calls arrived. Each call takes the spare
worker (started lazily on the first call), starts the next spare at once, hands the worker the
request and maps how it ended:

- a reply and exit 0: the reply;
- `MEMORY`, or a kill by the macOS footprint poll: `limit` (memory);
- `CPU`, `SIGXCPU` or a `SIGKILL` the front did not send (the hard CPU limit), or a kill by the
  macOS CPU poll (macOS does not enforce the hard limit): `limit` (CPU);
- a kill by the front's wall clock, counted from when the call was received (so time spent
  queued counts, and a call with no time left when its turn comes never reaches a worker):
  `limit` (wall clock);
- `cancel(call_id)`: the worker is killed and the call is `cancelled`;
- `REFUSED`: `refused`; anything else (`FRAME`, `CRASHED`, a signal): `crashed`.

Spawning is behind `Spawner`. `PosixSpawner` starts the base interpreter with `subprocess` on
Linux and macOS; the worker applies its own CPU and memory limits there (`limits_posix.py`),
and on macOS this front polls the memory footprint and CPU time (`limits_macos.py`), because
macOS enforces neither `RLIMIT_AS` nor the hard `RLIMIT_CPU`. On Windows the front cannot start
processes inside its sandbox, and the worker applies no limit itself, so production uses
`BrokerSpawner` (stage 5), whose broker puts each worker in a Job Object with the limits.
`PosixSpawner` refuses Windows unless a test asks for unlimited workers explicitly.
"""

import signal
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from typing import Protocol

from .framing import (
    CPU,
    EXIT_OK,
    LIMITS,
    MEMORY,
    REFUSED,
    REPLY_CAP,
    REQUEST_CAP,
    WALL_SECONDS,
    FrameError,
    encode_frame,
    read_frame,
)

if sys.platform == "darwin":
    from .limits_macos import usage as macos_usage

POLL_SECONDS = 0.05  # the macOS footprint and CPU poll (D3)
CPU_GRACE = 1.0  # the poll kills this long past the CPU limit, as Linux's hard limit does

# Exit statuses that mean the CPU limit: the worker's own status, `SIGXCPU` when the handler did
# not run, and the hard limit's `SIGKILL` (when the front did not send it).
CPU_STATUSES = {CPU} if sys.platform == "win32" else {CPU, -signal.SIGXCPU, -signal.SIGKILL}


@dataclass(frozen=True)
class Limits:
    """One call's limits (D3's defaults; the user layer may lower them, or raise them up to the
    caps)."""

    cpu_seconds: int = LIMITS["--cpu-seconds"][2]
    memory_mib: int = LIMITS["--memory-mib"][2]
    wall_seconds: float = WALL_SECONDS[2]

    def __post_init__(self) -> None:
        checks = (
            ("cpu_seconds", self.cpu_seconds, LIMITS["--cpu-seconds"]),
            ("memory_mib", self.memory_mib, LIMITS["--memory-mib"]),
        )
        for name, value, (low, high, _) in checks:
            if not isinstance(value, int) or not low <= value <= high:
                raise ValueError(f"{name} must be a whole number from {low} to {high}")
        if not 0 < self.wall_seconds <= WALL_SECONDS[1]:
            raise ValueError(f"wall_seconds must be over 0 and at most {WALL_SECONDS[1]}")

    def argv(self) -> list[str]:
        """The worker's command-line limits."""
        return ["--cpu-seconds", str(self.cpu_seconds), "--memory-mib", str(self.memory_mib)]


class WorkerProcess(Protocol):
    """A started worker: what `subprocess.Popen` provides."""

    pid: int
    stdin: object
    stdout: object

    def kill(self) -> None: ...

    def wait(self, timeout: float | None = None) -> int: ...


class Spawner(Protocol):
    def spawn(self, limits: Limits) -> WorkerProcess: ...


class PosixSpawner:
    """Start workers with `subprocess` (Linux and macOS).

    By default the worker is this front's own interpreter (the base interpreter, D3) under
    `-I -B`, with only `PYTHONHASHSEED` in its environment. Tests pass their own command and
    environment. On Windows it refuses unless `unlimited_on_windows` is set, because there the
    worker would run with no CPU or memory limit (only the wall clock).
    """

    def __init__(self, command=None, env=None, stderr=None, unlimited_on_windows=False):
        if sys.platform == "win32" and not unlimited_on_windows:
            raise RuntimeError("Windows workers need the broker (stage 5) for their limits")
        self.command = list(command or [sys.executable, "-I", "-B", "-m", "imperium_cas.worker"])
        self.env = {"PYTHONHASHSEED": "0"} if env is None else dict(env)
        self.stderr = subprocess.DEVNULL if stderr is None else stderr

    def spawn(self, limits: Limits) -> WorkerProcess:
        return subprocess.Popen(  # noqa: S603 - a fixed interpreter, module and limits
            [*self.command, *limits.argv()],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            env=self.env,
            close_fds=True,
        )


class BrokerSpawner:
    """Windows: ask the broker for a worker in its own Job Object. Stage 5 fills this in."""

    def spawn(self, limits: Limits) -> WorkerProcess:
        raise NotImplementedError("the Windows broker is stage 5 of the CAS plan")


def default_spawner() -> Spawner:
    return BrokerSpawner() if sys.platform == "win32" else PosixSpawner()


def error(category: str, message: str, **extra) -> dict:
    return {"ok": False, "category": category, "message": f"{category}: {message}", **extra}


class Run:
    """One call on one worker: the deadline, the memory poll and why the front stopped it."""

    def __init__(self, proc: WorkerProcess, call_id, deadline: float):
        self.proc = proc
        self.call_id = call_id
        self.deadline = deadline
        self.memory_limit: int | None = None  # bytes of footprint, set when the call starts
        self.cpu_limit: float | None = None  # CPU seconds, set when the call starts
        self.peak: int | None = None  # the footprint seen when the poll killed the worker
        self.reason: str | None = None
        self.finished = threading.Event()
        self.lock = threading.Lock()

    def stop(self, reason: str) -> None:
        """Kill the worker once; the first reason wins."""
        with self.lock:
            if self.reason is None and not self.finished.is_set():
                self.reason = reason
                self.proc.kill()


class Pool:
    def __init__(self, spawner: Spawner, limits: Limits | None = None, usage=None):
        self.spawner = spawner
        self.limits = limits or Limits()
        # pid -> (footprint bytes, CPU seconds), polled every `POLL_SECONDS`: macOS's only memory
        # limit, and its CPU backstop.
        self.usage = usage or (macos_usage if sys.platform == "darwin" else None)
        self.spare: WorkerProcess | None = None
        self.running: Run | None = None
        self.queued: set = set()
        self.cancelled: set = set()
        self.next_ticket = 0
        self.serving = 0
        self.cond = threading.Condition()
        self.closed = False

    def call(self, request: dict, call_id=None, received: float | None = None) -> dict:
        """Run one call and return its reply or error. `received` is the `time.monotonic()` at
        which the front received the call; the wall clock counts from it."""
        received = time.monotonic() if received is None else received
        deadline = received + self.limits.wall_seconds
        with self.cond:
            ticket = self.next_ticket
            self.next_ticket += 1
            if call_id is not None:
                self.queued.add(call_id)
            while ticket != self.serving:
                self.cond.wait()
            self.queued.discard(call_id)
        try:
            if call_id is not None and call_id in self.cancelled:
                return error("cancelled", "the call was cancelled")
            if self.closed:
                return error("crashed", "the server is shutting down")
            if time.monotonic() >= deadline:
                return self.wall_clock(queued=True)
            return self.run(request, call_id, deadline)
        finally:
            with self.cond:
                self.cancelled.discard(call_id)
                self.serving += 1
                self.cond.notify_all()

    def cancel(self, call_id) -> None:
        """Cancel a call: kill its worker if it is running, or drop it when its turn comes."""
        with self.cond:
            if self.running is not None and self.running.call_id == call_id:
                self.running.stop("cancelled")
            elif call_id in self.queued:
                self.cancelled.add(call_id)

    def close(self) -> None:
        """Kill the spare and any running worker."""
        with self.cond:
            self.closed = True
            spare, self.spare = self.spare, None
            if self.running is not None:
                self.running.stop("cancelled")
        if spare is not None:
            spare.kill()
            spare.wait()
            close_pipes(spare)

    def take(self) -> WorkerProcess:
        """The spare (or a new worker on the first call), and the next spare started at once."""
        proc = self.spare or self.spawner.spawn(self.limits)
        self.spare = None
        try:
            self.spare = self.spawner.spawn(self.limits)
        except (OSError, NotImplementedError):  # noqa: S110 - the next call starts its own
            pass
        return proc

    def run(self, request: dict, call_id, deadline: float) -> dict:
        try:
            payload = encode_frame(request, REQUEST_CAP)
        except FrameError:
            return error("parse", "the request is over 1 MiB")
        try:
            proc = self.take()
        except (OSError, NotImplementedError) as exc:
            return error("crashed", f"no worker could be started ({exc})")
        run = Run(proc, call_id, deadline)
        with self.cond:
            self.running = run
        watcher = threading.Thread(target=self.watch, args=(run,), daemon=True)
        watcher.start()
        reply = None
        try:
            read_frame(proc.stdout, REPLY_CAP)  # the ready frame: warm, limited and hooked
            if self.usage is not None:
                baseline = self.usage(proc.pid)
                if baseline is not None:
                    run.memory_limit = baseline[0] + self.limits.memory_mib * 1024 * 1024
                    run.cpu_limit = baseline[1] + self.limits.cpu_seconds + CPU_GRACE
            proc.stdin.write(payload)
            proc.stdin.close()
            reply = read_frame(proc.stdout, REPLY_CAP)
        except (FrameError, OSError):
            reply = None
        status = proc.wait()
        with run.lock:
            run.finished.set()
        watcher.join()
        with self.cond:
            self.running = None
        close_pipes(proc)
        return self.outcome(run, reply, status)

    def watch(self, run: Run) -> None:
        """Kill the worker at its deadline, or (macOS) when its footprint or CPU time is over."""
        while True:
            left = run.deadline - time.monotonic()
            wait = min(left, POLL_SECONDS) if self.usage is not None else left
            if run.finished.wait(max(wait, 0.0)):
                return
            if time.monotonic() >= run.deadline:
                run.stop("wall clock")
                return
            if run.memory_limit is not None:
                used = self.usage(run.proc.pid)
                if used is not None and used[0] > run.memory_limit:
                    run.peak = used[0]
                    run.stop("memory")
                    return
                if used is not None and used[1] > run.cpu_limit:
                    run.stop("cpu")
                    return

    def wall_clock(self, queued: bool = False) -> dict:
        seconds = f"{self.limits.wall_seconds:g} s"
        shown = f"wall clock (queued, {seconds})" if queued else f"wall clock ({seconds})"
        return error("limit", shown, limit="wall clock")

    def outcome(self, run: Run, reply: dict | None, status: int) -> dict:
        memory = error("limit", f"memory ({self.limits.memory_mib} MiB)", limit="memory")
        if run.reason == "cancelled":
            return error("cancelled", "the call was cancelled")
        if run.reason == "wall clock":
            return self.wall_clock()
        if run.reason == "memory":
            return {**memory, "footprint": run.peak, "footprint_limit": run.memory_limit}
        if run.reason == "cpu":
            return error("limit", f"CPU time ({self.limits.cpu_seconds} s)", limit="cpu")
        if status == EXIT_OK and reply is not None:
            return reply
        if status == MEMORY:
            return memory
        if status in CPU_STATUSES:
            return error("limit", f"CPU time ({self.limits.cpu_seconds} s)", limit="cpu")
        if status == REFUSED:
            return error("refused", "the call did something the CAS does not allow")
        return error("crashed", f"the worker exited with status {status}", status=status)


def close_pipes(proc: WorkerProcess) -> None:
    for stream in (proc.stdin, proc.stdout):
        try:
            stream.close()
        except OSError:  # noqa: S110 - a pipe already closed
            pass
