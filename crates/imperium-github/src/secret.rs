//! The GitHub token: a type that cannot be printed, the sources that produce it, and the scrubbing
//! applied to every error text that leaves this crate.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// A secret string. It has no `Debug` or `Display`, so it cannot reach a log, an error or a
/// prompt by formatting; the only way to read it is [`Secret::expose`].
///
/// ```compile_fail
/// fn needs_debug<T: std::fmt::Debug>() {}
/// needs_debug::<imperium_github::Secret>();
/// ```
///
/// ```compile_fail
/// fn needs_display<T: std::fmt::Display>() {}
/// needs_display::<imperium_github::Secret>();
/// ```
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret text. Use it only to build the `Authorization` header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<Secret, String>> + Send + 'a>>;

/// Where the daemon gets the GitHub token. Asked again after a 401, so a source that can rotate
/// (the future keychain source) is picked up.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> TokenFuture<'_>;
}

/// `gh auth token` for `github.com`, the signed-in `gh` account (decision D1).
#[derive(Debug, Default)]
pub struct GhAuthToken;

impl TokenSource for GhAuthToken {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async {
            let output = tokio::process::Command::new("gh")
                .args(["auth", "token", "--hostname", "github.com"])
                .env_remove("GH_HOST")
                .env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .output()
                .await
                .map_err(|error| format!("could not run gh: {error}"))?;
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !output.status.success() || text.is_empty() {
                // `gh`'s stderr is not echoed: nothing it prints is needed to say "sign in".
                return Err("gh has no GitHub token; run `gh auth login`".to_string());
            }
            Ok(Secret::new(text))
        })
    }
}

/// A fixed token, for tests and for callers that already hold one.
pub struct StaticToken(Mutex<Secret>);

impl StaticToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(Mutex::new(Secret::new(token)))
    }

    /// Replaces the token, as a rotation would.
    pub fn replace(&self, token: impl Into<String>) {
        *self.0.lock().expect("token lock") = Secret::new(token);
    }
}

impl TokenSource for StaticToken {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move { Ok(self.0.lock().expect("token lock").clone()) })
    }
}

const TOKEN_PREFIXES: [&str; 6] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"];

/// Removes the token, anything shaped like a GitHub token, and `Authorization` values from
/// `text`, which may be GitHub's error text or a transport error.
pub fn scrub(text: &str, secret: Option<&Secret>) -> String {
    let mut out = text.to_string();
    if let Some(secret) = secret.filter(|s| !s.expose().is_empty()) {
        out = out.replace(secret.expose(), "[token]");
    }
    let mut result = String::with_capacity(out.len());
    let mut redact_next = false;
    for word in out.split_inclusive(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',')) {
        let trimmed =
            word.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ','));
        let tail = &word[trimmed.len()..];
        let lowered = trimmed.to_ascii_lowercase();
        let looks_secret = TOKEN_PREFIXES.iter().any(|p| trimmed.starts_with(p));
        if redact_next || looks_secret {
            result.push_str("[token]");
            result.push_str(tail);
            redact_next = false;
            continue;
        }
        // `Authorization: Bearer x` or `Bearer x`: drop the credential that follows the scheme.
        redact_next = matches!(lowered.as_str(), "bearer" | "token" | "basic");
        result.push_str(word);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_removes_the_token_and_token_shaped_words() {
        let secret = Secret::new("sentinel-123");
        let text = "request with sentinel-123 failed; Authorization: Bearer abcdef; ghp_AAAA";
        let clean = scrub(text, Some(&secret));
        assert!(!clean.contains("sentinel-123"), "{clean}");
        assert!(!clean.contains("abcdef"), "{clean}");
        assert!(!clean.contains("ghp_AAAA"), "{clean}");
        assert!(clean.contains("failed"), "{clean}");
    }
}
