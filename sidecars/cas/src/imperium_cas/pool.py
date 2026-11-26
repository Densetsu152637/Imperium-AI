"""The front's side of the single-use workers (plan D3). It never imports SymPy or `worker`.

A `Pool` runs one call at a time, in the order the calls arrived. Each call takes the spare
worker (started lazily on the first call), starts the next spare at once, hands the worker the
request and maps how it ended:

- a reply and exit 0: the reply;
- `MEMORY`, or a kill by the macOS footprint poll: `limit` (memory);
- `CPU` or `SIGXCPU`, or a kill by the macOS CPU poll (macOS does not enforce the hard limit):
  `limit` (CPU);
- a `SIGKILL` the front did not send: `limit`, with a neutral message (usually the hard CPU
  limit, but the kernel's out-of-memory killer sends it too);
- a kill by the front's wall clock, counted from when the call was received (so time spent
  queued counts, and a call with no time left when its turn comes never reaches a worker):
  `limit` (wall clock);
- `cancel(call_id)`: the worker is killed and the call is `cancelled`, also when the cancel
  arrives while the worker is still being started;
- `REFUSED`: `refused`; anything else (`FRAME`, `CRASHED`, a signal): `crashed`;
- a ready frame that does not match (no audit hook, another PID, or on Linux no address-space
  limit): the worker is killed and the call is `crashed`;
- a usage poll that cannot read a live worker (macOS `proc_pid_rusage`): the worker is killed and
  the call is `refused`, because its memory would otherwise go unchecked.

Spawning is behind `Spawner`. `PosixSpawner` starts the base interpreter with `subprocess` on
Linux and macOS; the worker applies its own CPU and memory limits there (`limits_posix.py`),
and on macOS this front polls the memory footprint and CPU time (`limits_macos.py`), because
macOS enforces neither `RLIMIT_AS` nor the hard `RLIMIT_CPU`. On Windows the front cannot start
processes inside its sandbox, and the worker applies no limit itself, so production uses
`BrokerSpawner` (stage 5), whose broker puts each worker in a Job Object with the limits.
Until then `WindowsSpawner` puts each worker in a Job Object from the front
(`limits_windows.py`): the tests on Windows use it, and so does a front started with
`IMPERIUM_CAS_WINDOWS_JOBS=1`. `PosixSpawner` refuses Windows unless a test asks for unlimited
workers explicitly.
"""

import os
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
if sys.platform == "win32":
    from .limits_windows import CREATE_SUSPENDED, WorkerJob

WINDOWS_JOBS = "IMPERIUM_CAS_WINDOWS_JOBS"  # set to 1: the front's own Job Objects (see above)

POLL_SECONDS = 0.05  # the macOS footprint and CPU poll (D3)
CPU_GRACE = 1.0  # the poll kills this long past the CPU limit, as Linux's hard limit does
EXIT_GRACE = 0.2  # how long a worker whose usage read failed may take to show it has exited

# Exit statuses that mean the CPU limit: the worker's own status, and `SIGXCPU` when the handler
# did not run. A `SIGKILL` the front did not send is a limit, but not necessarily the CPU one.
CPU_STATUSES = {CPU} if sys.platform == "win32" else {CPU, -signal.SIGXCPU}
KILLED = None if sys.platform == "win32" else -signal.SIGKILL


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

    def poll(self) -> int | None: ...

    def wait(self, timeout: float | None = None) -> int: ...


class Spawner(Protocol):
    """Starts workers. `arm` runs when a worker reports ready, `exceeded` names the limit that
    ended it (if the spawner applied it), and `release` runs once its status is known."""

    def spawn(self, limits: Limits) -> WorkerProcess: ...

    def arm(self, proc: WorkerProcess) -> None: ...

    def exceeded(self, proc: WorkerProcess, status: int) -> str | None: ...

    def release(self, proc: WorkerProcess) -> None: ...


class PosixSpawner:
    """Start workers with `subprocess` (Linux and macOS).

    By default the worker is this front's own interpreter (the base interpreter, D3) under
    `-I -B`, with an empty environment (`-I` ignores every `PYTHON*` variable anyway, so no hash
    seed can be passed; the tools' outputs are canonical instead). Tests pass their own command
    and environment. On Windows it refuses unless `unlimited_on_windows` is set, because there
    the worker would run with no CPU or memory limit (only the wall clock).
    """

    def __init__(self, command=None, env=None, stderr=None, unlimited_on_windows=False):
        if sys.platform == "win32" and not unlimited_on_windows:
            raise RuntimeError("Windows workers need the broker (stage 5) for their limits")
        self.command = list(command or [sys.executable, "-I", "-B", "-m", "imperium_cas.worker"])
        self.env = {} if env is None else dict(env)
        self.stderr = subprocess.DEVNULL if stderr is None else stderr

    def spawn(self, limits: Limits, creationflags: int = 0) -> WorkerProcess:
        return subprocess.Popen(  # noqa: S603 - a fixed interpreter, module and limits
            [*self.command, *limits.argv()],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.stderr,
            env=self.env,
            close_fds=True,
            creationflags=creationflags,
        )

    def arm(self, proc: WorkerProcess) -> None:
        """Nothing to do: the worker applied its own limits (Unix)."""

    def exceeded(self, proc: WorkerProcess, status: int) -> str | None:
        return None

    def release(self, proc: WorkerProcess) -> None:
        """Nothing to release."""


class WindowsSpawner:
    """Windows before stage 5: each worker in its own Job Object with the call's CPU and memory
    limits. The worker is created suspended, assigned to its job and only then resumed, so it
    never runs outside the job (`limits_windows.py`).
    """

    def __init__(self, command=None, env=None, stderr=None):
        self.posix = PosixSpawner(command, env, stderr, unlimited_on_windows=True)
        self.command = self.posix.command
        self.jobs: dict = {}
        self.lock = threading.Lock()

    def spawn(self, limits: Limits) -> WorkerProcess:
        # Started suspended, put in its job, then resumed: it runs no instruction outside it.
        proc = self.posix.spawn(limits, creationflags=CREATE_SUSPENDED)
        try:
            job = WorkerJob(proc.pid, limits.cpu_seconds, limits.memory_mib)
            job.resume()
        except OSError:
            discard(proc)
            raise
        with self.lock:
            self.jobs[proc.pid] = job
        return proc

    def job(self, proc: WorkerProcess):
        with self.lock:
            return self.jobs.get(proc.pid)

    def arm(self, proc: WorkerProcess) -> None:
        job = self.job(proc)
        if job is None:
            raise OSError("the worker has no job")
        job.arm()

    def exceeded(self, proc: WorkerProcess, status: int) -> str | None:
        job = self.job(proc)
        return None if job is None else job.exceeded(status)

    def release(self, proc: WorkerProcess) -> None:
        with self.lock:
            job = self.jobs.pop(proc.pid, None)
        if job is not None:
            job.close()


class BrokerSpawner:
    """Windows: ask the broker for a worker in its own Job Object. Stage 5 fills this in."""

    def spawn(self, limits: Limits) -> WorkerProcess:
        raise NotImplementedError("the Windows broker is stage 5 of the CAS plan")

    def arm(self, proc: WorkerProcess) -> None:
        raise NotImplementedError("the Windows broker is stage 5 of the CAS plan")

    def exceeded(self, proc: WorkerProcess, status: int) -> str | None:
        return None

    def release(self, proc: WorkerProcess) -> None:
        """Nothing to release."""


def default_spawner(environ=None) -> Spawner:
    """Unix: `PosixSpawner`. Windows: the broker (stage 5), or with `IMPERIUM_CAS_WINDOWS_JOBS=1`
    in `environ` (default: this process's), `WindowsSpawner`. Plan D3 rejects front-side Job
    Objects for production, so they stay an explicit choice until the broker lands."""
    if sys.platform != "win32":
        return PosixSpawner()
    if (os.environ if environ is None else environ).get(WINDOWS_JOBS) == "1":
        return WindowsSpawner()
    return BrokerSpawner()


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
        # pid -> (footprint bytes, CPU seconds) or `None`, polled every `POLL_SECONDS`: macOS's
        # only memory limit, and its CPU backstop. A `None` for a live worker stops it.
        self.usage = usage or (macos_usage if sys.platform == "darwin" else None)
        self.spare: WorkerProcess | None = None
        self.running: Run | None = None
        self.current = None  # the call dequeued and not yet finished, so a cancel is never lost
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
            self.current = call_id
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
                self.current = None
                self.serving += 1
                self.cond.notify_all()

    def cancel(self, call_id) -> None:
        """Cancel a call: kill its worker if it is running, or drop it when its turn comes.

        A call between its dequeue and its worker being recorded (a slow spawn) is marked, and
        `run` kills the worker as soon as it records it."""
        if call_id is None:
            return
        with self.cond:
            if self.running is not None and self.running.call_id == call_id:
                self.running.stop("cancelled")
            elif call_id in self.queued or call_id == self.current:
                self.cancelled.add(call_id)

    def close(self) -> None:
        """Kill the spare and any running worker."""
        with self.cond:
            self.closed = True
            spare, self.spare = self.spare, None
            if self.running is not None:
                self.running.stop("cancelled")
        if spare is not None:
            discard(spare, self.spawner)

    def take(self) -> WorkerProcess:
        """The spare (or a new worker on the first call), and the next spare started at once.

        The new spare is recorded under the lock, or killed when the pool closed meanwhile."""
        with self.cond:
            proc, self.spare = self.spare, None
        if proc is None:
            proc = self.spawner.spawn(self.limits)
        try:
            spare = self.spawner.spawn(self.limits)
        except (OSError, NotImplementedError):
            return proc  # the next call starts its own
        with self.cond:
            if not self.closed:
                spare, self.spare = None, spare
        if spare is not None:
            discard(spare, self.spawner)
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
        watcher = None
        try:
            with self.cond:
                self.running = run
                if call_id is not None and call_id in self.cancelled:
                    run.stop("cancelled")  # cancelled while its worker was starting
            watcher = threading.Thread(target=self.watch, args=(run,), daemon=True)
            watcher.start()
            reply = self.exchange(run, payload)
            status = proc.wait()
            with run.lock:
                run.finished.set()
            return self.outcome(run, reply, status, self.spawner.exceeded(proc, status))
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.wait()
            self.spawner.release(proc)
            with run.lock:
                run.finished.set()
            if watcher is not None:
                watcher.join()
            with self.cond:
                self.running = None
            close_pipes(proc)

    def exchange(self, run: Run, payload: bytes) -> dict | None:
        """Check the ready frame, start the usage poll, send the call and read the reply."""
        proc = run.proc
        try:
            ready = read_frame(proc.stdout, REPLY_CAP)  # warm, limited and hooked
            if not ready_matches(ready, proc.pid):
                run.stop("ready")
                return None
            self.spawner.arm(proc)
            if self.usage is not None:
                baseline = self.usage(proc.pid)
                if baseline is None and self.still_alive(proc):
                    baseline = self.usage(proc.pid)  # one more try: it may have been exiting
                if baseline is None:
                    run.stop("unmonitored")
                    return None
                run.memory_limit = baseline[0] + self.limits.memory_mib * 1024 * 1024
                run.cpu_limit = baseline[1] + self.limits.cpu_seconds + CPU_GRACE
            proc.stdin.write(payload)
            proc.stdin.close()
            return read_frame(proc.stdout, REPLY_CAP)
        except (FrameError, OSError):
            return None

    def watch(self, run: Run) -> None:
        """Kill the worker at its deadline, or (macOS) when its footprint or CPU time is over, or
        when its usage cannot be read while it is alive."""
        while True:
            left = run.deadline - time.monotonic()
            wait = min(left, POLL_SECONDS) if self.usage is not None else left
            if run.finished.wait(max(wait, 0.0)):
                return
            if time.monotonic() >= run.deadline:
                run.stop("wall clock")
                return
            if run.memory_limit is None:
                continue
            used = self.usage(run.proc.pid)
            if used is None:
                if self.still_alive(run.proc):
                    run.stop("unmonitored")
                    return
                continue
            if used[0] > run.memory_limit:
                run.peak = used[0]
                run.stop("memory")
                return
            if used[1] > run.cpu_limit:
                run.stop("cpu")
                return

    def still_alive(self, proc: WorkerProcess) -> bool:
        """Whether a worker whose usage could not be read is still running. A worker that is
        exiting can fail the read before `poll` sees its status, so it gets a moment to exit."""
        try:
            proc.wait(EXIT_GRACE)
        except subprocess.TimeoutExpired:
            return True
        return False

    def wall_clock(self, queued: bool = False) -> dict:
        seconds = f"{self.limits.wall_seconds:g} s"
        shown = f"wall clock (queued, {seconds})" if queued else f"wall clock ({seconds})"
        return error("limit", shown, limit="wall clock")

    def outcome(self, run: Run, reply: dict | None, status: int, exceeded=None) -> dict:
        """How the call ended; `exceeded` is the limit the spawner's job saw (Windows)."""
        memory = error("limit", f"memory ({self.limits.memory_mib} MiB)", limit="memory")
        cpu = error("limit", f"CPU time ({self.limits.cpu_seconds} s)", limit="cpu")
        if run.reason == "cancelled":
            return error("cancelled", "the call was cancelled")
        if run.reason == "wall clock":
            return self.wall_clock()
        if run.reason == "memory":
            return {**memory, "footprint": run.peak, "footprint_limit": run.memory_limit}
        if run.reason == "cpu":
            return cpu
        if run.reason == "unmonitored":
            return error("refused", "the worker's memory use could not be read, so it was stopped")
        if run.reason == "ready":
            return error("crashed", "the worker did not report its audit hook and limits")
        if status == EXIT_OK and reply is not None:
            return reply
        if exceeded == "cpu":
            return cpu
        if status == MEMORY or exceeded == "memory":
            return memory
        if status in CPU_STATUSES:
            return cpu
        if status == KILLED:
            return error(
                "limit",
                f"the worker was killed by the system (the CPU limit of {self.limits.cpu_seconds}"
                " s, or memory)",
                limit="killed",
            )
        if status == REFUSED:
            event = reply.get("event") if isinstance(reply, dict) else None
            where = reply.get("where") if isinstance(reply, dict) else None
            shown = f" (audit event {event})" if isinstance(event, str) else ""
            if isinstance(where, str):
                shown = f" (audit event {event} from {where})"
            return error("refused", f"the call did something the CAS does not allow{shown}")
        return error("crashed", f"the worker exited with status {status}", status=status)


def ready_matches(ready: dict, pid: int) -> bool:
    """The ready frame says the hook is installed and names the PID the spawner holds; on Linux it
    also reports the address-space limit as applied."""
    if ready.get("ready") is not True or ready.get("audit_hook") is not True:
        return False
    if ready.get("pid") != pid:
        return False
    if sys.platform.startswith("linux"):
        applied = ready.get("applied")
        if not isinstance(applied, dict) or not isinstance(applied.get("address_space"), int):
            return False
    return True


def discard(proc: WorkerProcess, spawner=None) -> None:
    proc.kill()
    proc.wait()
    if spawner is not None:
        spawner.release(proc)
    close_pipes(proc)


def close_pipes(proc: WorkerProcess) -> None:
    for stream in (proc.stdin, proc.stdout):
        try:
            stream.close()
        except OSError:  # noqa: S110 - a pipe already closed
            pass
