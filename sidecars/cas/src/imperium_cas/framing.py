"""Length-prefixed JSON frames between the front and a worker (plan D3, pipe framing).

A frame is a 4-byte big-endian length, then that many bytes of UTF-8 JSON holding one object.
The reader checks the length against its cap before it reads or allocates the body, so a hostile
or broken prefix costs four bytes: a request is at most 1 MiB, a reply at most 256 KiB.

The worker's exit codes and the limits' bounds live here too, because the front (`pool.py`)
shares them with the worker but must never import it: the worker imports SymPy.
"""

import json

REQUEST_CAP = 1024 * 1024
REPLY_CAP = 256 * 1024
PREFIX = 4

# The worker's exit codes (`worker.py` explains each); `pool.py` maps them to error categories.
EXIT_OK = 0
USAGE = 64  # a bad command line
FRAME = 65  # a request frame over its cap, truncated or not JSON
MEMORY = 66  # a `MemoryError` after the hook: the memory limit (`RLIMIT_AS` on Linux)
CPU = 67  # `SIGXCPU`: the CPU limit's soft limit (its hard limit is a `SIGKILL`)
CRASHED = 70  # any other exception after the hook, or a request read without the hook
REFUSED = 77  # the audit hook saw an event outside the allow-list

# Command-line limits: name -> (smallest, largest, default), from plan D3.
LIMITS = {"--cpu-seconds": (1, 120, 15), "--memory-mib": (64, 8192, 1024)}
WALL_SECONDS = (1, 300, 20)  # the front's wall clock: smallest, largest, default


class FrameError(Exception):
    """A frame that is truncated, over its cap, or not one JSON object."""


def read_exact(stream, count: int) -> bytes:
    """Read exactly `count` bytes, or raise `FrameError` at end of stream."""
    chunks: list[bytes] = []
    left = count
    while left:
        chunk = stream.read(left)
        if not chunk:
            raise FrameError(f"the stream ended {left} bytes short of a frame")
        chunks.append(chunk)
        left -= len(chunk)
    return b"".join(chunks)


def read_frame(stream, cap: int) -> dict:
    """Read one frame. The length is checked against `cap` before the body is read."""
    length = int.from_bytes(read_exact(stream, PREFIX), "big")
    if length > cap:
        raise FrameError(f"a frame of {length} bytes is over the cap of {cap}")
    body = read_exact(stream, length)
    try:
        value = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, ValueError, RecursionError) as exc:
        raise FrameError(f"a frame is not valid JSON: {exc}") from None
    if not isinstance(value, dict):
        raise FrameError("a frame must hold one JSON object")
    return value


def encode_frame(value: dict, cap: int) -> bytes:
    """Encode one frame, refusing a body over `cap`."""
    body = json.dumps(value, ensure_ascii=True, separators=(",", ":")).encode("ascii")
    if len(body) > cap:
        raise FrameError(f"a frame of {len(body)} bytes is over the cap of {cap}")
    return len(body).to_bytes(PREFIX, "big") + body


def write_frame(stream, value: dict, cap: int) -> None:
    stream.write(encode_frame(value, cap))
    stream.flush()
