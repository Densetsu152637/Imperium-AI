//! Refusing programs that name `eval` or `exec` (plan A2, stage 1a finding).
//!
//! Both exist inside Monty and run code built at run time, which defeats any reading of what a
//! program calls. They add no host capability (every host effect still passes through the driver
//! and dispatch), so this is hygiene, not a boundary: `getattr` reaches the same functions. The
//! scan is conservative. It skips comments and the text of plain string literals, but reads the
//! expressions inside f-strings (and t-strings) as code, and it refuses any name with a character
//! outside ASCII, since Python folds names with NFKC and a full-width or styled spelling would
//! otherwise reach the same builtin.

/// Names a program may not use.
const REFUSED: [&str; 2] = ["eval", "exec"];

/// `Err` with the reason when `code` names `eval` or `exec` outside a string literal, or uses a
/// name with a character outside ASCII.
pub fn dynamic_code(code: &str) -> Result<(), String> {
    let chars: Vec<char> = code.chars().collect();
    let scanner = Scanner { chars: &chars };
    scanner.code(0, false).map(|_| ())
}

struct Scanner<'a> {
    chars: &'a [char],
}

impl Scanner<'_> {
    fn at(&self, i: usize) -> Option<char> {
        self.chars.get(i).copied()
    }

    /// Scans code from `i`. Inside an f-string replacement field (`in_field`), stops at the `}`
    /// that closes it and returns its index; otherwise runs to the end.
    fn code(&self, mut i: usize, in_field: bool) -> Result<usize, String> {
        let mut depth = 0_usize;
        while let Some(c) = self.at(i) {
            match c {
                '#' => {
                    while self.at(i).is_some_and(|c| c != '\n') {
                        i += 1;
                    }
                }
                '\'' | '"' => i = self.string(i, "")?,
                '{' | '(' | '[' => {
                    depth += 1;
                    i += 1;
                }
                '}' if in_field && depth == 0 => return Ok(i),
                '}' | ')' | ']' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                }
                c if c == '_' || c.is_alphanumeric() => {
                    let start = i;
                    while self.at(i).is_some_and(|c| c == '_' || c.is_alphanumeric()) {
                        i += 1;
                    }
                    let name: String = self.chars[start..i].iter().collect();
                    // A string prefix (`f"..."`, `rb'...'`) directly before a quote.
                    if matches!(self.at(i), Some('\'' | '"')) && is_string_prefix(&name) {
                        i = self.string(i, &name)?;
                        continue;
                    }
                    if !name.is_ascii() {
                        return Err(format!(
                            "programs may only use ASCII names; {name:?} is not one"
                        ));
                    }
                    if REFUSED.contains(&name.as_str()) {
                        return Err(format!(
                            "{name}() is not available in programs: call your tools directly"
                        ));
                    }
                }
                _ => i += 1,
            }
        }
        Ok(i)
    }

    /// Skips a string literal whose opening quote is at `i`, returning the index after it. The
    /// replacement fields of an f-string or t-string are scanned as code.
    fn string(&self, i: usize, prefix: &str) -> Result<usize, String> {
        let quote = self.chars[i];
        let triple = self.at(i + 1) == Some(quote) && self.at(i + 2) == Some(quote);
        let formatted = prefix
            .chars()
            .any(|c| matches!(c.to_ascii_lowercase(), 'f' | 't'));
        let mut i = i + if triple { 3 } else { 1 };
        while let Some(c) = self.at(i) {
            match c {
                // An escaped character never ends the literal (raw strings included).
                '\\' => i += 2,
                c if c == quote => {
                    if !triple {
                        return Ok(i + 1);
                    }
                    if self.at(i + 1) == Some(quote) && self.at(i + 2) == Some(quote) {
                        return Ok(i + 3);
                    }
                    i += 1;
                }
                // A one-line literal cannot hold a newline: what follows is code again.
                '\n' if !triple => return Ok(i),
                '{' if formatted => {
                    if self.at(i + 1) == Some('{') {
                        i += 2;
                    } else {
                        i = self.code(i + 1, true)? + 1;
                    }
                }
                _ => i += 1,
            }
        }
        Ok(i)
    }
}

fn is_string_prefix(name: &str) -> bool {
    name.len() <= 2
        && name
            .chars()
            .all(|c| matches!(c.to_ascii_lowercase(), 'r' | 'u' | 'b' | 'f' | 't'))
}

#[cfg(test)]
mod tests {
    use super::dynamic_code;

    #[test]
    fn eval_and_exec_are_refused_where_they_are_code() {
        for code in [
            "print(eval('1+1'))",
            "exec('print(2)')",
            "f = eval\nprint(f('1'))",
            "x = 1\n  # comment\nexec ('x')",
            "print(f\"{eval('1')}\")",
            "print(f'''{ exec(\"x\") }''')",
            "print(rf\"{eval('1')}\")",
            "builtins.eval",
            "s = 'one line\nexec(x)",
        ] {
            assert!(dynamic_code(code).is_err(), "{code:?} was accepted");
        }
    }

    #[test]
    fn the_names_in_strings_comments_and_longer_names_are_fine() {
        for code in [
            "print('eval and exec')",
            "print(\"exec\")",
            "# eval(x)\nprint(1)",
            "evaluate = 1\nexecute = 2\nprint(evaluate + execute)",
            "print(f'eval is {1 + 1}')",
            "print(f'{{eval}}')",
            "print('''\neval(x)\n''')",
            "print(r'\\'eval')",
            "d = {'a': 1}\nprint(f\"{d['a']}\")",
            "print('café')",
        ] {
            assert_eq!(dynamic_code(code), Ok(()), "{code:?} was refused");
        }
    }

    #[test]
    fn names_outside_ascii_are_refused() {
        // Full-width letters fold to `eval` under NFKC.
        assert!(dynamic_code("\u{ff45}\u{ff56}\u{ff41}\u{ff4c}('1')").is_err());
        assert!(dynamic_code("caf\u{e9} = 1").is_err());
    }
}
