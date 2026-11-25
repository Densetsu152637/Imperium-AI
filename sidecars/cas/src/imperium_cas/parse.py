"""Tokenizer and Pratt parser for the CAS input language (plan D1).

The parser reads agent-written maths text and builds a plain-Python syntax tree. It never calls
`eval`, `exec`, `compile`, `sympify` or `parse_expr`, and it never touches SymPy: the tree goes
through `guard.py` first and only then into SymPy objects (stage 2a's `build.py`).

The language:

- Numbers: integers and decimals with an optional exponent (`12`, `0.5`, `.5`, `1.5e-3`). A
  decimal is an exact rational, never a float. An exponent is only read as part of the number when
  digits follow it, so `2e` is `2*e` and `2E+3` is 2000.
- Names: `[A-Za-z][A-Za-z0-9]*(_[A-Za-z0-9]+)*`. A name in the function table followed by `(` is a
  call, `pi E I oo` are constants and every other name is a symbol.
- Operators: `+ - * / ^ **`, unary minus, postfix `!`, parentheses, and at the top level one
  relation from `= == != < <= > >=`.
- Lists and matrices: `[a, b]` and `[[1, 2], [3, 4]]` (a matrix is rectangular, two levels deep).
- Implicit multiplication: a number followed by a name, a prefix square root or `(`, and `)(`.
- A short table maps common maths characters to their ASCII meaning. Any other non-ASCII
  character, any control character and NUL are refused.

Caps: 16 KiB of text, 4,096 tokens, nesting depth 200, 8,192 tree nodes, 1,000 digits per
number. Every tree walk in this package is iterative; the only recursion is the parser's, and it
is bounded by the depth cap (at most three stack frames per level).
"""

from dataclasses import dataclass

from .functions import CONSTANTS, FUNCTIONS

MAX_CHARS = 16 * 1024
MAX_TOKENS = 4096
MAX_DEPTH = 200
MAX_NODES = 8192
MAX_NUMBER_DIGITS = 1000

IDENTIFIER_BODY = "[A-Za-z][A-Za-z0-9]*(_[A-Za-z0-9]+)*"
IDENTIFIER_PATTERN = "^" + IDENTIFIER_BODY + "$"

RELATIONS = frozenset({"==", "!=", "<", "<=", ">", ">="})

# Binding powers: `+ -` 10, `* /` and implicit multiplication 20, prefix minus 25, `^` 31 (right
# associative, so its right side is parsed at 30), prefix square root 35, postfix `!` 40.
BP_ADD = 10
BP_MUL = 20
BP_NEG = 25
BP_POW = 30
BP_SQRT = 35
BP_FACT = 40

# Characters outside ASCII that the language accepts, written as escapes so the source stays ASCII.
UNICODE_OPERATORS = {
    "×": "*",  # multiplication sign
    "·": "*",  # middle dot
    "−": "-",  # minus sign
    "÷": "/",  # division sign
    "≤": "<=",
    "≥": ">=",
    "≠": "!=",
}
UNICODE_SUPERSCRIPTS = {"²": 2, "³": 3}
PI_CHAR = "π"
SQRT_CHAR = "√"

WHITESPACE = " \t\n\r"
SINGLE_OPERATORS = "+-*/^!()[],=<>"


class CasInputError(Exception):
    """Input the language or the guard refused. Carries a column (1-based) and a message."""

    category = "input"

    @property
    def message(self) -> str:
        return self.args[0]

    @property
    def column(self) -> int:
        return self.args[1]

    def __str__(self) -> str:
        return self.args[0]


class ParseError(CasInputError):
    category = "parse"


@dataclass(frozen=True, slots=True)
class Token:
    kind: str  # num, id, op, sqrt or end
    value: object
    pos: int  # 0-based index into the text


@dataclass(frozen=True, slots=True, eq=False)
class Num:
    """The exact value `mantissa * 10**exponent`. The guard sizes it without computing it."""

    mantissa: int
    exponent: int
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Sym:
    name: str
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Const:
    name: str
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Neg:
    operand: object
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class BinOp:
    op: str  # + - * / ^
    left: object
    right: object
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Call:
    name: str
    args: tuple
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Rel:
    op: str  # == != < <= > >=
    left: object
    right: object
    pos: int


@dataclass(frozen=True, slots=True, eq=False)
class Seq:
    """A list `[a, b]`. A matrix is a `Seq` of equal-length `Seq`s of scalars."""

    items: tuple
    pos: int


@dataclass(frozen=True, slots=True)
class Parsed:
    root: object
    tokens: int
    nodes: int


def fail(message: str, pos: int) -> ParseError:
    """The error for a problem at a 0-based index (reported as a 1-based column)."""
    return ParseError(message, pos + 1)


def ascii_letter(ch: str) -> bool:
    return "a" <= ch <= "z" or "A" <= ch <= "Z"


def ascii_digit(ch: str) -> bool:
    return "0" <= ch <= "9"


def is_identifier(name: object) -> bool:
    """True for a string that matches `IDENTIFIER_PATTERN` (no regex, no Unicode digits)."""
    if not isinstance(name, str) or not name or not ascii_letter(name[0]):
        return False
    for part in name.split("_"):
        if not part:
            return False
        for ch in part:
            if not (ascii_letter(ch) or ascii_digit(ch)):
                return False
    return True


def describe_char(ch: str) -> str:
    code = ord(ch)
    if code < 32 or code == 127:
        return f"control character U+{code:04X}"
    if code < 127:
        return f"'{ch}'"
    return f"non-ASCII character U+{code:04X}"


def describe_token(tok: Token) -> str:
    if tok.kind == "end":
        return "the end of the input"
    if tok.kind == "num":
        return "a number"
    if tok.kind == "id":
        return f"'{tok.value}'"
    if tok.kind == "sqrt":
        return "the square root sign"
    return f"'{tok.value}'"


def scan_digits(text: str, start: int) -> int:
    end = start
    while end < len(text) and ascii_digit(text[end]):
        end += 1
    return end


def scan_number(text: str, start: int) -> tuple[Token, int]:
    """Read a number at `start`. Returns the token and the index after it."""
    int_end = scan_digits(text, start)
    int_digits = text[start:int_end]
    frac_digits = ""
    end = int_end
    if end < len(text) and text[end] == ".":
        frac_end = scan_digits(text, end + 1)
        if frac_end == end + 1:
            raise fail("a decimal point needs digits after it", end)
        frac_digits = text[end + 1 : frac_end]
        end = frac_end
    exponent = 0
    if end < len(text) and text[end] in "eE":
        sign_at = end + 1
        negative = False
        if sign_at < len(text) and text[sign_at] in "+-":
            negative = text[sign_at] == "-"
            sign_at += 1
        exp_end = scan_digits(text, sign_at)
        if exp_end > sign_at:
            if exp_end - sign_at > MAX_NUMBER_DIGITS:
                raise fail(f"an exponent has at most {MAX_NUMBER_DIGITS:,} digits", sign_at)
            exponent = int(text[sign_at:exp_end])
            if negative:
                exponent = -exponent
            end = exp_end
    if len(int_digits) + len(frac_digits) > MAX_NUMBER_DIGITS:
        raise fail(f"a number has at most {MAX_NUMBER_DIGITS:,} digits", start)
    mantissa = int(int_digits + frac_digits)
    return Token("num", (mantissa, exponent - len(frac_digits)), start), end


def scan_name(text: str, start: int) -> tuple[Token, int]:
    end = start + 1
    while end < len(text) and (ascii_letter(text[end]) or ascii_digit(text[end])):
        end += 1
    while end < len(text) and text[end] == "_":
        after = end + 1
        if after < len(text) and (ascii_letter(text[after]) or ascii_digit(text[after])):
            end = after
            while end < len(text) and (ascii_letter(text[end]) or ascii_digit(text[end])):
                end += 1
        else:
            raise fail("an underscore must sit between letters or digits of a name", end)
    return Token("id", text[start:end], start), end


def tokenize(text: str) -> list[Token]:
    """Split `text` into tokens, ending with an `end` token. Checks the length and token caps."""
    if not isinstance(text, str):
        raise fail("the expression must be text", 0)
    if len(text) > MAX_CHARS:
        raise fail(f"the expression is longer than {MAX_CHARS:,} characters", MAX_CHARS)
    tokens: list[Token] = []
    i = 0
    size = len(text)
    while i < size:
        ch = text[i]
        if ch in WHITESPACE:
            i += 1
            continue
        if ascii_digit(ch) or (ch == "." and i + 1 < size and ascii_digit(text[i + 1])):
            tok, i = scan_number(text, i)
            tokens.append(tok)
        elif ascii_letter(ch):
            tok, i = scan_name(text, i)
            tokens.append(tok)
        elif ch in UNICODE_OPERATORS:
            tokens.append(Token("op", normalise_operator(UNICODE_OPERATORS[ch]), i))
            i += 1
        elif ch in UNICODE_SUPERSCRIPTS:
            tokens.append(Token("op", "^", i))
            tokens.append(Token("num", (UNICODE_SUPERSCRIPTS[ch], 0), i))
            i += 1
        elif ch == PI_CHAR:
            tokens.append(Token("id", "pi", i))
            i += 1
        elif ch == SQRT_CHAR:
            tokens.append(Token("sqrt", ch, i))
            i += 1
        elif ch in SINGLE_OPERATORS:
            two = text[i : i + 2]
            if two in ("**", "==", "!=", "<=", ">="):
                tokens.append(Token("op", normalise_operator(two), i))
                i += 2
            else:
                tokens.append(Token("op", normalise_operator(ch), i))
                i += 1
        else:
            raise fail(f"{describe_char(ch)} is not part of the language", i)
        if len(tokens) > MAX_TOKENS:
            raise fail(f"the expression has more than {MAX_TOKENS:,} tokens", i - 1)
    tokens.append(Token("end", None, size))
    return tokens


def normalise_operator(op: str) -> str:
    if op == "**":
        return "^"
    if op == "=":
        return "=="
    return op


def children(node: object) -> tuple:
    """The direct children of a tree node."""
    if isinstance(node, (BinOp, Rel)):
        return (node.left, node.right)
    if isinstance(node, Neg):
        return (node.operand,)
    if isinstance(node, Call):
        return node.args
    if isinstance(node, Seq):
        return node.items
    return ()


def postorder(root: object):
    """Yield every node of the tree, children before parents, without recursion."""
    stack = [(root, False)]
    while stack:
        node, expanded = stack.pop()
        if expanded:
            yield node
            continue
        stack.append((node, True))
        for child in reversed(children(node)):
            stack.append((child, False))


def dump(root: object) -> str:
    """A canonical S-expression of a tree, for tests and error reports."""
    done: list[str] = []
    for node in postorder(root):
        count = len(children(node))
        kids = done[len(done) - count :] if count else []
        del done[len(done) - count :]
        if isinstance(node, Num):
            text = str(node.mantissa) if node.exponent == 0 else f"{node.mantissa}e{node.exponent}"
        elif isinstance(node, (Sym, Const)):
            text = node.name
        elif isinstance(node, Neg):
            text = f"(neg {kids[0]})"
        elif isinstance(node, (BinOp, Rel)):
            text = f"({node.op} {kids[0]} {kids[1]})"
        elif isinstance(node, Call):
            text = "(" + " ".join([node.name, *kids]) + ")"
        else:
            text = "[" + " ".join(kids) + "]"
        done.append(text)
    return done[0]


class Parser:
    """A Pratt parser over a token list. All state is public so the module stays attribute-clean."""

    def __init__(self, toks: list[Token]):
        self.toks = toks
        self.i = 0
        self.nodes = 0

    def peek(self) -> Token:
        return self.toks[self.i]

    def advance(self) -> Token:
        tok = self.toks[self.i]
        self.i += 1
        return tok

    def count(self, pos: int) -> None:
        self.nodes += 1
        if self.nodes > MAX_NODES:
            raise fail(f"the expression has more than {MAX_NODES:,} parts", pos)

    def leftover(self, tok: Token) -> ParseError:
        """The error for a token that cannot continue the expression."""
        prev = self.toks[self.i - 1] if self.i > 0 else None
        if tok.kind == "end":
            return fail("the expression ends early: a ')', ']' or an operand is missing", tok.pos)
        if tok.kind == "op" and tok.value in RELATIONS:
            return fail(
                f"'{tok.value}' is not allowed here: one relation is allowed, "
                "at the top level of an expression",
                tok.pos,
            )
        if tok.kind == "op" and tok.value in (")", "]"):
            return fail(f"unmatched '{tok.value}'", tok.pos)
        if tok.kind == "op" and tok.value == ",":
            return fail("',' is only allowed between list items or function arguments", tok.pos)
        starts_operand = tok.kind in ("id", "sqrt", "num") or (
            tok.kind == "op" and tok.value in ("(", "[")
        )
        if starts_operand and prev is not None:
            if prev.kind == "id" and tok.kind == "id":
                return fail(
                    f"two names side by side: write '{prev.value}*{tok.value}' "
                    f"(a name like '{prev.value}{tok.value}' is one symbol)",
                    tok.pos,
                )
            return fail(
                f"missing operator before {describe_token(tok)}: write '*' for a product",
                tok.pos,
            )
        return fail(f"unexpected {describe_token(tok)}", tok.pos)

    def expect(self, value: str) -> Token:
        tok = self.peek()
        if tok.kind == "op" and tok.value == value:
            return self.advance()
        raise self.leftover(tok)

    def expr(self, min_bp: int, depth: int):
        if depth > MAX_DEPTH:
            raise fail(
                f"the expression is nested more than {MAX_DEPTH} levels deep", self.peek().pos
            )
        left = self.prefix(depth)
        while True:
            tok = self.peek()
            if tok.kind == "op":
                value = tok.value
                if value == "!":
                    if BP_FACT <= min_bp:
                        break
                    self.advance()
                    self.count(tok.pos)
                    left = Call("factorial", (left,), tok.pos)
                elif value in ("+", "-"):
                    if BP_ADD <= min_bp:
                        break
                    self.advance()
                    right = self.expr(BP_ADD, depth)
                    self.count(tok.pos)
                    left = BinOp(value, left, right, tok.pos)
                elif value in ("*", "/"):
                    if BP_MUL <= min_bp:
                        break
                    self.advance()
                    right = self.expr(BP_MUL, depth)
                    self.count(tok.pos)
                    left = BinOp(value, left, right, tok.pos)
                elif value == "^":
                    if BP_POW + 1 <= min_bp:
                        break
                    self.advance()
                    right = self.expr(BP_POW, depth + 1)
                    self.count(tok.pos)
                    left = BinOp("^", left, right, tok.pos)
                elif value == "(" and self.implicit_ok(tok):
                    if BP_MUL <= min_bp:
                        break
                    right = self.expr(BP_MUL, depth)
                    self.count(tok.pos)
                    left = BinOp("*", left, right, tok.pos)
                else:
                    break
            elif tok.kind in ("id", "sqrt") and self.implicit_ok(tok):
                if BP_MUL <= min_bp:
                    break
                right = self.expr(BP_MUL, depth)
                self.count(tok.pos)
                left = BinOp("*", left, right, tok.pos)
            else:
                break
        return left

    def implicit_ok(self, tok: Token) -> bool:
        """True when `tok` after the operand just parsed is an implicit multiplication."""
        prev = self.toks[self.i - 1]
        if prev.kind == "num":
            return tok.kind in ("id", "sqrt") or (tok.kind == "op" and tok.value == "(")
        return prev.kind == "op" and prev.value == ")" and tok.kind == "op" and tok.value == "("

    def prefix(self, depth: int):
        tok = self.advance()
        kind = tok.kind
        if kind == "num":
            self.count(tok.pos)
            return Num(tok.value[0], tok.value[1], tok.pos)
        if kind == "id":
            return self.name(tok, depth)
        if kind == "sqrt":
            operand = self.expr(BP_SQRT, depth + 1)
            self.count(tok.pos)
            return Call("sqrt", (operand,), tok.pos)
        if kind == "op":
            value = tok.value
            if value == "(":
                nxt = self.peek()
                if nxt.kind == "op" and nxt.value == ")":
                    raise fail("empty parentheses", nxt.pos)
                inner = self.expr(0, depth + 1)
                self.expect(")")
                return inner
            if value == "[":
                return self.seq(tok, depth)
            if value == "-":
                operand = self.expr(BP_NEG, depth + 1)
                self.count(tok.pos)
                return Neg(operand, tok.pos)
            if value == "+":
                return self.expr(BP_NEG, depth + 1)
        if kind == "end":
            raise fail("an operand is missing at the end of the input", tok.pos)
        raise fail(f"expected an operand, found {describe_token(tok)}", tok.pos)

    def name(self, tok: Token, depth: int):
        name = tok.value
        nxt = self.peek()
        is_call = nxt.kind == "op" and nxt.value == "("
        if not is_call:
            self.count(tok.pos)
            if name in CONSTANTS:
                return Const(name, tok.pos)
            return Sym(name, tok.pos)
        if name in CONSTANTS:
            raise fail(f"'{name}' is a constant, not a function: write '{name}*(...)'", tok.pos)
        function = FUNCTIONS.get(name)
        if function is None:
            known = ", ".join(sorted(FUNCTIONS))
            raise fail(f"unknown function '{name}'; the functions are: {known}", tok.pos)
        self.advance()  # the "("
        args = []
        while True:
            args.append(self.expr(0, depth + 1))
            nxt = self.peek()
            if nxt.kind == "op" and nxt.value == ",":
                self.advance()
                continue
            self.expect(")")
            break
        low, high = function.min_args, function.max_args
        if len(args) < low or (high is not None and len(args) > high):
            want = (
                f"{low}"
                if high == low
                else f"{low} or more"
                if high is None
                else f"{low} to {high}"
            )
            raise fail(f"'{name}' takes {want} argument(s), got {len(args)}", tok.pos)
        self.count(tok.pos)
        return Call(name, tuple(args), tok.pos)

    def seq(self, open_tok: Token, depth: int):
        nxt = self.peek()
        if nxt.kind == "op" and nxt.value == "]":
            raise fail("empty list", nxt.pos)
        items = []
        while True:
            items.append(self.expr(0, depth + 1))
            nxt = self.peek()
            if nxt.kind == "op" and nxt.value == ",":
                self.advance()
                continue
            self.expect("]")
            break
        self.count(open_tok.pos)
        check_shape(items, open_tok.pos)
        return Seq(tuple(items), open_tok.pos)


def check_shape(items: list, pos: int) -> None:
    """A list is flat, or a rectangular matrix of flat rows: no ragged rows, no third level."""
    rows = [item for item in items if isinstance(item, Seq)]
    if not rows:
        return
    if len(rows) != len(items):
        raise fail("a list mixes rows and single values", pos)
    width = len(rows[0].items)
    for row in rows:
        if len(row.items) != width:
            raise fail("the rows of a matrix must have the same length", row.pos)
        for entry in row.items:
            if isinstance(entry, Seq):
                raise fail("lists nest at most two levels (a matrix of values)", entry.pos)


def parse_stats(text: str) -> Parsed:
    """Parse `text`, returning the tree with its token and node counts."""
    if isinstance(text, str) and not text.strip(WHITESPACE):
        raise fail("the expression is empty", 0)
    toks = tokenize(text)
    parser = Parser(toks)
    root = parser.expr(0, 0)
    tok = parser.peek()
    if tok.kind == "op" and tok.value in RELATIONS:
        parser.advance()
        right = parser.expr(0, 0)
        parser.count(tok.pos)
        root = Rel(tok.value, root, right, tok.pos)
        tok = parser.peek()
        if tok.kind == "op" and tok.value in RELATIONS:
            raise fail("chained relations (a < b < c) are not supported: write each one", tok.pos)
    if tok.kind != "end":
        raise parser.leftover(tok)
    return Parsed(root, len(toks) - 1, parser.nodes)


def parse(text: str):
    """Parse `text` into a syntax tree, or raise `ParseError`."""
    return parse_stats(text).root


def render_error(err: CasInputError, text: str) -> str:
    """The one-line tool error: category, reason, and for a parse error the column and a caret."""
    if not isinstance(err, ParseError):
        return f"{err.category}: {err.message}"
    idx = max(err.column - 1, 0)
    start = max(0, idx - 30)
    window = text[start : start + 70] if isinstance(text, str) else ""
    shown = "".join(ch if " " <= ch < "\x7f" else "?" for ch in window)
    return f"parse: {err.message} (column {err.column})\n  {shown}\n  {' ' * (idx - start)}^"
