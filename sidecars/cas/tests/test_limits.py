"""Stage 2b: the per-call limits through the front's pool (plan D3, goal 2).

The CPU spin, the fast allocation and the blocking sleep each end with `limit` naming their
limit. On Linux and macOS the worker applies its own limits; on Windows the test workers run in
the front's Job Objects (`WindowsSpawner`, until stage 5's broker). Measurements go to the "cas
measurements" section.
"""

import signal
import sys
import threading
import time

import pytest

from imperium_cas.framing import CPU, MEMORY, REPLY_CAP, encode_frame, read_frame
from imperium_cas.pool import BrokerSpawner, Limits, Pool, PosixSpawner, Run, default_spawner
from support import MEASUREMENTS, make_spawner

UNIX = pytest.mark.skipif(
    sys.platform == "win32", reason="Windows limits are the broker's Job Object (stage 5)"
)
SLACK = 2.0  # seconds of scheduling noise allowed on shared CI runners


def timed(pool: Pool, request: dict, **kwargs) -> tuple[dict, float]:
    """One call through the pool, with the spare already warm, and how long it took."""
    started = time.monotonic()
    reply = pool.call(request, received=started, **kwargs)
    return reply, time.monotonic() - started


def warm(pool: Pool) -> None:
    """Start the spare if needed and wait for its ready frame, so timings exclude the warm-up."""
    if pool.spare is None:
        pool.spare = pool.spawner.spawn(pool.limits)
    ready = read_frame(pool.spare.stdout, REPLY_CAP)
    # Put the ready frame back in front of the stream, as the pool expects to read it.
    pool.spare.stdout = Prepend(ready, pool.spare.stdout)


class Prepend:
    """A stream that yields one already-read frame before the rest of `stream`."""

    def __init__(self, frame: dict, stream):
        self.head = encode_frame(frame, REPLY_CAP)
        self.stream = stream

    def read(self, size: int = -1) -> bytes:
        if self.head:
            part, self.head = self.head[:size], self.head[size:]
            return part
        return self.stream.read(size)

    def close(self) -> None:
        self.stream.close()


@pytest.fixture
def pools():
    """Pools made by a test are closed (spare killed) when it ends."""
    made: list[Pool] = []

    def make(script=None, *extra, **limits) -> Pool:
        pool = Pool(make_spawner(script, *extra), Limits(**limits))
        made.append(pool)
        return pool

    yield make
    for pool in made:
        pool.close()


def test_a_cpu_spin_ends_with_the_cpu_limit(pools):
    pool = pools("spin.py", cpu_seconds=2, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"spin": True})
    assert reply["category"] == "limit" and reply["limit"] == "cpu", reply
    assert reply["message"] == "limit: CPU time (2 s)"
    # Soft limit = ceil(CPU used after warm-up) + 2 s, so at most 3 s of CPU after the call.
    # Windows checks a job's per-process time limit only every few seconds: allow that much.
    grace = 4 if sys.platform == "win32" else 1
    assert elapsed <= 2 + grace + SLACK, elapsed
    MEASUREMENTS.append(f"cpu spin, limit 2 s ({sys.platform}): stopped after {elapsed:.2f} s")


@UNIX
def test_the_hard_cpu_limit_stops_a_worker_that_never_runs_the_handler(pools):
    """Linux: the hard limit's `SIGKILL`. macOS does not kill at the hard limit, so there the
    front's CPU poll stops it one second past the limit."""
    pool = pools("spin.py", "--ignore-xcpu", cpu_seconds=2, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"spin": True})
    MEASUREMENTS.append(
        f"cpu spin ignoring SIGXCPU, limit 2 s ({sys.platform}): {reply['message']}"
        f" after {elapsed:.2f} s"
    )
    # Linux: an unsent `SIGKILL`, reported neutrally (the kernel's OOM killer sends it too).
    expected = "killed" if sys.platform.startswith("linux") else "cpu"
    assert reply["category"] == "limit" and reply["limit"] == expected, reply
    assert elapsed <= 2 + 2 + SLACK, elapsed


@pytest.mark.skipif(sys.platform != "darwin", reason="the macOS poll")
def test_the_macos_poll_reads_cpu_seconds_in_seconds():
    """The Mach-tick conversion: this process's CPU time from the poll matches `getrusage`."""
    import os
    import resource

    from imperium_cas.limits_macos import usage

    deadline = time.process_time() + 0.5
    while time.process_time() < deadline:
        pass
    own = resource.getrusage(resource.RUSAGE_SELF)
    polled = usage(os.getpid())[1]
    assert 0.8 < polled / (own.ru_utime + own.ru_stime) < 1.25, polled


def test_a_fast_allocation_ends_with_the_memory_limit(pools):
    pool = pools("alloc.py", memory_mib=256, wall_seconds=30)
    warm(pool)
    reply, elapsed = timed(pool, {"alloc": True})
    assert reply["category"] == "limit" and reply["limit"] == "memory", reply
    assert reply["message"] == "limit: memory (256 MiB)"
    assert elapsed <= 1 + SLACK, elapsed
    if sys.platform == "darwin":
        # The footprint poll, not a hard cap: record how far over the limit the worker got.
        over = (reply["footprint"] - reply["footprint_limit"]) / 2**20
        MEASUREMENTS.append(
            f"macOS memory poll, limit 256 MiB: killed {elapsed:.2f} s after the call,"
            f" {over:.0f} MiB over the limit at the poll that killed it"
        )
        assert over < 2048, over  # the bound 05 states
    else:
        MEASUREMENTS.append(
            f"memory, limit 256 MiB ({sys.platform}): MemoryError after {elapsed:.2f} s"
        )


def test_a_blocking_sleep_ends_with_the_wall_clock(pools):
    pool = pools("sleep.py", wall_seconds=2)
    warm(pool)
    reply, elapsed = timed(pool, {"sleep": True})
    assert reply == {
        "ok": False,
        "category": "limit",
        "message": "limit: wall clock (2 s)",
        "limit": "wall clock",
    }
    assert 2 <= elapsed <= 2 + 1 + SLACK, elapsed
    MEASUREMENTS.append(f"sleep, wall clock 2 s ({sys.platform}): stopped after {elapsed:.2f} s")


@pytest.mark.skipif(sys.platform != "win32", reason="the Windows Job Object")
def test_on_windows_the_job_holds_the_worker_from_the_start():
    """The worker is in its job as soon as it is started, before it reads its call, and
    closing the job's handle kills it."""
    spawner = make_spawner("sleep.py")
    proc = spawner.spawn(Limits(cpu_seconds=3, memory_mib=256))
    try:
        job = spawner.job(proc)
        assert job is not None and job.memory_limit > 256 * 2**20
        spawner.release(proc)
        proc.wait(10)
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait()


def test_default_spawner_on_windows_takes_jobs_only_when_asked():
    if sys.platform != "win32":
        return
    from imperium_cas.pool import WindowsSpawner

    assert isinstance(default_spawner({}), BrokerSpawner)
    assert isinstance(default_spawner({"IMPERIUM_CAS_WINDOWS_JOBS": "1"}), WindowsSpawner)


def test_posix_spawner_refuses_windows_without_the_opt_in():
    if sys.platform == "win32":
        with pytest.raises(RuntimeError, match="broker"):
            PosixSpawner()
    else:
        assert PosixSpawner().command[1:] == ["-I", "-B", "-m", "imperium_cas.worker"]


GOAL_2 = (
    "10^10^10",
    "1e999999999",
    "sqrt(2)^(10^6)",
    "factorial(factorial(20))",
    "factorial(10^7)",
    "expand((x+y+z)^5000)",
    "integrate(exp(x^x^x), x)",
    "[[2,1],[1,1]]^(10^6)",
    "(" * 50_000 + "1" + ")" * 50_000,
    "det([" + ",".join("[" + ",".join(f"x+{i}*{j}" for j in range(30)) + "]" for i in range(30)),
)


def test_goal_2_inputs_are_refused_or_stopped_and_the_next_call_works(pools):
    """Each goal 2 input gives `guard`, `parse` or `limit` within the wall clock plus 1 s.

    Stage 2b has only the `build` call, so `expand`, `integrate` and `det` are unknown names
    (`parse`); stage 3 runs them again through its tools."""
    pool = pools(cpu_seconds=5, wall_seconds=10)
    for expression in GOAL_2:
        reply, elapsed = timed(pool, {"tool": "build", "expression": expression})
        assert reply["category"] in {"guard", "parse", "limit"}, (expression[:40], reply)
        assert elapsed <= 10 + 1 + SLACK, (expression[:40], elapsed)
    reply, _ = timed(pool, {"tool": "build", "expression": "x^2 + 2x"})
    assert reply == {"ok": True, "text": "x**2 + 2*x"}


def test_a_normal_call_succeeds_right_after_each_limit(pools):
    for script, limits in (
        ("spin.py", {"cpu_seconds": 1}),
        ("alloc.py", {"memory_mib": 128}),
        ("sleep.py", {"wall_seconds": 1}),
    ):
        pool = pools(script, **limits)
        reply, _ = timed(pool, {"go": True})
        assert reply["category"] == "limit", (script, reply)
    pool = pools(cpu_seconds=1, memory_mib=128, wall_seconds=5)
    reply, _ = timed(pool, {"tool": "build", "expression": "factorial(20)"})
    assert reply == {"ok": True, "text": "2432902008176640000"}


def test_time_spent_queued_counts_against_the_deadline(pools):
    pool = pools("sleep.py", wall_seconds=2)
    warm(pool)
    received = time.monotonic()
    results: dict[str, dict] = {}

    def submit(name: str) -> None:
        results[name] = pool.call({"sleep": True}, call_id=name, received=received)

    first = threading.Thread(target=submit, args=("first",))
    first.start()
    while pool.running is None:
        time.sleep(0.01)
    second = threading.Thread(target=submit, args=("second",))
    second.start()
    first.join(30)
    second.join(30)
    assert results["first"]["message"] == "limit: wall clock (2 s)"
    assert results["second"]["message"] == "limit: wall clock (queued, 2 s)"
    assert time.monotonic() - received <= 2 + 1 + SLACK


def test_a_cancel_kills_the_running_worker(pools):
    pool = pools("sleep.py", wall_seconds=60)
    warm(pool)
    result: dict = {}
    thread = threading.Thread(target=lambda: result.update(pool.call({}, call_id=7)))
    thread.start()
    while pool.running is None:
        time.sleep(0.01)
    proc = pool.running.proc
    started = time.monotonic()
    pool.cancel(7)
    thread.join(30)
    assert result == {
        "ok": False,
        "category": "cancelled",
        "message": "cancelled: the call was cancelled",
    }
    assert proc.poll() is not None  # the worker is gone
    assert time.monotonic() - started <= 1 + SLACK


def test_a_cancel_of_a_queued_call_drops_it_before_it_reaches_a_worker(pools):
    pool = pools("sleep.py", wall_seconds=3)
    warm(pool)
    results: dict = {}

    def submit(name: str) -> None:
        results[name] = pool.call({}, call_id=name)

    first = threading.Thread(target=submit, args=("first",))
    first.start()
    while pool.running is None:
        time.sleep(0.01)
    second = threading.Thread(target=submit, args=("second",))
    second.start()
    while "second" not in pool.queued:
        time.sleep(0.01)
    pool.cancel("second")
    first.join(30)
    second.join(30)
    assert results["first"]["limit"] == "wall clock"
    assert results["second"]["category"] == "cancelled"


def test_exit_codes_map_to_categories():
    pool = Pool(make_spawner(), Limits())

    def outcome(code: int) -> dict:
        return pool.outcome(Run(None, None, 0.0), None, code)

    assert outcome(MEMORY)["limit"] == "memory"
    assert outcome(CPU)["limit"] == "cpu"
    assert outcome(77)["category"] == "refused"
    assert outcome(70)["category"] == "crashed"
    assert outcome(65)["category"] == "crashed"
    assert outcome(0)["category"] == "crashed"  # exit 0 with no reply
    if sys.platform != "win32":
        assert outcome(-signal.SIGXCPU)["limit"] == "cpu"
        killed = outcome(-signal.SIGKILL)  # usually the hard CPU limit, but not only
        assert killed["category"] == "limit" and killed["limit"] == "killed"
        assert "killed by the system" in killed["message"] and "or memory" in killed["message"]
        assert outcome(-signal.SIGSEGV)["category"] == "crashed"


def test_limits_are_range_checked():
    for bad in ({"cpu_seconds": 0}, {"cpu_seconds": 121}, {"memory_mib": 63}):
        with pytest.raises(ValueError):
            Limits(**bad)
    with pytest.raises(ValueError):
        Limits(wall_seconds=301)
    assert Limits().argv() == ["--cpu-seconds", "15", "--memory-mib", "1024"]


def test_overhead_of_a_call_through_the_pool(pools):
    """Lazy start (the first call pays the warm-up), then a warm spare per call."""
    pool = pools()
    started = time.monotonic()
    first = pool.call({"tool": "build", "expression": "x + 1"})
    cold = time.monotonic() - started
    assert first == {"ok": True, "text": "x + 1"}
    timings = []
    for _ in range(3):
        warm(pool)
        reply, elapsed = timed(pool, {"tool": "build", "expression": "(x+1)^2"})
        assert reply == {"ok": True, "text": "(x + 1)**2"}
        timings.append(elapsed)
    MEASUREMENTS.append(
        f"pool ({sys.platform}): first call {cold:.2f} s (lazy start), warm calls"
        f" {min(timings) * 1000:.0f} to {max(timings) * 1000:.0f} ms"
    )


# --- The 2b security review: lost cancels, fail-open polls, leaks and the ready frame ---


class Wrapped:
    """A spawner around the test one: optionally slow, with a hook on each spawn."""

    def __init__(self, inner, delay: float = 0.0, on_spawn=None):
        self.inner, self.delay, self.on_spawn, self.spawned = inner, delay, on_spawn, []

    def arm(self, proc):
        self.inner.arm(proc)

    def exceeded(self, proc, status):
        return self.inner.exceeded(proc, status)

    def release(self, proc):
        self.inner.release(proc)

    def spawn(self, limits):
        time.sleep(self.delay)
        proc = self.inner.spawn(limits)
        self.spawned.append(proc)
        if self.on_spawn is not None:
            proc = self.on_spawn(len(self.spawned), proc) or proc
        return proc


def test_a_cancel_while_the_worker_is_starting_is_not_lost():
    spawner = Wrapped(make_spawner("sleep.py"), delay=1.0)
    pool = Pool(spawner, Limits(wall_seconds=60))
    result: dict = {}
    thread = threading.Thread(target=lambda: result.update(pool.call({}, call_id="slow")))
    try:
        thread.start()
        while pool.current != "slow":
            time.sleep(0.005)
        assert pool.running is None  # dequeued, but its worker is still being spawned
        started = time.monotonic()
        pool.cancel("slow")
        thread.join(30)
        assert result["category"] == "cancelled", result
        assert time.monotonic() - started < 2 + 1 + SLACK  # two slow spawns, then killed at once
        assert spawner.spawned[0].poll() is not None
    finally:
        pool.close()


def test_a_usage_poll_that_cannot_read_the_worker_refuses_the_call():
    """The macOS poll failing open: a baseline of `None` stops the worker before the call."""
    pool = Pool(make_spawner("sleep.py"), Limits(wall_seconds=30), usage=lambda pid: None)
    try:
        started = time.monotonic()
        reply = pool.call({})
        assert reply["category"] == "refused" and "could not be read" in reply["message"]
        assert time.monotonic() - started < 10 + SLACK
    finally:
        pool.close()


def test_a_usage_poll_lost_while_the_worker_runs_refuses_the_call():
    answers = iter([(1, 0.0)])
    pool = Pool(
        make_spawner("sleep.py"), Limits(wall_seconds=30), usage=lambda pid: next(answers, None)
    )
    try:
        started = time.monotonic()
        reply = pool.call({})
        assert reply["category"] == "refused", reply
        assert time.monotonic() - started < 10 + SLACK
    finally:
        pool.close()


class OtherPid:
    def __init__(self, proc):
        self.proc = proc
        self.pid = proc.pid + 1
        self.stdin, self.stdout = proc.stdin, proc.stdout

    def kill(self):
        self.proc.kill()

    def poll(self):
        return self.proc.poll()

    def wait(self, timeout=None):
        return self.proc.wait(timeout)


def test_a_ready_frame_that_does_not_match_is_crashed():
    spawner = Wrapped(make_spawner(), on_spawn=lambda count, proc: OtherPid(proc))
    pool = Pool(spawner, Limits(wall_seconds=60))
    try:
        reply = pool.call({"tool": "build", "expression": "1"})
        assert reply["category"] == "crashed" and "audit hook and limits" in reply["message"]
        assert spawner.spawned[0].poll() is not None
    finally:
        pool.close()


def test_the_ready_frame_check():
    from imperium_cas.pool import ready_matches

    applied = {"address_space": 1 << 30}
    good = {"ready": True, "audit_hook": True, "pid": 7, "applied": applied}
    assert ready_matches(good, 7)
    assert not ready_matches(good, 8)
    assert not ready_matches({**good, "audit_hook": False}, 7)
    if sys.platform.startswith("linux"):
        assert not ready_matches({**good, "applied": {"address_space": None}}, 7)


def test_an_error_after_the_worker_starts_still_kills_it(monkeypatch):
    spawner = Wrapped(make_spawner("sleep.py"))
    pool = Pool(spawner, Limits(wall_seconds=60))

    def broken(run, payload):
        raise RuntimeError("an unexpected failure in the front")

    monkeypatch.setattr(pool, "exchange", broken)
    try:
        with pytest.raises(RuntimeError):
            pool.call({})
        assert spawner.spawned[0].poll() is not None
        assert pool.running is None
    finally:
        pool.close()


def test_a_spare_started_while_the_pool_closes_is_killed():
    pool = None

    def close_during_the_spare(count, proc):
        if count == 2:
            pool.close()

    spawner = Wrapped(make_spawner(), on_spawn=close_during_the_spare)
    pool = Pool(spawner, Limits(wall_seconds=60))
    pool.call({"tool": "build", "expression": "1"})
    assert pool.spare is None
    assert all(proc.poll() is not None for proc in spawner.spawned)


def test_default_spawner_on_windows_is_the_broker_stub():
    if sys.platform != "win32":
        assert isinstance(default_spawner(), PosixSpawner)
        return
    assert isinstance(default_spawner({}), BrokerSpawner)
    reply = Pool(default_spawner({})).call({"tool": "build", "expression": "1"})
    assert reply["category"] == "crashed" and "stage 5" in reply["message"]


def test_isolated_workers_give_the_same_reply():
    """`-I` ignores `PYTHONHASHSEED`, so each worker has its own hash seed; set-valued results
    are canonical, so two workers answer alike."""
    from support import isolated_spawner

    calls = [
        ("solve", {"equations": ["x^2 + y^2 = 5", "x*y = 2"]}),
        ("matrix", {"matrix": "[[2, 0, 0], [0, 3, 4], [0, 4, 9]]", "operation": "eigenvectors"}),
        ("equal", {"left": "sqrt(a^2) + b + c", "right": "a + b + c"}),
        ("simplify", {"expression": "z + y + x + w*v*u"}),
    ]
    first, second = (Pool(isolated_spawner(), Limits(wall_seconds=60)) for _ in range(2))
    try:
        for name, args in calls:
            request = {"tool": name, "args": args}
            one, two = first.call(request), second.call(request)
            assert one["ok"], one
            assert one == two, name
    finally:
        first.close()
        second.close()
