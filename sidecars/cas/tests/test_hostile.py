"""The hostile corpus and the fuzz run, at parse and guard level.

Every input must be refused with a `parse` or `guard` error or give a result, in under 100 ms,
and the audit hook must record nothing outside the allow-list (`support.ALLOWED_EVENTS`: parse.py
and guard.py are pure functions of strings and numbers, so it is empty).
"""

import string
import time
from pathlib import Path

import pytest
from hypothesis import given, seed
from hypothesis import strategies as st

from imperium_cas.functions import FUNCTIONS
from imperium_cas.guard import check
from imperium_cas.parse import (
    MAX_CHARS,
    MAX_DEPTH,
    MAX_TOKENS,
    UNICODE_OPERATORS,
    CasInputError,
    parse,
)
from support import ALLOWED_EVENTS, FUZZ_EXAMPLES, FUZZ_SEED, TIME_LIMIT, run_recorded

CORPUS = Path(__file__).with_name("hostile.txt")


def load_corpus() -> list[tuple[str, str]]:
    """The corpus as `(section, text)` pairs. Escapes in the file are decoded."""
    lines: list[tuple[str, str]] = []
    section = ""
    for raw in CORPUS.read_text(encoding="ascii").splitlines():
        if raw.startswith("# ---"):
            section = raw.strip("# -")
            continue
        if not raw or raw.startswith("#"):
            continue
        lines.append((section, raw.encode("ascii").decode("unicode_escape")))
    return lines


ENTRIES = load_corpus()


def run_one(text: str):
    """Parse and guard `text`. Returns the outcome; asserts the corpus's three rules."""
    best = None
    for _ in range(3):  # best of three: a shared CI runner can stall one run
        started = time.perf_counter()
        outcome, events = run_recorded(lambda: check(parse(text)))
        elapsed = time.perf_counter() - started
        best = elapsed if best is None else min(best, elapsed)
        stray = [event for event in events if event not in ALLOWED_EVENTS]
        assert not stray, (
            f"audit events outside the allow-list: {sorted(set(stray))} for {text[:60]!r}"
        )
        if best < TIME_LIMIT:
            break
    assert best < TIME_LIMIT, f"took {best:.3f}s for {text[:60]!r}"
    if isinstance(outcome, Exception):
        assert isinstance(outcome, CasInputError), f"{type(outcome).__name__}: {outcome}"
        assert outcome.category in ("parse", "guard")
        assert 1 <= outcome.column <= max(len(text), MAX_CHARS) + 1
    return outcome


def test_the_corpus_is_big_enough():
    assert len(ENTRIES) >= 150
    sections = {section for section, _ in ENTRIES}
    assert len(sections) >= 5


@pytest.mark.parametrize(
    "text",
    [
        pytest.param(text, id=f"{index:03d}-{text[:24]!r}")
        for index, (_, text) in enumerate(ENTRIES)
    ],
)
def test_every_corpus_line(text):
    run_one(text)


def test_code_injection_lines_are_never_a_result():
    """Python code, attribute access and string syntax have no meaning in the language."""
    checked = 0
    for section, text in ENTRIES:
        if not section.startswith(("Python code", "string, f-string")):
            continue
        assert isinstance(run_one(text), CasInputError), f"{text!r} gave a result"
        checked += 1
    assert checked >= 100


def test_review_bypasses_are_all_refused():
    """Inputs that once passed the guard and made SymPy build huge integers."""
    checked = 0
    for section, text in ENTRIES:
        if section.startswith("Guard bypasses"):
            assert isinstance(run_one(text), CasInputError), f"{text!r} gave a result"
            checked += 1
    assert checked >= 30


def test_control_characters_and_non_ascii_are_never_a_result():
    for section, text in ENTRIES:
        if section.startswith("control characters"):
            outcome = run_one(text)
            has_control = any(ord(ch) < 32 and ch not in " \t\n\r" or ord(ch) >= 127 for ch in text)
            if has_control:
                assert isinstance(outcome, CasInputError), f"{text!r} gave a result"


@pytest.mark.parametrize(
    "text",
    [
        "x" * 2**20,
        "(" * 50_000 + "x" + ")" * 50_000,
        "(" * 50_000,
        "-" * 50_000 + "x",
        "2^" * 50_000 + "2",
        "[" * 50_000,
        "sin(" * 50_000,
        "1" * 100_000,
        "\x00" * 1000,
        "\ufeff" + "x" * 1000,
        "x+" * 20_000,
        "1+" * (MAX_TOKENS + 1),
        "(" * (MAX_DEPTH + 1) + ")" * (MAX_DEPTH + 1),
        " " * (MAX_CHARS + 1),
        "x" * (MAX_CHARS + 1),
        "\uff3f" * 5000,
        "9" * 1001,
        "1e" + "9" * 1001,
        "2^" * 3000 + "2",
        "factorial(" * 150 + "20" + ")" * 150,
        "10^" * 150 + "10",
        "x!" * 2000,
        "!" * 5000,
        "," * 5000,
        "[1," * 3000,
        "1<" * 3000,
    ],
    ids=lambda text: f"{text[:10]!r}x{len(text)}",
)
def test_huge_and_pathological_inputs_are_refused_fast(text):
    assert isinstance(run_one(text), CasInputError)


def test_the_goal_2_inputs_never_give_a_result():
    inputs = [
        "10^10^10",
        "1e999999999",
        "sqrt(2)^(10^6)",
        "factorial(factorial(20))",
        "factorial(10^7)",
        "[[2,1],[1,1]]^(10^6)",
        "(" * 50_000 + "x" + ")" * 50_000,
    ]
    for text in inputs:
        assert isinstance(run_one(text), CasInputError)
    # These three parse and pass the guard: the worker's limits stop them (plan D3).
    for text in ("(x+y+z)^5000", "exp(x^x^x)", "x"):
        assert not isinstance(run_one(text), CasInputError)


def test_hostile_names_are_not_identifiers():
    from imperium_cas.parse import is_identifier

    for name in ("__class__", "os.system", "x y", "x;y", "__import__", "", "x\n", "a.b", "x-1"):
        assert not is_identifier(name)


# --- fuzzing -----------------------------------------------------------------------------------

GRAMMAR_ALPHABET = (
    string.digits
    + string.ascii_letters
    + "+-*/^!()[],=<>. _\t\n"
    + "".join(UNICODE_OPERATORS)
    + "\u00b2\u00b3\u03c0\u221a"
)

atoms = st.one_of(
    st.integers(0, 10**7).map(str),
    st.sampled_from(["x", "y", "z", "n", "pi", "E", "I", "oo", "x_1", "alpha", "0", "1", "2"]),
    st.from_regex(r"[0-9]{1,3}\.[0-9]{1,3}(e[+-]?[0-9]{1,10})?", fullmatch=True),
)


def extend(children):
    binary = st.tuples(children, st.sampled_from(["+", "-", "*", "/", "^", "**"]), children).map(
        lambda parts: f"{parts[0]}{parts[1]}{parts[2]}"
    )
    call = st.tuples(st.sampled_from(sorted(FUNCTIONS)), st.lists(children, min_size=1, max_size=3))
    return st.one_of(
        binary,
        children.map(lambda c: f"({c})"),
        children.map(lambda c: f"-{c}"),
        children.map(lambda c: f"({c})!"),
        call.map(lambda parts: f"{parts[0]}({', '.join(parts[1])})"),
        st.lists(children, min_size=1, max_size=4).map(lambda xs: "[" + ", ".join(xs) + "]"),
        st.tuples(children, st.sampled_from(["=", "<", "<=", ">", ">=", "!="]), children).map(
            lambda parts: f"{parts[0]} {parts[1]} {parts[2]}"
        ),
    )


expressions = st.recursive(atoms, extend, max_leaves=30)


@seed(FUZZ_SEED)
@given(st.text(alphabet=GRAMMAR_ALPHABET, max_size=150))
def test_fuzz_grammar_alphabet(text):
    run_one(text)


@seed(FUZZ_SEED)
@given(st.text(max_size=100))
def test_fuzz_any_text(text):
    run_one(text)


@seed(FUZZ_SEED)
@given(st.binary(max_size=100).map(lambda raw: raw.decode("latin-1")))
def test_fuzz_random_bytes(text):
    run_one(text)


@seed(FUZZ_SEED)
@given(expressions)
def test_fuzz_expressions_shaped_like_the_grammar(text):
    run_one(text)


def test_fuzz_settings_are_the_ci_ones():
    from hypothesis import settings

    assert settings().max_examples == FUZZ_EXAMPLES
