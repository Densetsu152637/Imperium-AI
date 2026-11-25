//! Ratio, the programming language for agent harnesses.
//!
//! This crate is built in stages (chapter 10 of the specification). Stage 1, the token reader, is
//! here: [`lex`] turns source text into tokens with spans, inserts semicolons at line ends, reads
//! string interpolation with a mode stack, and reports lexical errors without panicking. Nothing
//! in the crate is wired into the engine yet.

mod diag;
mod lexer;
mod number;
mod token;

pub use diag::{Code, Diagnostic};
pub use lexer::{LexOptions, Lexed, MAX_INTERPOLATION_DEPTH, MAX_NESTING, lex, lex_with};
pub use token::{BraceKind, Contextual, Keyword, Span, Token, TokenKind};

/// The revision of the Ratio specification (`Densetsu152637/Ratio`) this crate implements.
///
/// `ratio-spec.lock` names the same commit in full; a test keeps the two in step. A language
/// change is a pull request in the specification repository first, then a bump of both here.
pub const SPEC_REVISION: &str = "4790c7c3b867aa5fb1a539716d3c54c713df36c1";

/// The draft number of that revision.
pub const SPEC_DRAFT: &str = "0.5";

/// The implementation stage reached (chapter 10's table): the token reader.
pub const STAGE: u32 = 1;
