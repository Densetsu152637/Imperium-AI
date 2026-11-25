//! Shared by the test binaries: the token reader's structural invariants, written independently
//! of the reader (positions come from a table built straight from the source text).

#![allow(dead_code)]

use imperium_ratio::{Lexed, TokenKind};

/// `(line, col)` for every byte offset that starts a character, and for the end of the source.
/// A leading byte-order mark is not counted. Offsets inside a character hold `(0, 0)`.
pub fn position_table(src: &str) -> Vec<(u32, u32)> {
    let mut table = vec![(0, 0); src.len() + 1];
    let bom = if src.starts_with('\u{feff}') { 3 } else { 0 };
    let (mut line, mut col) = (1u32, 1u32);
    for (offset, c) in src.char_indices() {
        if offset < bom {
            table[offset] = (1, 1);
            continue;
        }
        table[offset] = (line, col);
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    table[src.len()] = (line, col);
    if bom == src.len() {
        table[src.len()] = (1, 1);
    }
    table
}

/// The tokens in the notation of the conformance files, without the final `Eof`.
pub fn render(lexed: &Lexed) -> String {
    lexed
        .tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Eof)
        .map(|t| t.kind.render())
        .collect::<Vec<_>>()
        .join(" ")
}

fn only_trivia(mut gap: &str) -> bool {
    loop {
        gap = gap.trim_start();
        if gap.is_empty() {
            return true;
        }
        match gap.strip_prefix("//") {
            Some(rest) => {
                gap = match rest.find('\n') {
                    Some(i) => &rest[i..],
                    None => "",
                }
            }
            None => return false,
        }
    }
}

#[derive(PartialEq)]
enum Open {
    Str,
    Interp,
}

/// Checks everything the reader promises about any source, with or without errors:
///
/// - the stream ends in exactly one `Eof` at the end of the source;
/// - spans are ordered, do not overlap, lie on character boundaries, and carry the line and
///   column of their offsets;
/// - strings and interpolations are balanced and properly nested.
///
/// And when the source read without a diagnostic:
///
/// - the text between tokens is only whitespace, comments and a byte-order mark;
/// - brackets and braces are balanced;
/// - a number or punctuation token read alone from its own slice is the same token.
pub fn check_invariants(src: &str, lexed: &Lexed) -> Result<(), String> {
    let tokens = &lexed.tokens;
    let Some(last) = tokens.last() else {
        return Err("no tokens".to_owned());
    };
    if lexed
        .diagnostics
        .first()
        .is_some_and(|d| d.code == imperium_ratio::Code::SourceTooLarge)
    {
        // A source over the limit is not read at all: just an Eof.
        return if tokens.len() == 1 && last.kind == TokenKind::Eof {
            Ok(())
        } else {
            Err("an oversized source produced tokens".to_owned())
        };
    }
    if last.kind != TokenKind::Eof || last.span.start != src.len() || last.span.end != src.len() {
        return Err(format!("the stream does not end in Eof at {}", src.len()));
    }
    if tokens.iter().filter(|t| t.kind == TokenKind::Eof).count() != 1 {
        return Err("more than one Eof".to_owned());
    }
    let table = position_table(src);
    let clean = lexed.diagnostics.is_empty();
    let mut prev_end = 0usize;
    let mut modes: Vec<Open> = Vec::new();
    let mut delims: Vec<char> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let s = token.span;
        if s.start > s.end || s.end > src.len() {
            return Err(format!("token {index} has a bad range {s:?}"));
        }
        if !src.is_char_boundary(s.start) || !src.is_char_boundary(s.end) {
            return Err(format!(
                "token {index} is not on character boundaries {s:?}"
            ));
        }
        if s.start < prev_end {
            return Err(format!("token {index} overlaps its predecessor {s:?}"));
        }
        if (s.line, s.col) != table[s.start] {
            return Err(format!(
                "token {index} starts at {:?}, not {:?}",
                (s.line, s.col),
                table[s.start]
            ));
        }
        if (s.end_line, s.end_col) != table[s.end] {
            return Err(format!(
                "token {index} ends at {:?}, not {:?}",
                (s.end_line, s.end_col),
                table[s.end]
            ));
        }
        if clean {
            let mut gap = &src[prev_end..s.start];
            if prev_end == 0 {
                gap = gap.strip_prefix('\u{feff}').unwrap_or(gap);
            }
            if !only_trivia(gap) {
                return Err(format!("text between tokens is not trivia: {gap:?}"));
            }
        }
        match &token.kind {
            TokenKind::StringStart { .. } => modes.push(Open::Str),
            TokenKind::StringText(_) => {
                if modes.last() != Some(&Open::Str) {
                    return Err("string text outside a string".to_owned());
                }
            }
            TokenKind::InterpStart => {
                if modes.last() != Some(&Open::Str) {
                    return Err("interpolation outside a string".to_owned());
                }
                modes.push(Open::Interp);
                delims.push('#');
            }
            TokenKind::FormatSpec(_) => {
                if modes.last() != Some(&Open::Interp) {
                    return Err("format spec outside an interpolation".to_owned());
                }
            }
            TokenKind::InterpEnd => {
                if modes.pop() != Some(Open::Interp) {
                    return Err("interpolation end without a start".to_owned());
                }
                // Anything left open inside the interpolation is an error the reader reported.
                while delims.last().is_some_and(|d| *d != '#') {
                    delims.pop();
                    if clean {
                        return Err("delimiter left open inside an interpolation".to_owned());
                    }
                }
                delims.pop();
            }
            TokenKind::StringEnd => {
                if modes.pop() != Some(Open::Str) {
                    return Err("string end without a start".to_owned());
                }
            }
            TokenKind::Eof => {}
            other => {
                if modes.last() == Some(&Open::Str) {
                    return Err(format!("code token {other:?} directly inside string text"));
                }
                match other {
                    TokenKind::LParen => delims.push('('),
                    TokenKind::LBracket => delims.push('['),
                    TokenKind::LBrace(_) => delims.push('{'),
                    TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                        let want = match other {
                            TokenKind::RParen => '(',
                            TokenKind::RBracket => '[',
                            _ => '{',
                        };
                        if delims.last() == Some(&want) {
                            delims.pop();
                        } else if clean {
                            return Err(format!("unbalanced {other:?}"));
                        }
                    }
                    _ => {}
                }
            }
        }
        prev_end = s.end;
    }
    if !modes.is_empty() {
        return Err("a string or interpolation is left open at Eof".to_owned());
    }
    if clean && !delims.is_empty() {
        return Err("a delimiter is left open at Eof".to_owned());
    }
    if clean {
        for token in tokens {
            let simple = matches!(
                token.kind,
                TokenKind::Int(_)
                    | TokenKind::Quantity(_)
                    | TokenKind::Float(_)
                    | TokenKind::Duration(_)
                    | TokenKind::Money(_)
            ) || token.kind.punctuation().is_some();
            if !simple {
                continue;
            }
            let slice = &src[token.span.start..token.span.end];
            let again = imperium_ratio::lex(slice);
            let kinds: Vec<&TokenKind> = again
                .tokens
                .iter()
                .map(|t| &t.kind)
                .filter(|k| **k != TokenKind::Eof && **k != TokenKind::Semi { inserted: true })
                .collect();
            if kinds != [&token.kind] {
                return Err(format!(
                    "{slice:?} read alone gives {kinds:?}, not {:?}",
                    token.kind
                ));
            }
        }
    }
    Ok(())
}
