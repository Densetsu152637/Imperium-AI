# imperium-ratio

Ratio is a programming language for agent harnesses. This crate is its implementation, built in
stages (chapter 10 of the specification). It is standalone: nothing in Imperium depends on it yet,
and it is not wired into the engine.

## Spec revision

The crate implements draft 0.5 of the specification at revision
`4790c7c3b867aa5fb1a539716d3c54c713df36c1` of
[`Densetsu152637/Ratio`](https://github.com/Densetsu152637/Ratio). The pin is in two places that a
test keeps in step: `SPEC_REVISION` in `src/lib.rs` and `ratio-spec.lock`. A language change is a
pull request in the specification repository first, then a bump of both here; every
implementation pull request names the revision it implements.

## Stage 1: the token reader

`imperium_ratio::lex(source)` returns tokens with spans (byte range plus 1-based line and column
at both ends) and diagnostics. It implements chapter 02's lexical structure:

- comments and `///` documentation comments (kept as tokens), identifiers, the 36 reserved words,
  the contextual keywords in their one position each, and members after `.`, `?.` and `::`;
- integers (decimal, hex, `_` separators), quantities (`20k`, `1.5M`), floats, durations
  (`500ms`, `30s`, `5m`, `2h`) and money (`$0.50`, exact to a millionth);
- strings with a mode stack: short, long (dedented), raw and raw long strings, escapes, nested
  interpolation with format specs;
- semicolon insertion at line ends by the three conditions of chapter 02, using the kind of the
  innermost `{` (block, record, prompt body, enum body, match arms) that the reader works out from
  the keyword before it and the record rule of chapter 02;
- the limits of chapter 07's table that apply while reading: source size (`LexOptions`, 256 KiB,
  64 KiB for an authored harness), interpolation nesting (4, at most 8) and bracket nesting
  (128, at most 256);
- recovery: errors never panic, the stream always ends in `Eof`, strings and interpolations stay
  balanced, and at most 100 diagnostics are reported. The specification defines no lexical
  diagnostic codes, so the reader uses `L0001` to `L0016` (`Code`).

### Tests

- `conformance/syntax/*.ratio` are the first conformance files, in chapter 10's format
  (`// expect:` lines). The stage 1 runner (`tests/conformance.rs`) understands `lex-ok`,
  `lex-error CODE at LINE:COL`, `tokens:` (the expected tokens in `TokenKind::render` notation)
  and `option:` (a `LexOptions` field), and skips files whose expectations belong to later stages
  (`ok`, `value`, `check-error`, `runtime-error`, `fixture`). Every diagnostic code must have a
  `lex-error` file.
- `tests/properties.rs` reads fixed-seed random inputs, mutated conformance programs and hostile
  shapes: the reader never panics, is deterministic, and its spans round-trip (ordered, in
  bounds, on character boundaries, correct line and column, only trivia between tokens in a clean
  read, a number or punctuation token read alone from its slice is the same token). The crate has
  no dependencies, so the generator is a small xorshift rather than `proptest`.
- `conformance/builtins.txt` is a stub. Chapter 10 generates it from chapter 06 at the pinned
  revision and fails CI on drift; no built-ins exist before stage 7, so the generator and the
  drift check land then.

All of it runs in CI; nothing needs a local build.
