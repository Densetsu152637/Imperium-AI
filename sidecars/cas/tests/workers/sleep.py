"""A test-only worker that blocks without using CPU once it has the call (plan stage 2b).

`time.sleep` raises an audit event, so it blocks on a lock it already holds instead.
"""

import sys
import threading

from imperium_cas import worker

LOCK = threading.Lock()


def block(request: dict) -> dict:
    LOCK.acquire()
    LOCK.acquire()  # never returns: only the front's wall clock or a cancel ends it
    return {"ok": True}


if __name__ == "__main__":
    worker.run(block, sys.argv[1:])
