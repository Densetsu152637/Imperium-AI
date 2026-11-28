"""The CAS server's entry point: `python -I -B -m imperium_cas` (plan D3, D4, D6).

The Landlock execute rule is applied by the first statements here, before any other import (the
MCP SDK, `pool`) and before stdin is read; `tests/test_landlock.py` checks both. Then the MCP
server (`server.py`) serves stdin and stdout through the pool until stdin ends.
"""

from imperium_cas.landlock import confine  # noqa: I001 - must stay first (plan D4, R2-3)

CONFINEMENT = confine()

from imperium_cas.landlock import require_confined  # noqa: E402 - after the ruleset, on purpose
from imperium_cas.pool import Pool, default_spawner  # noqa: E402
from imperium_cas.server import run_stdio  # noqa: E402


def serve(pool) -> None:
    """Serve MCP until stdin ends; the Landlock record is checked before any read."""
    confinement = require_confined()
    run_stdio(pool, confinement)


def main() -> None:
    pool = Pool(default_spawner())
    try:
        serve(pool)
    finally:
        pool.close()


if __name__ == "__main__":
    main()
