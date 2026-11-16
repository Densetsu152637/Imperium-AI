use std::fmt;

const TOKEN_BYTES: usize = 32;

/// The bearer token clients must present. A new one is generated at every daemon launch.
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    /// 32 bytes from the OS random source, hex-encoded.
    pub fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(AccessToken(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Checks an `Authorization` header value (`Bearer <token>`) in constant time.
    pub fn accepts_header(&self, header: &str) -> bool {
        header.strip_prefix("Bearer ").is_some_and(|presented| {
            constant_time_eq::constant_time_eq(presented.as_bytes(), self.0.as_bytes())
        })
    }
}

impl From<String> for AccessToken {
    fn from(token: String) -> Self {
        AccessToken(token)
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_unique_hex() {
        let a = AccessToken::generate().unwrap();
        let b = AccessToken::generate().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), TOKEN_BYTES * 2);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn only_the_exact_bearer_token_is_accepted() {
        let token = AccessToken::from("abc123".to_string());
        assert!(token.accepts_header("Bearer abc123"));
        assert!(!token.accepts_header("Bearer abc124"));
        assert!(!token.accepts_header("abc123"));
        assert!(!token.accepts_header("Bearer "));
        assert!(!format!("{token:?}").contains("abc123"));
    }
}
