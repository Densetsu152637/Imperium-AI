//! Property tests for the token reader: it never panics, it is deterministic, and its spans
//! round-trip (see `support::check_invariants`). Inputs come from a fixed-seed generator, so a
//! failure reproduces; the seed and case number are in the message.

mod support;

use std::fs;
use std::path::Path;

use imperium_ratio::{LexOptions, lex, lex_with};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        usize::try_from(self.next() % u64::try_from(n).unwrap_or(1)).unwrap_or(0)
    }
}

const FRAGMENTS: &[&str] = &[
    "let",
    "x",
    "y",
    "_",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    "\r",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    "\"",
    "\"\"\"",
    "r\"",
    "r\"\"\"",
    "\\",
    "\\n",
    "\\u{41}",
    "\\u{",
    "\\q",
    "//",
    "///",
    "// c\n",
    ":",
    "::",
    ".",
    "..",
    "..=",
    "?",
    "?.",
    "??",
    "|>",
    "|",
    "||",
    "->",
    "=>",
    "=",
    "==",
    "!=",
    "<",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "%",
    "1",
    "0x1F",
    "1_000",
    "20k",
    "1.5M",
    "1e-3",
    "500ms",
    "30s",
    "5m",
    "2h",
    "$0.50",
    "$",
    "if",
    "else",
    "match",
    "fn",
    "harness",
    "prompt",
    "enum",
    "with",
    "timeout",
    "par",
    "race",
    "state",
    "meta",
    "type",
    "for",
    "in",
    "catch",
    "try",
    "import",
    "system",
    "data",
    "context",
    "é",
    "😀",
    "\u{feff}",
    "~",
    "@",
    ";",
    ",",
    "and",
    "or",
    "none",
    "true",
    "return",
    "break",
    "pub",
    "{{",
    "}}",
    "\"{",
    "}\"",
    "{x}",
    "{x:.2}",
    "\n  ",
    "\n\n\n",
    "9223372036854775808",
    "99999999999999999999",
];

fn from_fragments(rng: &mut Rng, max: usize) -> String {
    let count = rng.below(max + 1);
    let mut s = String::new();
    for _ in 0..count {
        s.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
    }
    s
}

fn from_characters(rng: &mut Rng, max: usize) -> String {
    let count = rng.below(max + 1);
    let mut s = String::new();
    for _ in 0..count {
        let c = match rng.below(4) {
            0 => char::from_u32(u32::try_from(rng.below(0x11_0000)).unwrap_or(0)),
            1 => char::from_u32(u32::try_from(rng.below(0x80)).unwrap_or(0)),
            _ => FRAGMENTS[rng.below(FRAGMENTS.len())].chars().next(),
        };
        s.push(c.unwrap_or('?'));
    }
    s
}

fn check(src: &str, label: &str) {
    let lexed = lex(src);
    if let Err(problem) = support::check_invariants(src, &lexed) {
        panic!("{label}: {problem}\nsource: {src:?}");
    }
    assert_eq!(
        lexed,
        lex(src),
        "{label}: reading is not deterministic: {src:?}"
    );
}

#[test]
fn arbitrary_fragment_soup_is_read_without_panicking() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for case in 0..4000 {
        let src = from_fragments(&mut rng, 80);
        check(&src, &format!("fragments case {case}"));
    }
}

#[test]
fn arbitrary_characters_are_read_without_panicking() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for case in 0..2000 {
        let src = from_characters(&mut rng, 160);
        check(&src, &format!("characters case {case}"));
    }
}

fn corpus() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let mut out = Vec::new();
    let mut dirs = vec![dir];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "ratio")
                && let Ok(text) = fs::read_to_string(&path)
            {
                out.push(text);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn mutated_conformance_programs_keep_the_invariants() {
    let corpus = corpus();
    assert!(!corpus.is_empty(), "the conformance corpus is empty");
    let mut rng = Rng(0xA5A5_1234_5678_9ABC);
    for (n, original) in corpus.iter().enumerate() {
        check(original, &format!("corpus file {n}"));
        let chars: Vec<char> = original.chars().collect();
        for case in 0..60 {
            let mut mutated = chars.clone();
            for _ in 0..=rng.below(3) {
                let at = rng.below(mutated.len() + 1);
                match rng.below(4) {
                    0 => {
                        let end = (at + rng.below(12)).min(mutated.len());
                        mutated.drain(at..end);
                    }
                    1 => {
                        let end = (at + rng.below(12)).min(mutated.len());
                        let copy: Vec<char> = mutated[at..end].to_vec();
                        for (i, c) in copy.into_iter().enumerate() {
                            mutated.insert(at + i, c);
                        }
                    }
                    2 => {
                        let piece = FRAGMENTS[rng.below(FRAGMENTS.len())];
                        for (i, c) in piece.chars().enumerate() {
                            mutated.insert(at + i, c);
                        }
                    }
                    _ => mutated.truncate(at),
                }
            }
            let src: String = mutated.into_iter().collect();
            check(&src, &format!("mutation {case} of corpus file {n}"));
        }
    }
}

#[test]
fn every_prefix_of_a_program_is_read_without_panicking() {
    let src = "meta { name: \"x\" }\n\n/// Doc.\n@turn\npub harness turn(t: Turn) -> Reply\n  with { context: main }\n{\n  let s = \"\"\"\n    a {if ok { \"y\" } else { \"n\" }:>6}\n    b\n    \"\"\"\n  match s {\n    Denied { by } if by == user => 1,\n    _ => 2\n  }\n}\n";
    for (end, _) in src.char_indices() {
        check(&src[..end], &format!("prefix of {end} bytes"));
    }
    check(src, "the whole program");
}

#[test]
fn hostile_shapes_stay_linear_and_balanced() {
    let big = 200_000;
    let cases = [
        "(".repeat(big),
        ")".repeat(big),
        "{".repeat(big),
        "}".repeat(big),
        "\"{".repeat(big / 2),
        "{x}\"".repeat(big / 4),
        "\"\"\"".repeat(big / 3),
        "if ".repeat(big / 3),
        "match ".repeat(big / 6),
        "a\n\n".repeat(big / 3) + "\n.b",
        format!("a{}.b", "\n".repeat(big)),
        "\n".repeat(big) + "{",
        "{\n".repeat(big / 2),
        "(\n".repeat(big / 2) + &"\n)".repeat(big / 4),
        "\\".repeat(big),
        "\"\\u{".repeat(big / 4),
        "$".repeat(big),
        "1".repeat(big),
        "0x".repeat(big / 2),
        "1e".repeat(big / 2),
        "///".repeat(big / 3),
        "r\"".repeat(big / 2),
    ];
    for (n, src) in cases.iter().enumerate() {
        let lexed = lex_with(
            src,
            &LexOptions {
                max_source_bytes: usize::MAX,
                ..LexOptions::default()
            },
        );
        if let Err(problem) = support::check_invariants(src, &lexed) {
            panic!("hostile case {n}: {problem}");
        }
    }
}
