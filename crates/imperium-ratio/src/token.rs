//! Tokens, spans, keywords and the notation tests use to write tokens down.

/// A range of the source with the line and column of both ends.
///
/// `start` and `end` are byte offsets into the source string (a byte-order mark counts, so
/// offsets index the original text). `line` and `col` are 1-based; a column counts Unicode scalar
/// values, and a byte-order mark is not counted. The end position is exclusive. Tokens the reader
/// inserts (a semicolon at a line end, the closers of an unterminated string) are zero-width.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// One token and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

macro_rules! words {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $text:literal),* $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),* }

        impl $name {
            /// Every word, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            /// The source spelling.
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),* }
            }

            /// The word spelled `text`, if there is one.
            pub fn from_word(text: &str) -> Option<$name> {
                match text { $($text => Some($name::$variant),)* _ => None }
            }
        }
    };
}

words! {
    /// Reserved words. They are identifiers only after `.`, `?.` and `::`.
    Keyword {
        And => "and", As => "as", Break => "break", Catch => "catch", Const => "const",
        Continue => "continue", Defer => "defer", Do => "do", Else => "else", Enum => "enum",
        False => "false", Fn => "fn", For => "for", Harness => "harness", If => "if",
        Import => "import", In => "in", Is => "is", Let => "let", Loop => "loop",
        Match => "match", None => "none", Not => "not", Or => "or", Prompt => "prompt",
        Pub => "pub", Raise => "raise", Repeat => "repeat", Return => "return", Test => "test",
        True => "true", Try => "try", Type => "type", Var => "var", While => "while",
        With => "with",
    }
}

words! {
    /// Words that are keywords in one position each and ordinary identifiers elsewhere.
    Contextual {
        Meta => "meta", State => "state", Par => "par", Race => "race", Timeout => "timeout",
        System => "system", Instruct => "instruct", Task => "task", Data => "data",
        Example => "example", Format => "format", Steps => "steps", Context => "context",
        Note => "note", Include => "include", Image => "image",
    }
}

impl Contextual {
    /// The section words of a prompt body.
    pub fn prompt_section(text: &str) -> Option<Contextual> {
        match Contextual::from_word(text)? {
            Contextual::Meta
            | Contextual::State
            | Contextual::Par
            | Contextual::Race
            | Contextual::Timeout => None,
            section => Some(section),
        }
    }
}

/// What a `{` opens, as far as the token reader can tell without the parser.
///
/// Semicolon insertion needs it: a line end inside a block or a prompt body can end a statement,
/// inside the other kinds it is only whitespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BraceKind {
    /// A block (statements; semicolons are inserted).
    Block,
    /// A record literal, record type, record pattern or import list.
    Record,
    /// A prompt body (sections; semicolons are inserted).
    PromptBody,
    /// An enum body.
    EnumBody,
    /// The arm list of a `match` or a `catch`.
    MatchBody,
}

impl BraceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            BraceKind::Block => "block",
            BraceKind::Record => "record",
            BraceKind::PromptBody => "prompt",
            BraceKind::EnumBody => "enum",
            BraceKind::MatchBody => "match",
        }
    }
}

/// What a token is.
///
/// A string is a run of tokens: `StringStart`, then `StringText` and interpolations in source
/// order, then `StringEnd`. An interpolation is `InterpStart`, code tokens, an optional
/// `FormatSpec`, then `InterpEnd`. A raw string is the single token `RawString`.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Keyword(Keyword),
    Contextual(Contextual),
    /// A decimal or hexadecimal integer (64-bit signed; larger literals are an error).
    Int(i64),
    /// `20k`, `1.5M`: the scaled integer value.
    Quantity(i64),
    Float(f64),
    /// A duration in milliseconds.
    Duration(i64),
    /// An amount of money in millionths of a dollar.
    Money(i64),
    /// An opening `"` or `"""`.
    StringStart {
        long: bool,
    },
    /// A run of string text with escapes decoded and, in a long string, indentation removed.
    StringText(String),
    InterpStart,
    /// The text after the `:` of an interpolation (the colon is part of the span only).
    FormatSpec(String),
    InterpEnd,
    StringEnd,
    /// `r"…"` or `r"""…"""`: the text is final (no escapes; long strings are dedented).
    RawString {
        text: String,
        long: bool,
    },
    /// A `///` comment; the text is everything after the three slashes.
    DocComment(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace(BraceKind),
    RBrace,
    Comma,
    /// `;`: written in the source, or `inserted` at a line end.
    Semi {
        inserted: bool,
    },
    Colon,
    ColonColon,
    Dot,
    DotDot,
    DotDotEq,
    Question,
    QuestionDot,
    QuestionQuestion,
    At,
    Arrow,
    FatArrow,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Pipe,
    PipePipe,
    PipeGt,
    Eof,
}

impl TokenKind {
    /// The one-line notation the conformance files use (`ident:x`, `kw:let`, `int:42`, `;!` for an
    /// inserted semicolon, `{block`, `text:"…"`).
    pub fn render(&self) -> String {
        match self {
            TokenKind::Ident(s) => format!("ident:{s}"),
            TokenKind::Keyword(k) => format!("kw:{}", k.as_str()),
            TokenKind::Contextual(c) => format!("ctx:{}", c.as_str()),
            TokenKind::Int(v) => format!("int:{v}"),
            TokenKind::Quantity(v) => format!("qty:{v}"),
            TokenKind::Float(v) => format!("float:{v:?}"),
            TokenKind::Duration(v) => format!("dur:{v}ms"),
            TokenKind::Money(v) => format!("money:{v}u"),
            TokenKind::StringStart { long: false } => "str[".to_owned(),
            TokenKind::StringStart { long: true } => "str3[".to_owned(),
            TokenKind::StringText(t) => format!("text:{t:?}"),
            TokenKind::InterpStart => "interp[".to_owned(),
            TokenKind::FormatSpec(t) => format!("spec:{t:?}"),
            TokenKind::InterpEnd => "]interp".to_owned(),
            TokenKind::StringEnd => "]str".to_owned(),
            TokenKind::RawString { text, long: false } => format!("raw:{text:?}"),
            TokenKind::RawString { text, long: true } => format!("raw3:{text:?}"),
            TokenKind::DocComment(t) => format!("doc:{t:?}"),
            TokenKind::LBrace(kind) => format!("{{{}", kind.as_str()),
            TokenKind::Semi { inserted: true } => ";!".to_owned(),
            TokenKind::Eof => "eof".to_owned(),
            other => other.punctuation().unwrap_or("?").to_owned(),
        }
    }

    /// The spelling of a punctuation token, `None` for everything else.
    pub const fn punctuation(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::RBrace => "}",
            TokenKind::Comma => ",",
            TokenKind::Semi { inserted: false } => ";",
            TokenKind::Colon => ":",
            TokenKind::ColonColon => "::",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::DotDotEq => "..=",
            TokenKind::Question => "?",
            TokenKind::QuestionDot => "?.",
            TokenKind::QuestionQuestion => "??",
            TokenKind::At => "@",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Eq => "=",
            TokenKind::EqEq => "==",
            TokenKind::NotEq => "!=",
            TokenKind::Lt => "<",
            TokenKind::Le => "<=",
            TokenKind::Gt => ">",
            TokenKind::Ge => ">=",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::PlusEq => "+=",
            TokenKind::MinusEq => "-=",
            TokenKind::StarEq => "*=",
            TokenKind::SlashEq => "/=",
            TokenKind::Pipe => "|",
            TokenKind::PipePipe => "||",
            TokenKind::PipeGt => "|>",
            _ => return None,
        })
    }
}
