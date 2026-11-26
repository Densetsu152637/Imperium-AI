"""The CAS's MCP front (plan D6): `tools/list` and `tools/call` over stdio. It never imports SymPy.

The SDK's low-level server serves the eleven tools of D6. Each call's arguments are checked
against the tool's input schema here (the worker's builder checks every name again), then the
call goes to the pool, which runs it in a fresh worker under its limits. The deadline counts
from when the call arrived. An MCP `notifications/cancelled` cancels the handler, which cancels
the call in the pool, and that kills its worker.

A result has one text item (the text, with the domain for `solve`, a note when there is one,
and a `LaTeX:` line) and `structuredContent` with the reply's fields. An error is an MCP tool
error (`isError`) with the one-line category message.

The `initialize` reply's `serverInfo` carries `confinement` (`landlock`, `no-landlock` or
`not-linux`), and without Landlock its `instructions` hold the one-line note of D4.
"""

import itertools
import time
from functools import partial

import anyio
import mcp_types as types
from jsonschema import Draft202012Validator
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

from .pool import Pool

VERSION = "0.1.0"
NO_LANDLOCK_NOTE = "Landlock unavailable: programs in /usr can be started inside the sandbox"
# D1's identifier pattern `[A-Za-z][A-Za-z0-9]*(_[A-Za-z0-9]+)*`, written shorter (the same
# language: a letter, then letters or digits, each optionally after one underscore).
NAME_PATTERN = "^[A-Za-z](_?[A-Za-z0-9])*$"
ASSUMPTIONS = [
    "real",
    "positive",
    "negative",
    "nonnegative",
    "nonpositive",
    "nonzero",
    "integer",
    "rational",
    "finite",
]
SYNTAX = "Syntax: x^2, 2x, sqrt(x), pi, E, I, oo."
ERROR_CHARS = 300  # a schema error's message is cut to this, since it can quote the input

EXPRESSION = {"type": "string", "maxLength": 16384}
NAME = {"type": "string", "pattern": NAME_PATTERN}
ASSUME = {
    "type": "object",
    "propertyNames": {"pattern": NAME_PATTERN},
    "additionalProperties": {"type": "array", "items": {"enum": ASSUMPTIONS}},
}
OUTPUT = {
    "type": "object",
    "properties": {
        "text": {"type": "string"},
        "latex": {"type": "string"},
        "kind": {"type": "string"},
    },
    "required": ["text", "latex", "kind"],
}


def tool(name: str, description: str, properties: dict, required: list[str]) -> types.Tool:
    schema = {
        "type": "object",
        "properties": {**properties, "assume": ASSUME},
        "required": required,
        "additionalProperties": False,
    }
    return types.Tool(
        name=name,
        description=f"{description} {SYNTAX}",
        input_schema=schema,
        output_schema=OUTPUT,
    )


def integer(low: int, high: int) -> dict:
    return {"type": "integer", "minimum": low, "maximum": high}


EXPR = {"expression": EXPRESSION}
VAR = {"variable": NAME}
TOOLS = [
    tool("simplify", "Simplify an expression exactly.", EXPR, ["expression"]),
    tool("expand", "Expand products and powers.", EXPR, ["expression"]),
    tool(
        "factor",
        "Factor a polynomial, or an integer into primes.",
        {**EXPR, "gaussian": {"type": "boolean"}},
        ["expression"],
    ),
    tool(
        "solve",
        "Solve equations (a = b, or an expression = 0) or one inequality; states the domain.",
        {
            "equations": {"type": "array", "items": EXPRESSION, "minItems": 1, "maxItems": 10},
            "variables": {"type": "array", "items": NAME, "minItems": 1, "maxItems": 10},
            "domain": {"enum": ["complex", "real"]},
        },
        ["equations"],
    ),
    tool(
        "differentiate",
        "Differentiate.",
        {**EXPR, **VAR, "order": integer(1, 20)},
        ["expression"],
    ),
    tool(
        "integrate",
        "Antiderivative (no + C), or the definite integral with lower and upper.",
        {**EXPR, **VAR, "lower": EXPRESSION, "upper": EXPRESSION},
        ["expression"],
    ),
    tool(
        "limit",
        "Limit as variable goes to `to` (oo allowed).",
        {**EXPR, **VAR, "to": EXPRESSION, "direction": {"enum": ["+", "-", "both"]}},
        ["expression", "to"],
    ),
    tool(
        "series",
        "Taylor or Laurent series with its O() term.",
        {**EXPR, **VAR, "at": EXPRESSION, "order": integer(1, 50)},
        ["expression"],
    ),
    tool(
        "matrix",
        "A matrix operation.",
        {
            "matrix": EXPRESSION,
            "operation": {
                "enum": [
                    "value",
                    "det",
                    "inverse",
                    "transpose",
                    "rank",
                    "rref",
                    "nullspace",
                    "eigenvalues",
                    "eigenvectors",
                    "charpoly",
                    "solve",
                ]
            },
            "rhs": EXPRESSION,
        },
        ["matrix", "operation"],
    ),
    tool(
        "evaluate",
        "Decimal value to `digits` significant digits, with `at` values substituted.",
        {
            **EXPR,
            "digits": integer(1, 1000),
            "at": {
                "type": "object",
                "propertyNames": {"pattern": NAME_PATTERN},
                "additionalProperties": EXPRESSION,
            },
        },
        ["expression"],
    ),
    tool(
        "equal",
        "Whether two expressions are equal: true (proved), false (with a point), or unknown.",
        {"left": EXPRESSION, "right": EXPRESSION},
        ["left", "right"],
    ),
]
TOOLS_BY_NAME = {item.name: item for item in TOOLS}
VALIDATORS = {item.name: Draft202012Validator(item.input_schema) for item in TOOLS}


def error_result(message: str) -> types.CallToolResult:
    return types.CallToolResult(content=[types.TextContent(text=message)], is_error=True)


def to_result(reply: dict) -> types.CallToolResult:
    """A worker or pool reply as an MCP tool result."""
    if not reply.get("ok"):
        return error_result(str(reply.get("message", "crashed: no reply")))
    first = reply["text"]
    if reply.get("domain"):
        first += f" ({reply['domain']})"
    lines = [first]
    if reply.get("note"):
        lines.append(f"Note: {reply['note']}")
    lines.append(f"LaTeX: {reply['latex']}")
    structured = {key: value for key, value in reply.items() if key != "ok"}
    return types.CallToolResult(
        content=[types.TextContent(text="\n".join(lines))], structured_content=structured
    )


def schema_error(name: str, arguments: dict) -> str | None:
    """The first input-schema error as a one-line `parse` message, or `None`."""
    error = next(VALIDATORS[name].iter_errors(arguments), None)
    if error is None:
        return None
    where = "/".join(str(part) for part in error.absolute_path) or "the arguments"
    return f"parse: {where}: {error.message}"[:ERROR_CHARS]


class Front:
    """The handlers, over one pool."""

    def __init__(self, pool: Pool):
        self.pool = pool
        self.ids = itertools.count(1)

    async def list_tools(self, ctx, params) -> types.ListToolsResult:
        return types.ListToolsResult(tools=TOOLS)

    async def call_tool(self, ctx, params: types.CallToolRequestParams) -> types.CallToolResult:
        received = time.monotonic()
        name, arguments = params.name, params.arguments or {}
        if name not in TOOLS_BY_NAME:
            return error_result(f"parse: unknown tool; the tools are {', '.join(TOOLS_BY_NAME)}")
        problem = schema_error(name, arguments)
        if problem is not None:
            return error_result(problem)
        call_id = next(self.ids)
        call = partial(self.pool.call, {"tool": name, "args": arguments}, call_id, received)
        try:
            reply = await anyio.to_thread.run_sync(call, abandon_on_cancel=True)
        except anyio.get_cancelled_exc_class():
            self.pool.cancel(call_id)  # kills the worker; the thread then finishes on its own
            raise
        return to_result(reply)


def confinement_middleware(confinement: str):
    """Add `confinement` to the `initialize` reply's `serverInfo` (plan D4)."""

    async def middleware(ctx, call_next):
        result = await call_next(ctx)
        if ctx.method == "initialize" and isinstance(result, dict):
            info = result.get("serverInfo")
            if isinstance(info, dict):
                result = {**result, "serverInfo": {**info, "confinement": confinement}}
        return result

    return middleware


def make_server(pool: Pool, confinement: str) -> Server:
    front = Front(pool)
    server = Server(
        "imperium_cas",
        version=VERSION,
        instructions=NO_LANDLOCK_NOTE if confinement == "no-landlock" else None,
        on_list_tools=front.list_tools,
        on_call_tool=front.call_tool,
    )
    # Replaces the SDK's default OpenTelemetry middleware: a sandboxed server exports nothing.
    server.middleware = [confinement_middleware(confinement)]
    return server


def run_stdio(pool: Pool, confinement: str) -> None:
    """Serve MCP on stdin and stdout until stdin ends."""
    server = make_server(pool, confinement)

    async def main() -> None:
        async with stdio_server() as (read_stream, write_stream):
            await server.run(read_stream, write_stream, server.create_initialization_options())

    anyio.run(main)
