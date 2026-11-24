//! Where the daemon gets the GitHub token: the `gh` login first, then `GH_TOKEN` or
//! `GITHUB_TOKEN` from the daemon's own environment for users whose only credential is one of
//! those. The client crate reads no environment variable (so nothing can point it at another
//! host or token); this source lives here, in the daemon.

use std::sync::Arc;

use imperium_github::{
    DaemonGitHub, GhAuthToken, GhCli, HttpGitHub, Secret, TokenFuture, TokenSource,
};

/// Reads a variable by name.
type Lookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A token held in an environment variable of the daemon itself. The first of `names` that is
/// set and not blank wins; the value is never printed.
pub struct EnvToken {
    names: Vec<&'static str>,
    lookup: Lookup,
}

impl EnvToken {
    /// `GH_TOKEN`, then `GITHUB_TOKEN`, read from the process environment each time.
    pub fn standard() -> Self {
        Self::with_lookup(["GH_TOKEN", "GITHUB_TOKEN"], |name| {
            std::env::var(name).ok()
        })
    }

    /// Reads `names` through `lookup` (for tests, which must not change the process
    /// environment).
    pub fn with_lookup(
        names: impl IntoIterator<Item = &'static str>,
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            names: names.into_iter().collect(),
            lookup: Box::new(lookup),
        }
    }
}

impl TokenSource for EnvToken {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            self.names
                .iter()
                .filter_map(|name| (self.lookup)(name))
                .map(|value| value.trim().to_string())
                .find(|value| !value.is_empty())
                .map(Secret::new)
                .ok_or_else(|| "no GH_TOKEN or GITHUB_TOKEN is set".to_string())
        })
    }
}

/// `primary`, and `fallback` when the primary has no token. A refused token is not retried
/// through the fallback: the fallback only fills in for a missing login.
pub struct GhThenEnv {
    primary: Arc<dyn TokenSource>,
    fallback: Arc<dyn TokenSource>,
}

impl GhThenEnv {
    pub fn new(primary: Arc<dyn TokenSource>, fallback: Arc<dyn TokenSource>) -> Self {
        Self { primary, fallback }
    }

    /// `gh auth token`, then the daemon's `GH_TOKEN` or `GITHUB_TOKEN`.
    pub fn standard() -> Self {
        Self::new(Arc::new(GhAuthToken), Arc::new(EnvToken::standard()))
    }
}

impl TokenSource for GhThenEnv {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            match self.primary.token().await {
                Ok(token) => Ok(token),
                Err(primary) => self
                    .fallback
                    .token()
                    .await
                    // The primary's message is the one that tells the user what to do.
                    .map_err(|_| primary),
            }
        })
    }
}

/// What the daemon uses: pull-request data over `https://api.github.com` with the token from
/// [`GhThenEnv`], and `gh` for failed-check logs.
pub fn daemon_github() -> Result<DaemonGitHub, String> {
    let api = HttpGitHub::api(Arc::new(GhThenEnv::standard()))?;
    Ok(DaemonGitHub::new(Arc::new(api), Arc::new(GhCli)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_github::StaticToken;

    struct Missing;

    impl TokenSource for Missing {
        fn token(&self) -> TokenFuture<'_> {
            Box::pin(async { Err("gh has no GitHub token; run `gh auth login`".to_string()) })
        }
    }

    fn env(pairs: &'static [(&'static str, &'static str)]) -> EnvToken {
        EnvToken::with_lookup(["GH_TOKEN", "GITHUB_TOKEN"], move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_string())
        })
    }

    #[tokio::test]
    async fn gh_token_wins_over_github_token_and_blank_values_are_skipped() {
        let both = env(&[("GH_TOKEN", " first "), ("GITHUB_TOKEN", "second")]);
        assert_eq!(both.token().await.unwrap().expose(), "first");
        let blank = env(&[("GH_TOKEN", "  "), ("GITHUB_TOKEN", "second")]);
        assert_eq!(blank.token().await.unwrap().expose(), "second");
        assert!(env(&[]).token().await.is_err());
    }

    #[tokio::test]
    async fn the_environment_fills_in_only_when_gh_has_no_login() {
        let from_env = GhThenEnv::new(Arc::new(Missing), Arc::new(env(&[("GH_TOKEN", "envtok")])));
        assert_eq!(from_env.token().await.unwrap().expose(), "envtok");
        let gh = GhThenEnv::new(
            Arc::new(StaticToken::new("ghtok")),
            Arc::new(env(&[("GH_TOKEN", "envtok")])),
        );
        assert_eq!(gh.token().await.unwrap().expose(), "ghtok");
        // With neither, the error says how to sign in with `gh`.
        let neither = GhThenEnv::new(Arc::new(Missing), Arc::new(env(&[])));
        let error = neither.token().await.err().unwrap();
        assert!(error.contains("gh auth login"), "{error}");
    }
}
