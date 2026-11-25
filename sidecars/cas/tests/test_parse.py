"""The tokenizer and parser: precedence, implicit multiplication, relations, matrices, the caps
and every refusal with its column."""

import re
import sys

import pytest
from hypothesis import given
from hypothesis import strategies as st

from imperium_cas.functions import FUNCTIONS
from imperium_cas.parse import (
    IDENTIFIER_BODY,
    IDENTIFIER_PATTERN,
    MAX_CHARS,
    MAX_DEPTH,
    MAX_NODES,
    MAX_NUMBER_DIGITS,
    MAX_TOKENS,
    CasInputError,
    Num,
    ParseError,
    dump,
    is_identifier,
    parse,
    parse_stats,
    render_error,
)
from support import FUZZ_SEED


def d(text: str) -> str:
    return dump(parse(text))


@pytest.mark.parametrize(
    ("text", "tree"),
    [
        ("1+2*3", "(+ 1 (* 2 3))"),
        ("1-2-3", "(- (- 1 2) 3)"),
        ("a/b/c", "(/ (/ a b) c)"),
        ("a-b*c", "(- a (* b c))"),
        ("2^3^2", "(^ 2 (^ 3 2))"),
        ("2**3", "(^ 2 3)"),
        ("a**b**c", "(^ a (^ b c))"),
        ("-x^2", "(neg (^ x 2))"),
        ("(-x)^2", "(^ (neg x) 2)"),
        ("2^-x", "(^ 2 (neg x))"),
        ("2*-3", "(* 2 (neg 3))"),
        ("a - -b", "(- a (neg b))"),
        ("-a*b", "(* (neg a) b)"),
        ("--x", "(neg (neg x))"),
        ("+x", "x"),
        ("x!", "(factorial x)"),
        ("2^3!", "(^ 2 (factorial 3))"),
        ("-x!", "(neg (factorial x))"),
        ("(x+1)!", "(factorial (+ x 1))"),
        ("x!!", "(factorial (factorial x))"),
        ("a*b+c", "(+ (* a b) c)"),
        ("a+b*c^d!", "(+ a (* b (^ c (factorial d))))"),
    ],
)
def test_precedence_and_associativity(text, tree):
    assert d(text) == tree


@pytest.mark.parametrize(
    ("text", "tree"),
    [
        ("2x", "(* 2 x)"),
        ("2 x", "(* 2 x)"),
        ("3(x+1)", "(* 3 (+ x 1))"),
        ("(x+1)(x-1)", "(* (+ x 1) (- x 1))"),
        ("(a)(b)(c)", "(* (* a b) c)"),
        ("2x^2", "(* 2 (^ x 2))"),
        ("2sin(x)", "(* 2 (sin x))"),
        ("2pi", "(* 2 pi)"),
        ("2sin(x)cos(x)", None),
        ("x^2y", "(* (^ x 2) y)"),
        ("2e", "(* 2 e)"),
        ("sin(x)(y)", "(* (sin x) y)"),
        ("-2x", "(* (neg 2) x)"),
        ("1/2x", "(* (/ 1 2) x)"),
        ("2/3(x)", "(* (/ 2 3) x)"),
    ],
)
def test_implicit_multiplication(text, tree):
    if tree is None:
        with pytest.raises(ParseError, match="missing operator"):
            parse(text)
        return
    assert d(text) == tree


def test_a_name_is_one_symbol():
    assert d("xy") == "xy"
    assert d("x2") == "x2"
    assert d("x_1") == "x_1"
    assert d("x_y_z") == "x_y_z"


def test_constants_symbols_and_functions():
    assert d("pi+E+I+oo") == "(+ (+ (+ pi E) I) oo)"
    assert d("e") == "e"  # not a constant: only E is
    assert d("sin(x)") == "(sin x)"
    assert d("atan2(y, x)") == "(atan2 y x)"
    assert d("log(8, 2)") == "(log 8 2)"
    assert d("Max(1, 2, 3)") == "(Max 1 2 3)"
    # A table name with no "(" is a symbol (plan D1: "any other identifier is a Symbol").
    assert d("sin") == "sin"
    assert d("re+im") == "(+ re im)"


@pytest.mark.parametrize("op", ["=", "==", "!=", "<", "<=", ">", ">="])
def test_every_relation(op):
    expected = "==" if op == "=" else op
    assert d(f"x^2 {op} 4") == f"({expected} (^ x 2) 4)"
    assert d(f"x{op}4") == f"({expected} x 4)"


def test_relation_binds_loosest():
    assert d("x+1 = 2*y") == "(== (+ x 1) (* 2 y))"
    assert d("x!=3") == "(!= x 3)"
    assert d("x! = 3") == "(== (factorial x) 3)"


@pytest.mark.parametrize(
    ("text", "column"),
    [
        ("1 < 2 < 3", 7),
        ("a = b = c", 7),
        ("a <= b < c", 8),
        ("(x < 1)", 4),
        ("sin(x < 1)", 7),
        ("[x < 1]", 4),
        ("x < y, z", 6),
    ],
)
def test_relations_are_top_level_and_never_chained(text, column):
    with pytest.raises(ParseError) as caught:
        parse(text)
    assert caught.value.column == column


def test_lists_and_matrices():
    assert d("[1, 2, 3]") == "[1 2 3]"
    assert d("[[1, 2], [3, 4]]") == "[[1 2] [3 4]]"
    assert d("[[1,2],[3,4]]^2") == "(^ [[1 2] [3 4]] 2)"
    assert d("[[x, 1], [1, x]] * [[1], [2]]") == "(* [[x 1] [1 x]] [[1] [2]])"
    assert d("[a+1, sin(b)]") == "[(+ a 1) (sin b)]"


@pytest.mark.parametrize(
    ("text", "column", "reason"),
    [
        ("[]", 2, "empty list"),
        ("[1,]", 4, "operand"),
        ("[,1]", 2, "operand"),
        ("[1 2]", 4, "missing operator"),
        ("[1,[2]]", 1, "mixes rows and single values"),
        ("[[1,2],[3]]", 8, "same length"),
        ("[[[1]]]", 3, "two levels"),
        ("[[1],[[2]]]", 7, "two levels"),
    ],
)
def test_list_refusals(text, column, reason):
    with pytest.raises(ParseError, match=reason) as caught:
        parse(text)
    assert caught.value.column == column


@pytest.mark.parametrize(
    ("text", "mantissa", "exponent"),
    [
        ("0", 0, 0),
        ("12", 12, 0),
        ("007", 7, 0),
        ("0.1", 1, -1),
        ("0.50", 50, -2),
        (".5", 5, -1),
        ("1.5e-3", 15, -4),
        ("1e5", 1, 5),
        ("2E+3", 2, 3),
        ("3e2", 3, 2),
        ("12.25", 1225, -2),
    ],
)
def test_numbers_are_exact(text, mantissa, exponent):
    node = parse(text)
    assert isinstance(node, Num)
    assert (node.mantissa, node.exponent) == (mantissa, exponent)


def test_a_huge_exponent_is_parsed_without_being_computed():
    node = parse("1e999999999")
    assert (node.mantissa, node.exponent) == (1, 999999999)
    node = parse("1e" + "9" * 1000)
    assert node.exponent == int("9" * 1000)


def test_digit_caps():
    assert parse("1" * MAX_NUMBER_DIGITS).mantissa == int("1" * MAX_NUMBER_DIGITS)
    for text in ("1" * (MAX_NUMBER_DIGITS + 1), "1." + "1" * MAX_NUMBER_DIGITS):
        with pytest.raises(ParseError, match="at most 1,000 digits"):
            parse(text)
    with pytest.raises(ParseError, match="exponent has at most"):
        parse("1e" + "1" * (MAX_NUMBER_DIGITS + 1))


@pytest.mark.parametrize(
    ("text", "column", "reason"),
    [
        ("1+", 3, "operand is missing"),
        ("(1+2", 5, "ends early"),
        ("1+2)", 4, "unmatched"),
        ("[1,2", 5, "ends early"),
        ("1,2", 2, "only allowed between"),
        ("x y", 3, "two names side by side"),
        ("x y z", 3, "two names side by side"),
        ("2 3", 3, "missing operator"),
        ("(x)y", 4, "missing operator"),
        ("(x)2", 4, "missing operator"),
        ("f(x)", 1, "unknown function 'f'"),
        ("1+foo(2)", 3, "unknown function 'foo'"),
        ("x(y)", 1, "unknown function 'x'"),
        ("pi(x)", 1, "constant, not a function"),
        ("sin()", 5, "operand"),
        ("sin(1,2)", 1, "takes 1 argument"),
        ("atan2(1)", 1, "takes 2 argument"),
        ("Max()", 5, "operand"),
        ("log(1,2,3)", 1, "takes 1 to 2"),
        ("()", 2, "empty parentheses"),
        ("x$", 2, "'$' is not part"),
        ("x.y", 2, "'.' is not part"),
        ("x;y", 2, "';'"),
        ("x@y", 2, "'@'"),
        ("a{1}", 2, "'{'"),
        ("a:b", 2, "':'"),
        ("a`b", 2, "'`'"),
        ("'x'", 1, "'''"),
        ('"x"', 1, "'\"'"),
        ("x\\y", 2, "'\\'"),
        ("x%y", 2, "'%'"),
        ("x_", 2, "underscore"),
        ("x__y", 2, "underscore"),
        ("_x", 1, "'_'"),
        ("1.", 2, "decimal point"),
        ("1.e5", 2, "decimal point"),
        ("x\x00", 2, "control character U+0000"),
        ("\x00", 1, "control character U+0000"),
        ("x\x1b[0m", 2, "control character U+001B"),
        ("x\x7f", 2, "control character U+007F"),
        ("x\x0b", 2, "control character U+000B"),
        ("﻿x", 1, "U+FEFF"),
        ("xé", 2, "U+00E9"),
        ("＿", 1, "U+FF3F"),
        ("x＿＿y", 2, "U+FF3F"),
        ("１", 1, "U+FF11"),
        ("١", 1, "U+0661"),
        ("x​", 2, "U+200B"),
        ("x +1", 2, "U+00A0"),
        ("∞", 1, "U+221E"),
    ],
)
def test_refusals_with_their_column(text, column, reason):
    with pytest.raises(ParseError) as caught:
        parse(text)
    assert reason in caught.value.message
    assert caught.value.column == column


@pytest.mark.parametrize("text", ["", " ", "\t\n\r ", "   \n"])
def test_empty_input(text):
    with pytest.raises(ParseError, match="empty"):
        parse(text)


def test_non_text_input_is_refused():
    for value in (None, 1, b"x", ["x"]):
        with pytest.raises(ParseError):
            parse(value)  # type: ignore[arg-type]


def test_unicode_table():
    assert d("2×x") == "(* 2 x)"
    assert d("2·x") == "(* 2 x)"
    assert d("a−b") == "(- a b)"
    assert d("a÷b") == "(/ a b)"
    assert d("a≤b") == "(<= a b)"
    assert d("a≥b") == "(>= a b)"
    assert d("a≠b") == "(!= a b)"
    assert d("π") == "pi"
    assert d("2π") == "(* 2 pi)"


def test_unicode_table_parse_shapes():
    assert d("√2") == "(sqrt 2)"
    assert d("√(x+1)") == "(sqrt (+ x 1))"
    assert d("√x^2") == "(^ (sqrt x) 2)"
    assert d("2√3") == "(* 2 (sqrt 3))"
    assert d("√√2") == "(sqrt (sqrt 2))"
    assert d("x²") == "(^ x 2)"
    assert d("x³") == "(^ x 3)"
    assert d("(x+1)²") == "(^ (+ x 1) 2)"
    assert d("2x²") == "(* 2 (^ x 2))"
    assert d("x²y") == "(* (^ x 2) y)"
    with pytest.raises(ParseError):
        parse("√")
    with pytest.raises(ParseError) as caught:
        parse("x π")
    assert "two names" in caught.value.message


def test_tabs_and_newlines_are_whitespace():
    assert d("x +\n\t1\r") == "(+ x 1)"


def test_length_cap():
    assert d("x" + " " * (MAX_CHARS - 1)) == "x"
    with pytest.raises(ParseError, match="longer than 16,384") as caught:
        parse(" " * MAX_CHARS + "x")
    assert caught.value.column == MAX_CHARS + 1
    with pytest.raises(ParseError, match="longer than"):
        parse("x" * 2**20)


def test_token_cap():
    at_cap = "x" + "+x" * 2047 + "!"
    stats = parse_stats(at_cap)
    assert stats.tokens == MAX_TOKENS
    with pytest.raises(ParseError, match="more than 4,096 tokens"):
        parse(at_cap + "!")
    with pytest.raises(ParseError, match="more than 4,096 tokens"):
        parse("1+" * 5000 + "1")


def test_a_superscript_counts_as_two_tokens():
    assert parse_stats("x²").tokens == 3


def test_the_node_cap_is_never_reached_before_the_token_cap():
    assert MAX_NODES >= 2 * MAX_TOKENS
    densest = [
        "2x+" * 1365 + "2",  # an implicit product per three tokens
        "2(x)+" * 819 + "2",
        "2x" + "*2x" * 1364,
        "-" * 100 + "x" + "!" * 1000,
        "[" * 2 + "1" + "]" * 2,
        "2x" * 1 + "".join(f"+{i}x" for i in range(1, 1000)),
        "x" + "+x" * 2047,
        "sin(x)" * 1,
    ]
    for text in densest:
        stats = parse_stats(text)
        assert stats.nodes <= 2 * stats.tokens
        assert stats.nodes <= MAX_NODES


@given(st.text(alphabet="x2+*-!()[], ", max_size=300))
def test_nodes_never_exceed_twice_the_tokens(text):
    try:
        stats = parse_stats(text)
    except CasInputError:
        return
    assert stats.nodes <= 2 * stats.tokens


def test_depth_cap():
    assert d("(" * MAX_DEPTH + "x" + ")" * MAX_DEPTH) == "x"
    with pytest.raises(ParseError, match="nested more than 200 levels"):
        parse("(" * (MAX_DEPTH + 1) + "x" + ")" * (MAX_DEPTH + 1))
    assert d("-" * MAX_DEPTH + "x").count("neg") == MAX_DEPTH
    with pytest.raises(ParseError, match="nested more than"):
        parse("-" * (MAX_DEPTH + 1) + "x")
    assert parse("2^" * MAX_DEPTH + "2")
    with pytest.raises(ParseError, match="nested more than"):
        parse("2^" * (MAX_DEPTH + 1) + "2")
    with pytest.raises(ParseError, match="nested more than"):
        parse("sin(" * (MAX_DEPTH + 1) + "x" + ")" * (MAX_DEPTH + 1))
    with pytest.raises(ParseError, match="nested more than"):
        parse("[" * 300 + "1" + "]" * 300)


def test_the_parser_uses_few_stack_frames_per_level():
    """Python's recursion limit is never reached: three frames per level at most."""
    frames = 0
    frame = sys._getframe()
    while frame is not None:
        frames += 1
        frame = frame.f_back
    old = sys.getrecursionlimit()
    sys.setrecursionlimit(frames + 3 * MAX_DEPTH + 60)
    try:
        assert parse("(" * MAX_DEPTH + "x" + ")" * MAX_DEPTH)
        assert parse("-" * MAX_DEPTH + "x")
        assert parse("sin(" * MAX_DEPTH + "x" + ")" * MAX_DEPTH)
        assert parse("2^" * MAX_DEPTH + "2")
        assert parse("1+(" * (MAX_DEPTH // 2) + "1" + ")" * (MAX_DEPTH // 2))
    finally:
        sys.setrecursionlimit(old)


@pytest.mark.parametrize(
    "text",
    [
        "1+1*sin(" * 200 + "1" + ")" * 200,
        "1+1*[" * 200 + "1" + "]" * 200,
        "1+(" * 300 + "1" + ")" * 300,
        "1*sqrt(1+" * 200 + "1" + ")" * 200,
        "-1+(" * 150 + "1" + ")" * 150,
    ],
    ids=lambda text: f"{text[:10]!r}x{len(text)}",
)
def test_every_recursive_shape_hits_the_depth_cap_not_python_s(text):
    """Binary and implicit right-hand sides count as depth too, so frames stay bounded."""
    old = sys.getrecursionlimit()
    frames = 0
    frame = sys._getframe()
    while frame is not None:
        frames += 1
        frame = frame.f_back
    sys.setrecursionlimit(frames + 3 * MAX_DEPTH + 60)
    try:
        with pytest.raises(CasInputError, match="nested more than"):
            parse(text)
    finally:
        sys.setrecursionlimit(old)


@pytest.mark.parametrize("text", ["1*-" * 300 + "1", "1+2^" * 300 + "1", "1+√" * 300 + "1"])
def test_flat_chains_of_operators_do_not_nest(text):
    assert parse(text)


def test_a_superscript_cannot_follow_a_superscript():
    with pytest.raises(ParseError, match="power of a power") as caught:
        parse("x²³")
    assert caught.value.column == 3
    assert d("x² ³") != ""  # a space is not a superscript chain


def test_a_long_left_leaning_chain_needs_no_recursion():
    text = "+".join(["x"] * 2000)
    node = parse(text)
    assert dump(node).count("(+") == 1999


def test_fifty_thousand_nested_parentheses_are_refused_quickly():
    with pytest.raises(ParseError, match="longer than"):
        parse("(" * 50_000 + "x" + ")" * 50_000)
    with pytest.raises(ParseError):
        parse("(" * 50_000)


def test_unknown_function_lists_the_known_ones():
    with pytest.raises(ParseError) as caught:
        parse("foo(1)")
    for name in FUNCTIONS:
        assert name in caught.value.message


def test_the_same_text_always_parses_to_the_same_tree():
    text = "x^2 + 2x + 1 = 0"
    assert d(text) == d(text)


def test_render_error_has_a_caret_under_the_column():
    text = "1 + x y"
    with pytest.raises(ParseError) as caught:
        parse(text)
    lines = render_error(caught.value, text).splitlines()
    assert lines[0].startswith("parse: two names side by side")
    assert lines[0].endswith("(column 7)")
    assert lines[1] == "  1 + x y"
    assert lines[2] == "  " + " " * 6 + "^"


def test_render_error_never_echoes_a_huge_input():
    text = "(" * 20_000
    with pytest.raises(ParseError) as caught:
        parse(text)
    assert len(render_error(caught.value, text)) < 400


def test_render_error_hides_control_characters():
    text = "x\x00y\x1b"
    with pytest.raises(ParseError) as caught:
        parse(text)
    rendered = render_error(caught.value, text)
    assert "\x00" not in rendered
    assert "\x1b" not in rendered


# --- identifiers -----------------------------------------------------------------------------


@pytest.mark.parametrize(
    "name",
    ["x", "X", "x1", "abc", "x_1", "a_b_c", "x_1_2", "alpha_beta", "A1_b2", "pi", "sin"],
)
def test_valid_identifiers(name):
    assert is_identifier(name)
    assert re.fullmatch(IDENTIFIER_BODY, name)


@pytest.mark.parametrize(
    "name",
    [
        "",
        "_",
        "_x",
        "x_",
        "x__y",
        "__class__",
        "1x",
        "x y",
        "x-y",
        "x.y",
        "os.system",
        "x\n",
        " x",
        "x ",
        "α",
        "xé",
        "ｘ",
        "x＿y",
        "١",
        "a" + "١",
        "x'",
    ],
)
def test_invalid_identifiers(name):
    assert not is_identifier(name)
    assert not re.fullmatch(IDENTIFIER_BODY, name)


def test_non_text_is_never_an_identifier():
    for value in (None, 1, b"x", ["x"], ("x",)):
        assert not is_identifier(value)


def test_the_published_pattern_is_anchored():
    assert IDENTIFIER_PATTERN == "^" + IDENTIFIER_BODY + "$"


@given(st.text(alphabet="abXY019_ .-", max_size=12))
def test_is_identifier_agrees_with_the_pattern(name):
    assert is_identifier(name) == bool(re.fullmatch(IDENTIFIER_BODY, name))


@given(st.text(max_size=12))
def test_is_identifier_agrees_with_the_pattern_on_any_text(name):
    assert is_identifier(name) == bool(re.fullmatch(IDENTIFIER_BODY, name))


@given(st.from_regex(IDENTIFIER_BODY, fullmatch=True))
def test_a_valid_identifier_is_one_name_to_the_tokenizer(name):
    assert d(name) == name


def test_fuzz_seed_is_available():
    assert isinstance(FUZZ_SEED, int)
