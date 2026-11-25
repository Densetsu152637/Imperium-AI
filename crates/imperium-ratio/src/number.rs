//! Number, quantity, duration and money literals.
//!
//! Every literal is ASCII, so the scanner works on bytes. A scan always consumes the whole run of
//! digits, separators and unit letters, so one bad literal gives one diagnostic.

use crate::diag::Code;
use crate::token::TokenKind;

/// Why a literal was refused.
pub(crate) struct NumError {
    pub(crate) code: Code,
    pub(crate) message: String,
}

fn error(code: Code, message: &str) -> NumError {
    NumError {
        code,
        message: message.to_owned(),
    }
}

/// Scans the literal starting at `start` (a digit or `$`) and returns where it ends.
pub(crate) fn scan(src: &str, start: usize) -> (usize, Result<TokenKind, NumError>) {
    let b = src.as_bytes();
    match b.get(start) {
        Some(b'$') => scan_money(src, start),
        Some(b'0')
            if b.get(start + 1) == Some(&b'x')
                && b.get(start + 2).is_some_and(u8::is_ascii_hexdigit) =>
        {
            scan_hex(src, start)
        }
        _ => scan_decimal(src, start),
    }
}

/// Digits with `_` separators; a separator needs a digit on both sides. Returns the end and the
/// digits without separators.
fn digits(b: &[u8], start: usize, hex: bool) -> (usize, String) {
    let is_digit = |c: u8| {
        if hex {
            c.is_ascii_hexdigit()
        } else {
            c.is_ascii_digit()
        }
    };
    let mut i = start;
    let mut out = String::new();
    loop {
        match b.get(i) {
            Some(&c) if is_digit(c) => {
                out.push(char::from(c));
                i += 1;
            }
            Some(b'_') if !out.is_empty() && b.get(i + 1).is_some_and(|&n| is_digit(n)) => {
                i += 1;
            }
            _ => break,
        }
    }
    (i, out)
}

/// The end of a run of ASCII letters, digits and underscores.
fn skip_word(b: &[u8], start: usize) -> usize {
    let mut i = start;
    while b
        .get(i)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    {
        i += 1;
    }
    i
}

fn bad_suffix(text: &str) -> NumError {
    NumError {
        code: Code::InvalidNumericSuffix,
        message: format!(
            "`{text}` is not a unit; use k, M, ms, s, m or h, and `_` only between digits"
        ),
    }
}

fn out_of_range(what: &str) -> NumError {
    NumError {
        code: Code::NumberOutOfRange,
        message: format!("{what} is outside the range of a 64-bit signed integer"),
    }
}

fn scan_hex(src: &str, start: usize) -> (usize, Result<TokenKind, NumError>) {
    let b = src.as_bytes();
    let (i, hex_digits) = digits(b, start + 2, true);
    let end = skip_word(b, i);
    if end != i {
        let text = src.get(i..end).unwrap_or("");
        return (end, Err(bad_suffix(text)));
    }
    match i64::from_str_radix(&hex_digits, 16) {
        Ok(value) => (end, Ok(TokenKind::Int(value))),
        Err(_) => (end, Err(out_of_range("the hexadecimal literal"))),
    }
}

fn scan_money(src: &str, start: usize) -> (usize, Result<TokenKind, NumError>) {
    let b = src.as_bytes();
    if !b.get(start + 1).is_some_and(u8::is_ascii_digit) {
        return (
            start + 1,
            Err(error(
                Code::UnexpectedCharacter,
                "`$` must start an amount of money, such as `$0.50`",
            )),
        );
    }
    let (mut i, whole) = digits(b, start + 1, false);
    let mut fraction = String::new();
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        let (j, f) = digits(b, i + 1, false);
        i = j;
        fraction = f;
    }
    let end = skip_word(b, i);
    if end != i {
        let text = src.get(i..end).unwrap_or("");
        return (end, Err(bad_suffix(text)));
    }
    // Amounts are exact to 1e-6: more digits are fine only when they are zeros.
    let extra = fraction.get(6..).unwrap_or("");
    if extra.bytes().any(|c| c != b'0') {
        return (
            end,
            Err(error(
                Code::NumberNotExact,
                "an amount of money is exact to six decimal places",
            )),
        );
    }
    let head = fraction.get(..6).unwrap_or(fraction.as_str());
    let padded = format!("{head:0<6}");
    let micros = whole
        .parse::<u128>()
        .ok()
        .and_then(|w| w.checked_mul(1_000_000))
        .and_then(|w| padded.parse::<u128>().ok().and_then(|f| w.checked_add(f)))
        .and_then(|v| i64::try_from(v).ok());
    match micros {
        Some(value) => (end, Ok(TokenKind::Money(value))),
        None => (end, Err(out_of_range("the amount of money"))),
    }
}

fn scan_decimal(src: &str, start: usize) -> (usize, Result<TokenKind, NumError>) {
    let b = src.as_bytes();
    let (mut i, whole) = digits(b, start, false);
    let mut fraction: Option<String> = None;
    if b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
        let (j, f) = digits(b, i + 1, false);
        i = j;
        fraction = Some(f);
    }
    let mut exponent: Option<String> = None;
    if matches!(b.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        let mut sign = "";
        match b.get(j) {
            Some(b'+') => {
                j += 1;
            }
            Some(b'-') => {
                sign = "-";
                j += 1;
            }
            _ => {}
        }
        if b.get(j).is_some_and(u8::is_ascii_digit) {
            let (k, e) = digits(b, j, false);
            exponent = Some(format!("{sign}{e}"));
            i = k;
        }
    }
    let end = skip_word(b, i);
    let suffix = src.get(i..end).unwrap_or("");
    let is_float = fraction.is_some() || exponent.is_some();
    let result = match suffix {
        "" if is_float => float(&whole, fraction.as_deref(), exponent.as_deref()),
        "" => whole
            .parse::<u128>()
            .ok()
            .and_then(|v| i64::try_from(v).ok())
            .map(TokenKind::Int)
            .ok_or_else(|| out_of_range("the integer literal")),
        "k" | "M" if exponent.is_some() => Err(error(
            Code::InvalidNumericSuffix,
            "a quantity cannot have an exponent",
        )),
        "k" => quantity(&whole, fraction.as_deref(), 1_000),
        "M" => quantity(&whole, fraction.as_deref(), 1_000_000),
        "ms" | "s" | "m" | "h" if is_float => Err(error(
            Code::NumberNotExact,
            "a duration is a whole number of its unit, such as `30s`",
        )),
        "ms" => duration(&whole, 1),
        "s" => duration(&whole, 1_000),
        "m" => duration(&whole, 60_000),
        "h" => duration(&whole, 3_600_000),
        other => Err(bad_suffix(other)),
    };
    (end, result)
}

fn float(
    whole: &str,
    fraction: Option<&str>,
    exponent: Option<&str>,
) -> Result<TokenKind, NumError> {
    let mut text = whole.to_owned();
    if let Some(f) = fraction {
        text.push('.');
        text.push_str(f);
    }
    if let Some(e) = exponent {
        text.push('e');
        text.push_str(e);
    }
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() => Ok(TokenKind::Float(value)),
        _ => Err(error(
            Code::NumberOutOfRange,
            "the float literal is outside the range of a 64-bit float",
        )),
    }
}

/// `1.5M` is 1,500,000; the scaled value must be a whole number.
fn quantity(whole: &str, fraction: Option<&str>, scale: u128) -> Result<TokenKind, NumError> {
    let fraction = fraction.unwrap_or("");
    let digits = format!("{whole}{fraction}");
    let places = u32::try_from(fraction.len()).unwrap_or(u32::MAX);
    let scaled = digits
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(scale));
    let divisor = 10u128.checked_pow(places);
    match (scaled, divisor) {
        (Some(total), Some(divisor)) => {
            let value = total / divisor;
            if value * divisor != total {
                return Err(error(
                    Code::NumberNotExact,
                    "a quantity must scale to a whole number",
                ));
            }
            i64::try_from(value)
                .map(TokenKind::Quantity)
                .map_err(|_| out_of_range("the quantity"))
        }
        _ => Err(out_of_range("the quantity")),
    }
}

fn duration(whole: &str, millis_per_unit: u128) -> Result<TokenKind, NumError> {
    whole
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(millis_per_unit))
        .and_then(|v| i64::try_from(v).ok())
        .map(TokenKind::Duration)
        .ok_or_else(|| out_of_range("the duration"))
}
