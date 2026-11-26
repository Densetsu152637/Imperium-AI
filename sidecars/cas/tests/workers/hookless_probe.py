"""A test-only worker whose audit hook is never installed (a regression stand-in).

The real worker must then refuse to read the request: it exits `CRASHED` after a ready frame
that reports `audit_hook: false`.
"""

import sys

from imperium_cas import worker

worker.install_audit_hook = lambda: None

if __name__ == "__main__":
    worker.run(worker.handle, sys.argv[1:])
