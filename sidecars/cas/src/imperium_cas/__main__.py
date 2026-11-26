"""The CAS server's entry point: `python -I -B -m imperium_cas` (plan D3, D4).

The Landlock execute rule is applied by the first statements here, before any other import (the
MCP SDK, `pool`) and before stdin is read; `tests/test_landlock.py` checks both. Until stage 3
puts the MCP server here, the front serves length-prefixed request frames from stdin through the
pool, one reply frame each, until stdin ends.
"""

from imperium_cas.landlock import confine  # noqa: I001 - must stay first (plan D4, R2-3)

CONFINEMENT = confine()

import sys  # noqa: E402 - after the ruleset, on purpose

from imperium_cas.framing import REPLY_CAP, REQUEST_CAP, FrameError, read_frame, write_frame  # noqa: E402
from imperium_cas.landlock import require_confined  # noqa: E402
from imperium_cas.pool import Pool, default_spawner  # noqa: E402


def serve(stdin, stdout, pool) -> None:
    """Answer request frames until stdin ends; the Landlock record is checked before any read."""
    require_confined()
    while True:
        try:
            request = read_frame(stdin, REQUEST_CAP)
        except FrameError:
            return
        write_frame(stdout, pool.call(request), REPLY_CAP)


def main() -> None:
    pool = Pool(default_spawner())
    try:
        serve(sys.stdin.buffer, sys.stdout.buffer, pool)
    finally:
        pool.close()


if __name__ == "__main__":
    main()
