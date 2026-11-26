"""Stage 3: the MCP front (plan D6), through the SDK's in-memory client and a real stdio process.

The in-memory tests run the real server and pool, whose workers are real hooked workers. The
whole hostile corpus goes through `tools/call` (each line as the main argument of one tool, in
turn), and no reply may be `refused` (an audit event outside the allow-list) or `crashed`. The
stdio test starts `python -I -B -m imperium_cas` on Linux and macOS through a bootstrap (the
package is not installed; stage 7 covers the real launch through the `.pth`), and on Windows it
waits for stage 7, because the server needs the broker there.
"""

import json
import re
import subprocess
import sys
import threading
import time

import anyio
import pytest
from mcp import Client

from imperium_cas import landlock, tools
from imperium_cas.parse import is_identifier
from imperium_cas.pool import Limits, Pool
from imperium_cas.server import (
    NAME_PATTERN,
    NO_LANDLOCK_NOTE,
    TOOLS,
    confinement_middleware,
    make_server,
)
from support import (  # noqa: I001
    MEASUREMENTS,
    SRC,
    WORKER_TIMEOUT,
    isolated_command,
    make_spawner,
    tool_requests,
)
from test_hostile import ENTRIES

TOOL_LIST_BUDGET = 7_000  # characters of what a model sees: names, descriptions, input schemas
CORPUS_CHUNKS = 4
# Servers per corpus chunk, each with its own pool, so the chunk runs in parallel (two on
# Windows, where the test run shares one Job Object with a memory cap).
PARALLEL = 2 if sys.platform == "win32" else 4


def run(coroutine_function, *args):
    return anyio.run(coroutine_function, *args)


@pytest.fixture
def pools():
    made: list[Pool] = []

    def make(script=None, **limits) -> Pool:
        pool = Pool(make_spawner(script), Limits(**limits))
        made.append(pool)
        return pool

    yield make
    for pool in made:
        pool.close()


def schemas_of(node) -> list[dict]:
    """Every schema object under `node`."""
    found = []
    if isinstance(node, dict):
        found.append(node)
        for value in node.values():
            found.extend(schemas_of(value))
    elif isinstance(node, list):
        for value in node:
            found.extend(schemas_of(value))
    return found


def test_the_tool_list_is_the_eleven_tools_with_strict_schemas():
    assert [item.name for item in TOOLS] == list(tools.TOOLS)
    for item in TOOLS:
        schema = item.input_schema
        assert schema["type"] == "object" and schema["additionalProperties"] is False
        assert item.output_schema["required"] == ["text", "latex", "kind"]
        assert set(schema["required"]) <= set(schema["properties"])
        assert "assume" in schema["properties"]
        for name, prop in schema["properties"].items():
            if name in {"variable", "variables"}:
                inner = prop.get("items", prop)
                assert inner["pattern"] == NAME_PATTERN, (item.name, name)
        for node in schemas_of(schema):
            if node.get("type") == "string" and "pattern" not in node:
                assert node["maxLength"] == 16384, (item.name, node)
            if node.get("type") == "object" and node is not schema:
                assert node["propertyNames"]["pattern"] == NAME_PATTERN, (item.name, node)


def test_the_schema_pattern_is_the_identifier_pattern():
    names = ["x", "x1", "a_b", "a_1_b2", "X", "a__b", "_a", "a_", "1a", "x y", "os.system", ""]
    names += ["__class__", "a_b_", "ab_c__d", "é", "x\n"]
    for name in names:
        assert bool(re.fullmatch(NAME_PATTERN, name)) == is_identifier(name), name


def test_the_tool_list_is_under_its_budget():
    visible = json.dumps(
        [
            {"name": t.name, "description": t.description, "inputSchema": t.input_schema}
            for t in TOOLS
        ],
        separators=(",", ":"),
    )
    full = json.dumps([t.model_dump(by_alias=True, exclude_none=True) for t in TOOLS])
    MEASUREMENTS.append(
        f"tools/list: {len(visible)} characters as a model sees it (names, descriptions, input"
        f" schemas; about {len(visible) // 4} to {len(visible) * 2 // 7} tokens at 3.5 to 4"
        f" characters per token), {len(full)} with output schemas"
    )
    assert len(visible) <= TOOL_LIST_BUDGET, len(visible)


def test_a_solve_call_returns_text_latex_and_structured_content(pools):
    pool = pools(wall_seconds=60)

    async def main():
        async with Client(make_server(pool, "landlock"), mode="legacy") as client:
            listed = await client.list_tools()
            assert len(listed.tools) == 11
            return await client.call_tool("solve", {"equations": ["x^2 = 4"]})

    result = run(main)
    assert not result.is_error
    assert result.content[0].text == "{-2, 2} (complex)\nLaTeX: \\left\\{-2, 2\\right\\}"
    assert result.structured_content == {
        "text": "{-2, 2}",
        "latex": "\\left\\{-2, 2\\right\\}",
        "kind": "set",
        "domain": "complex",
    }


def test_errors_are_tool_errors_with_their_category(pools):
    pool = pools(wall_seconds=60)
    calls = [
        ("simplify", {"expression": "x +"}, "parse: "),
        ("simplify", {"expression": "2^(10^7)"}, "guard: "),
        ("solve", {"equations": ["x < 1"]}, "math: "),
        ("simplify", {"expression": "x", "bogus": 1}, "parse: the arguments: "),
        ("differentiate", {"expression": "x", "variable": "__class__"}, "parse: variable: "),
        ("simplify", {"expression": "x", "assume": {"os.system": ["real"]}}, "parse: "),
        ("evaluate", {"expression": "x", "digits": 1001}, "parse: digits: "),
        ("simplify", {"expression": "x" * 16385}, "parse: expression: "),
        ("build", {"expression": "1"}, "parse: unknown tool"),
    ]

    async def main():
        async with Client(make_server(pool, "landlock"), mode="legacy") as client:
            return [await client.call_tool(name, args) for name, args, _ in calls]

    for (name, _, prefix), result in zip(calls, run(main), strict=True):
        assert result.is_error, name
        assert result.content[0].text.startswith(prefix), (name, result.content[0].text)
        assert len(result.content[0].text) <= 300 or prefix in {"parse: ", "guard: "}


def test_a_limit_through_the_server_is_a_tool_error(pools):
    pool = pools("sleep.py", wall_seconds=2)

    async def main():
        async with Client(make_server(pool, "landlock"), mode="legacy") as client:
            return await client.call_tool("simplify", {"expression": "x"})

    result = run(main)
    assert result.is_error and result.content[0].text == "limit: wall clock (2 s)"


def test_a_cancelled_call_kills_its_worker(pools):
    """The client gives up (and sends `notifications/cancelled`); the front cancels the call in
    the pool, which kills the worker long before its 60 s wall clock."""
    pool = pools("sleep.py", wall_seconds=60)
    seen: dict = {}

    def watch():
        deadline = time.monotonic() + WORKER_TIMEOUT
        while time.monotonic() < deadline and "proc" not in seen:
            running = pool.running
            if running is not None:
                seen["proc"] = running.proc
            time.sleep(0.01)

    watcher = threading.Thread(target=watch)
    watcher.start()

    async def main():
        async with Client(make_server(pool, "landlock"), mode="legacy") as client:
            started = time.monotonic()
            with pytest.raises(Exception):  # noqa: B017, PT011 - the SDK's timeout error
                await client.call_tool("simplify", {"expression": "x"}, read_timeout_seconds=1)
            return started

    started = run(main)
    watcher.join()
    proc = seen["proc"]
    while proc.poll() is None and time.monotonic() - started < 15:
        time.sleep(0.05)
    assert proc.poll() is not None, "the worker outlived the cancelled call"
    assert time.monotonic() - started < 15


def test_the_initialize_reply_carries_the_confinement():
    async def main(confinement: str):
        middleware = confinement_middleware(confinement)

        class Context:
            method = "initialize"

        async def call_next(ctx):
            return {"protocolVersion": "2025-06-18", "serverInfo": {"name": "imperium_cas"}}

        return await middleware(Context(), call_next)

    for confinement in ("landlock", "no-landlock", "not-linux"):
        reply = run(main, confinement)
        assert reply["serverInfo"] == {"name": "imperium_cas", "confinement": confinement}
    server = make_server(None, "no-landlock")
    assert server.create_initialization_options().instructions == NO_LANDLOCK_NOTE
    assert make_server(None, "landlock").create_initialization_options().instructions is None


def corpus_calls() -> list[tuple[str, dict]]:
    """Each corpus line as the main argument of one tool, the tools taken in turn."""
    return [tool_requests(text)[index % 11] for index, (_, text) in enumerate(ENTRIES)]


@pytest.mark.parametrize("chunk", range(CORPUS_CHUNKS))
def test_the_hostile_corpus_through_tools_call(pools, chunk):
    """No line is `refused` (the hook saw an event outside the allow-list) or `crashed`."""
    calls = corpus_calls()[chunk::CORPUS_CHUNKS]
    servers = [
        make_server(pools(cpu_seconds=5, memory_mib=512, wall_seconds=15), "landlock")
        for _ in range(PARALLEL)
    ]
    results: list = [None] * len(calls)

    async def lane(index: int) -> None:
        async with Client(servers[index], mode="legacy") as client:
            for position in range(index, len(calls), PARALLEL):
                name, args = calls[position]
                results[position] = await client.call_tool(name, args)

    async def main():
        async with anyio.create_task_group() as group:
            for index in range(PARALLEL):
                group.start_soon(lane, index)

    started = time.monotonic()
    run(main)
    MEASUREMENTS.append(
        f"corpus chunk {chunk} through tools/call ({sys.platform}): {len(calls)} calls in"
        f" {time.monotonic() - started:.0f} s"
    )
    for (name, args), result in zip(calls, results, strict=True):
        text = result.content[0].text
        if result.is_error:
            category = text.split(":", 1)[0]
            assert category in {"parse", "guard", "math", "limit"}, (name, str(args)[:80], text)


BOOTSTRAP_SPAWNER = """
import imperium_cas.pool as pool
pool.default_spawner = lambda: pool.PosixSpawner(command={command!r})
"""


def stdio_command() -> list[str]:
    """`python -I -B -c <bootstrap>` running `imperium_cas` as `__main__`, with workers that find
    the package the same way (the bootstrap is test-only; production uses the `.pth`)."""
    worker = isolated_command("imperium_cas.worker")
    command = isolated_command("imperium_cas")
    head, run_module = command[-1].split("\n", 1)
    command[-1] = "\n".join([head, BOOTSTRAP_SPAWNER.format(command=worker).strip(), run_module])
    return command


@pytest.mark.skipif(sys.platform == "win32", reason="the server needs the broker (stage 7)")
def test_a_real_stdio_server():
    messages = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "solve", "arguments": {"equations": ["x^2 = 4"]}},
        },
    ]
    proc = subprocess.Popen(  # noqa: S603 - the test interpreter and a fixed bootstrap
        stdio_command(),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={},
        cwd=SRC,
    )
    replies: dict = {}
    try:
        for message in messages:
            proc.stdin.write((json.dumps(message) + "\n").encode())
            proc.stdin.flush()
            if "id" in message:
                replies[message["id"]] = json.loads(proc.stdout.readline())
        proc.stdin.close()
        code = proc.wait(WORKER_TIMEOUT)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()
    assert code == 0, proc.stderr.read().decode(errors="replace")
    info = replies[1]["result"]["serverInfo"]
    expected = landlock.STATE["confinement"]  # `None` here: this session is not confined
    assert expected is None
    if sys.platform.startswith("linux"):
        assert info["confinement"] in {"landlock", "no-landlock"}
    else:
        assert info["confinement"] == "not-linux"
    assert info["name"] == "imperium_cas"
    assert len(replies[2]["result"]["tools"]) == 11
    call = replies[3]["result"]
    assert call["isError"] is False
    assert call["content"][0]["text"].startswith("{-2, 2} (complex)\nLaTeX: ")
    assert call["structuredContent"]["kind"] == "set"
