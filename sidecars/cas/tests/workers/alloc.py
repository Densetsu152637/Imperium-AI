"""A test-only worker that allocates as fast as it can once it has the call (plan stage 2b).

Each chunk is written (not zero pages), so it counts in the resident size and the footprint.
"""

import sys

from imperium_cas import worker

CHUNK = 16 * 1024 * 1024


def alloc(request: dict) -> dict:
    chunks = []
    while True:
        chunks.append(b"\x5a" * CHUNK)


if __name__ == "__main__":
    worker.run(alloc, sys.argv[1:])
