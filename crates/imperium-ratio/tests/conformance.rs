//! The conformance runner (chapter 10, "The conformance suite").
//!
//! Every `.ratio` file under `conformance/` starts with expectation lines. This stage runs the
//! ones about reading tokens:
//!
//! ```text
//! // expect: lex-ok                       (no diagnostics)
//! // expect: lex-error L0001 at 7:13      (one line per diagnostic, in order)
//! // tokens: kw:let ident:x = int:1 ;!    (the tokens, in the notation of TokenKind::render;
//! //                                       several `tokens:` lines join with a space)
//! // option: max_interpolation_depth 1    (a LexOptions field)
//! ```
//!
//! Files whose expectations are `ok`, `value`, `check-error`, `runtime-error` or `fixture` belong
//! to later stages; the runner counts them and skips them.

mod support;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use imperium_ratio::{Code, LexOptions, SPEC_REVISION, lex_with};

#[derive(Default)]
struct Case {
    lex_ok: bool,
    errors: Vec<(String, u32, u32)>,
    tokens: Option<String>,
    options: LexOptions,
    other_stage: bool,
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "ratio") {
            out.push(path);
        }
    }
}

fn parse(path: &Path, text: &str) -> Result<Case, String> {
    let mut case = Case::default();
    let mut token_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("// ") else {
            break;
        };
        if let Some(expect) = rest.strip_prefix("expect: ") {
            let words: Vec<&str> = expect.split_whitespace().collect();
            match words.as_slice() {
                ["lex-ok"] => case.lex_ok = true,
                ["lex-error", code, "at", position] => {
                    let (l, c) = position
                        .split_once(':')
                        .ok_or_else(|| format!("{}: bad position {position}", path.display()))?;
                    let l = l
                        .parse()
                        .map_err(|_| format!("{}: bad line", path.display()))?;
                    let c = c
                        .parse()
                        .map_err(|_| format!("{}: bad column", path.display()))?;
                    case.errors.push(((*code).to_owned(), l, c));
                }
                ["ok" | "value" | "check-error" | "runtime-error", ..] => case.other_stage = true,
                _ => {
                    return Err(format!(
                        "{}: unknown expectation `{expect}`",
                        path.display()
                    ));
                }
            }
        } else if let Some(tokens) = rest.strip_prefix("tokens: ") {
            token_lines.push(tokens.trim());
        } else if rest.starts_with("fixture: ") {
            case.other_stage = true;
        } else if let Some(option) = rest.strip_prefix("option: ") {
            let (name, value) = option
                .split_once(' ')
                .ok_or_else(|| format!("{}: bad option `{option}`", path.display()))?;
            let value: usize = value
                .trim()
                .parse()
                .map_err(|_| format!("{}: bad option value `{value}`", path.display()))?;
            match name {
                "max_source_bytes" => case.options.max_source_bytes = value,
                "max_interpolation_depth" => case.options.max_interpolation_depth = value,
                "max_nesting" => case.options.max_nesting = value,
                "max_diagnostics" => case.options.max_diagnostics = value,
                _ => return Err(format!("{}: unknown option `{name}`", path.display())),
            }
        }
    }
    if !token_lines.is_empty() {
        case.tokens = Some(token_lines.join(" "));
    }
    Ok(case)
}

#[test]
fn the_lexer_conformance_files_pass() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("conformance");
    let mut paths = Vec::new();
    files(&root, &mut paths);
    paths.sort();
    assert!(
        paths.len() >= 30,
        "expected the stage 1 corpus, found {} files",
        paths.len()
    );

    let mut failures = Vec::new();
    let mut run = 0usize;
    let mut skipped = 0usize;
    let mut codes_seen: BTreeSet<String> = BTreeSet::new();
    for path in &paths {
        let name = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let case = match parse(path, &text) {
            Ok(case) => case,
            Err(problem) => {
                failures.push(problem);
                continue;
            }
        };
        if case.other_stage && !case.lex_ok && case.errors.is_empty() && case.tokens.is_none() {
            skipped += 1;
            continue;
        }
        if !case.lex_ok && case.errors.is_empty() {
            failures.push(format!(
                "{name}: no lexer expectation (use lex-ok or lex-error)"
            ));
            continue;
        }
        run += 1;
        let lexed = lex_with(&text, &case.options);
        if let Err(problem) = support::check_invariants(&text, &lexed) {
            failures.push(format!("{name}: invariant broken: {problem}"));
        }
        let got: Vec<(String, u32, u32)> = lexed
            .diagnostics
            .iter()
            .map(|d| (d.code.as_str().to_owned(), d.span.line, d.span.col))
            .collect();
        if got != case.errors {
            failures.push(format!(
                "{name}: diagnostics differ\n  expected {:?}\n  got      {:?}\n  messages {:?}",
                case.errors,
                got,
                lexed
                    .diagnostics
                    .iter()
                    .map(|d| d.message.as_str())
                    .collect::<Vec<_>>()
            ));
        }
        for (code, _, _) in &case.errors {
            codes_seen.insert(code.clone());
        }
        if let Some(expected) = &case.tokens {
            let rendered = support::render(&lexed);
            if rendered != *expected {
                failures.push(format!(
                    "{name}: tokens differ\n  expected {expected}\n  got      {rendered}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failure(s)\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(run >= 30, "only {run} lexer files ran ({skipped} skipped)");

    // Every diagnostic code has at least one lex-error file.
    for code in Code::ALL {
        assert!(
            codes_seen.contains(code.as_str()),
            "no conformance file expects {code}"
        );
    }
}

#[test]
fn the_spec_pin_names_one_revision() {
    let lock = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ratio-spec.lock"))
        .expect("ratio-spec.lock");
    let commit = lock
        .lines()
        .find_map(|line| line.strip_prefix("commit = "))
        .expect("a commit line");
    assert_eq!(commit.trim(), SPEC_REVISION);
    assert!(SPEC_REVISION.starts_with("4790c7c"));
}

#[test]
fn the_builtins_drift_check_is_still_a_stub() {
    // Chapter 10 generates `builtins.txt` from the pinned specification and fails CI when the
    // checked-in file differs. No stage before 7 has built-ins, so the file holds comments only;
    // when the generator lands, this test becomes the drift check.
    let text = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("conformance")
            .join("builtins.txt"),
    )
    .expect("builtins.txt");
    assert!(
        text.lines()
            .all(|line| line.trim().is_empty() || line.starts_with('#')),
        "builtins.txt has entries but no generator checks them yet"
    );
}
