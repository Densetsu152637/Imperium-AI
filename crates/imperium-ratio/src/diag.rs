//! Lexical diagnostics.
//!
//! The specification defines the limits that apply while reading tokens but no lexical
//! diagnostic codes (its `E` codes belong to the checker), so the token reader uses its own `L`
//! series. Each code has at least one `lex-error` conformance file.

use std::fmt;

use crate::token::Span;

/// The lexical diagnostic codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Code {
    /// A character that starts no token.
    UnexpectedCharacter,
    /// A string that ends at a line break (short strings) or at the end of the source.
    UnterminatedString,
    /// An unknown or malformed escape.
    InvalidEscape,
    /// A `\r` that is not part of `\r\n`.
    BareCarriageReturn,
    /// A lone `}` in string text (write `\}`).
    LoneCloseBrace,
    /// A line break inside an interpolation of a short string.
    LineBreakInInterpolation,
    /// An interpolation that is not closed before the end of the source.
    UnterminatedInterpolation,
    /// Letters directly after a number that are not a unit, or a bad digit separator.
    InvalidNumericSuffix,
    /// A number outside the range of its type.
    NumberOutOfRange,
    /// A quantity, duration or amount that cannot be exact.
    NumberNotExact,
    /// Interpolations nested deeper than the limit.
    InterpolationTooDeep,
    /// Brackets, blocks and interpolations nested deeper than the limit.
    NestingTooDeep,
    /// A source larger than the package-source limit.
    SourceTooLarge,
    /// A closing delimiter that matches nothing, or an opening one never closed.
    UnbalancedDelimiter,
    /// More diagnostics than the reader reports; it stops.
    TooManyErrors,
    /// A `{`, `}` or line break in a format spec.
    InvalidFormatSpec,
}

impl Code {
    /// Every code, in numeric order.
    pub const ALL: [Code; 16] = [
        Code::UnexpectedCharacter,
        Code::UnterminatedString,
        Code::InvalidEscape,
        Code::BareCarriageReturn,
        Code::LoneCloseBrace,
        Code::LineBreakInInterpolation,
        Code::UnterminatedInterpolation,
        Code::InvalidNumericSuffix,
        Code::NumberOutOfRange,
        Code::NumberNotExact,
        Code::InterpolationTooDeep,
        Code::NestingTooDeep,
        Code::SourceTooLarge,
        Code::UnbalancedDelimiter,
        Code::TooManyErrors,
        Code::InvalidFormatSpec,
    ];

    /// The code as written in diagnostics and conformance files (`L0001`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::UnexpectedCharacter => "L0001",
            Code::UnterminatedString => "L0002",
            Code::InvalidEscape => "L0003",
            Code::BareCarriageReturn => "L0004",
            Code::LoneCloseBrace => "L0005",
            Code::LineBreakInInterpolation => "L0006",
            Code::UnterminatedInterpolation => "L0007",
            Code::InvalidNumericSuffix => "L0008",
            Code::NumberOutOfRange => "L0009",
            Code::NumberNotExact => "L0010",
            Code::InterpolationTooDeep => "L0011",
            Code::NestingTooDeep => "L0012",
            Code::SourceTooLarge => "L0013",
            Code::UnbalancedDelimiter => "L0014",
            Code::TooManyErrors => "L0015",
            Code::InvalidFormatSpec => "L0016",
        }
    }

    /// The code spelled `text`, if there is one.
    pub fn from_code(text: &str) -> Option<Code> {
        Code::ALL.into_iter().find(|code| code.as_str() == text)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A problem found while reading tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub message: String,
    pub span: Span,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: error[{}]: {}",
            self.span.line, self.span.col, self.code, self.message
        )
    }
}
