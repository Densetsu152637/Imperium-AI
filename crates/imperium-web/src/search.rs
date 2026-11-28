//! Search providers behind one trait, and the registry that builds them from `web.search`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::retry::{RetryPolicy, send_retrying};
use crate::{Hit, WebError, html};

/// The most of a provider's answer that is read.
const SEARCH_BODY_BYTES: usize = 4 * 1024 * 1024;

pub type SearchFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Hit>, WebError>> + Send + 'a>>;

/// Where `web_search` sends queries; [`Searxng`], [`Brave`], [`Tavily`], [`Kagi`], and [`Exa`]
/// are built in.
pub trait SearchProvider: Send + Sync + fmt::Debug {
    /// Results for `query`, asking for `count` (1 to 20). Extra results are dropped by the caller.
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        count: usize,
    ) -> SearchFuture<'a>;
}

/// What providers may use from `[web]`.
pub struct SearchConfig<'a> {
    /// `web.searxng_url`.
    pub searxng_url: &'a str,
    /// Loads the key for the named provider (its own `web.<name>_key`, else `web.api_key`);
    /// called only by providers that need a key.
    pub api_key: &'a dyn Fn(&str) -> Option<String>,
}

/// Builds a provider from `[web]`, or `None` when its settings are incomplete.
pub type SearchProviderBuilder =
    Box<dyn Fn(&SearchConfig<'_>) -> Option<Arc<dyn SearchProvider>> + Send + Sync>;

/// Provider builders by `web.search` name. The default has `searxng`, `brave`, `tavily`, `kagi`, and `exa`.
pub struct SearchProviders {
    builders: Vec<(String, SearchProviderBuilder)>,
}

impl Default for SearchProviders {
    fn default() -> Self {
        let mut providers = SearchProviders {
            builders: Vec::new(),
        };
        providers.register("searxng", |config| {
            (!config.searxng_url.is_empty()).then(|| {
                Arc::new(Searxng {
                    url: config.searxng_url.to_string(),
                }) as Arc<dyn SearchProvider>
            })
        });
        providers.register("brave", |config| {
            (config.api_key)("brave")
                .map(|api_key| Arc::new(Brave { api_key }) as Arc<dyn SearchProvider>)
        });
        providers.register("tavily", |config| {
            (config.api_key)("tavily")
                .map(|api_key| Arc::new(Tavily { api_key }) as Arc<dyn SearchProvider>)
        });
        providers.register("kagi", |config| {
            (config.api_key)("kagi")
                .map(|api_key| Arc::new(Kagi::new(api_key)) as Arc<dyn SearchProvider>)
        });
        providers.register("exa", |config| {
            (config.api_key)("exa")
                .map(|api_key| Arc::new(Exa::new(api_key)) as Arc<dyn SearchProvider>)
        });
        providers
    }
}

impl SearchProviders {
    /// Adds a provider under `name`, replacing any provider already called that.
    pub fn register(
        &mut self,
        name: &str,
        builder: impl Fn(&SearchConfig<'_>) -> Option<Arc<dyn SearchProvider>> + Send + Sync + 'static,
    ) {
        self.builders.retain(|(existing, _)| existing != name);
        self.builders.push((name.to_string(), Box::new(builder)));
    }

    /// The provider called `name`, or `None` when the name is unknown (such as `none`) or the
    /// provider's settings are incomplete.
    pub fn build(&self, name: &str, config: &SearchConfig<'_>) -> Option<Arc<dyn SearchProvider>> {
        self.builders
            .iter()
            .find(|(existing, _)| existing == name)
            .and_then(|(_, builder)| builder(config))
    }
}

/// A SearXNG instance with the JSON format enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Searxng {
    pub url: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Brave {
    pub api_key: String,
}

// The key stays out of logs and error messages.
impl fmt::Debug for Brave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Brave")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Tavily {
    pub api_key: String,
}

// The key stays out of logs and error messages.
impl fmt::Debug for Tavily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tavily")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

const KAGI_URL: &str = "https://kagi.com/api/v0";
const EXA_URL: &str = "https://api.exa.ai";

/// Kagi's Search API (an API token, billed per query).
#[derive(Clone, PartialEq, Eq)]
pub struct Kagi {
    pub api_key: String,
    /// The API root; only tests change it.
    pub base_url: String,
}

impl Kagi {
    pub fn new(api_key: String) -> Self {
        Kagi {
            api_key,
            base_url: KAGI_URL.into(),
        }
    }
}

// The key stays out of logs and error messages.
impl fmt::Debug for Kagi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Kagi")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Exa's search API, which returns each result with a text excerpt.
#[derive(Clone, PartialEq, Eq)]
pub struct Exa {
    pub api_key: String,
    /// The API root; only tests change it.
    pub base_url: String,
}

impl Exa {
    pub fn new(api_key: String) -> Self {
        Exa {
            api_key,
            base_url: EXA_URL.into(),
        }
    }
}

// The key stays out of logs and error messages.
impl fmt::Debug for Exa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Exa")
            .field("api_key", &"<redacted>")
            .finish()
    }
}

fn http_error(error: reqwest::Error) -> WebError {
    WebError::Http(error.to_string())
}

/// The JSON a provider answers with. A rate limit or server error is asked again politely
/// (see [`crate::retry`]); an answer that is bot protection or an access rule rather than JSON
/// is [`WebError::Blocked`], and nothing of its text is returned. Errors name the provider's
/// host, never the request's query string or headers (the key is in a header).
async fn json_body(request: reqwest::RequestBuilder) -> Result<Value, WebError> {
    let response = send_retrying(request, &RetryPolicy::default())
        .await
        .map_err(|error| http_error(error.without_url()))?;
    let status = response.status();
    let host = response
        .url()
        .host_str()
        .unwrap_or("the provider")
        .to_string();
    let mitigated = crate::cf_mitigated(response.headers());
    let wait = crate::retry::retry_after(response.headers());
    let body = crate::read_capped(response, SEARCH_BODY_BYTES).await;
    if status.is_success() {
        if let Ok(json) = serde_json::from_str(&body) {
            return Ok(json);
        }
        // Not JSON: usually a challenge page standing in for the answer.
        let title = html::title(&body);
        let text = html::to_text(&body);
        return Err(
            match crate::block::detect(status.as_u16(), mitigated, &body, title.as_deref(), &text) {
                Some(reason) => blocked(host, reason, wait),
                None => WebError::Http(format!("{host} did not answer with JSON")),
            },
        );
    }
    if matches!(status.as_u16(), 401 | 402) {
        return Err(WebError::Http(format!(
            "HTTP {} from {host}: the provider did not accept the key or plan",
            status.as_u16()
        )));
    }
    let title = html::title(&body);
    let text = html::to_text(&body);
    Err(
        match crate::block::detect(status.as_u16(), mitigated, &body, title.as_deref(), &text) {
            Some(reason) => blocked(host, reason, wait),
            None => WebError::Http(format!("HTTP {} from {host}", status.as_u16())),
        },
    )
}

fn blocked(host: String, reason: String, wait: Option<std::time::Duration>) -> WebError {
    WebError::Blocked {
        url: host,
        reason: crate::with_wait(reason, wait),
        tried: Vec::new(),
    }
}

impl SearchProvider for Searxng {
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        _count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async move {
            let request = http
                .get(format!("{}/search", self.url.trim_end_matches('/')))
                .query(&[("q", query), ("format", "json")]);
            Ok(parse_hits(json_body(request).await?.get("results")))
        })
    }
}

impl SearchProvider for Brave {
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async move {
            let request = http
                .get("https://api.search.brave.com/res/v1/web/search")
                .query(&[("q", query), ("count", &count.to_string())])
                .header("X-Subscription-Token", &self.api_key)
                .header("Accept", "application/json");
            Ok(parse_hits(
                json_body(request).await?.pointer("/web/results"),
            ))
        })
    }
}

impl SearchProvider for Tavily {
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async move {
            let request = http
                .post("https://api.tavily.com/search")
                .bearer_auth(&self.api_key)
                .json(&json!({"query": query, "max_results": count}));
            Ok(parse_hits(json_body(request).await?.get("results")))
        })
    }
}

impl SearchProvider for Kagi {
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async move {
            let request = http
                .get(format!("{}/search", self.base_url.trim_end_matches('/')))
                .query(&[("q", query), ("limit", &count.to_string())])
                .header("Authorization", format!("Bot {}", self.api_key));
            let body = json_body(request).await?;
            // `t` is 0 for a search result and 1 for related searches, which are not results.
            let results: Vec<Value> = body
                .get("data")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|item| item.get("t").and_then(Value::as_i64) == Some(0))
                .cloned()
                .collect();
            Ok(parse_hits(Some(&Value::Array(results))))
        })
    }
}

impl SearchProvider for Exa {
    fn search<'a>(
        &'a self,
        http: &'a reqwest::Client,
        query: &'a str,
        count: usize,
    ) -> SearchFuture<'a> {
        Box::pin(async move {
            let request = http
                .post(format!("{}/search", self.base_url.trim_end_matches('/')))
                .header("x-api-key", &self.api_key)
                .json(&json!({
                    "query": query,
                    "numResults": count,
                    "contents": {"text": {"maxCharacters": 400}}
                }));
            Ok(parse_hits(json_body(request).await?.get("results")))
        })
    }
}

/// Results from a provider's JSON result list; entries without a URL are skipped.
pub(crate) fn parse_hits(list: Option<&Value>) -> Vec<Hit> {
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        title: String,
        #[serde(default)]
        url: String,
        #[serde(default, alias = "content", alias = "description", alias = "text")]
        snippet: String,
    }
    list.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| serde_json::from_value::<Raw>(item.clone()).ok())
        .filter(|raw| !raw.url.is_empty())
        .map(|raw| Hit {
            title: html::to_text(&raw.title),
            url: raw.url,
            snippet: html::to_text(&raw.snippet),
        })
        .collect()
}
