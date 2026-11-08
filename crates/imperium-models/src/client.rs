//! Model clients behind one trait, so the agent loop streams completions without knowing which
//! API an endpoint speaks. Clients are built by an endpoint's `kind` from [`ChatClients`]; the
//! built-in kinds speak the OpenAI-compatible Chat Completions API, and a client for another API
//! (such as Anthropic Messages or OpenAI Responses) is registered under a new kind.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::chat::{ChatRequest, Completion, Endpoint, ModelError, stream_chat};
use crate::llamacpp;

/// A streamed completion: its text, tool calls, and the usage the provider reported.
pub type ChatFuture<'a> = Pin<Box<dyn Future<Output = Result<Completion, ModelError>> + Send + 'a>>;
/// An answer from a model's server about its tokenizer or context.
pub type TokenFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModelError>> + Send + 'a>>;

/// Calls one model endpoint.
pub trait ChatClient: Send + Sync + fmt::Debug {
    /// Streams a completion, calling `on_text` with each text fragment as it arrives.
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a>;

    /// The model's own tokenizer, when its server offers one; context breakdowns then count
    /// exactly instead of estimating.
    fn tokenizer(&self) -> Option<&dyn Tokenizer> {
        None
    }
}

/// A model server's tokenizer and what it reports about itself.
pub trait Tokenizer: Send + Sync {
    /// Identifies the server, so counts, refusals, and its context window are kept per server.
    fn server(&self) -> &str;

    /// The tokens the model reads for `text`, without BOS and with special tokens parsed as
    /// the chat template's output is.
    fn count_tokens<'a>(&'a self, text: &'a str) -> TokenFuture<'a, u64>;

    /// The tokens the chat template renders for `messages` and `tools` (OpenAI wire shapes).
    fn template_tokens<'a>(
        &'a self,
        messages: &'a Value,
        tools: &'a [Value],
    ) -> TokenFuture<'a, u64>;

    /// The context window one request gets, if the server reports it.
    fn context_window(&self) -> TokenFuture<'_, Option<u64>>;
}

/// An OpenAI-compatible Chat Completions endpoint (hosted providers, vLLM, Ollama, LM Studio).
#[derive(Debug, Clone)]
pub struct OpenAiCompatible {
    http: reqwest::Client,
    endpoint: Endpoint,
}

impl OpenAiCompatible {
    pub fn new(http: reqwest::Client, endpoint: Endpoint) -> Self {
        OpenAiCompatible { http, endpoint }
    }
}

impl ChatClient for OpenAiCompatible {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        Box::pin(stream_chat(&self.http, &self.endpoint, request, on_text))
    }
}

/// A llama.cpp server: the OpenAI-compatible API plus its tokenizer (`/tokenize`,
/// `/apply-template`) and properties (`/props`).
#[derive(Debug, Clone)]
pub struct LlamaCpp {
    chat: OpenAiCompatible,
    root: String,
}

impl LlamaCpp {
    pub fn new(http: reqwest::Client, endpoint: Endpoint) -> Self {
        let root = llamacpp::server_root(&endpoint.base_url).to_string();
        LlamaCpp {
            chat: OpenAiCompatible::new(http, endpoint),
            root,
        }
    }
}

impl ChatClient for LlamaCpp {
    fn stream<'a>(
        &'a self,
        request: ChatRequest<'a>,
        on_text: &'a mut (dyn FnMut(&str) + Send),
    ) -> ChatFuture<'a> {
        self.chat.stream(request, on_text)
    }

    fn tokenizer(&self) -> Option<&dyn Tokenizer> {
        Some(self)
    }
}

impl Tokenizer for LlamaCpp {
    fn server(&self) -> &str {
        &self.root
    }

    fn count_tokens<'a>(&'a self, text: &'a str) -> TokenFuture<'a, u64> {
        Box::pin(llamacpp::count_tokens(
            &self.chat.http,
            &self.chat.endpoint,
            text,
        ))
    }

    fn template_tokens<'a>(
        &'a self,
        messages: &'a Value,
        tools: &'a [Value],
    ) -> TokenFuture<'a, u64> {
        Box::pin(llamacpp::template_tokens(
            &self.chat.http,
            &self.chat.endpoint,
            messages,
            tools,
        ))
    }

    fn context_window(&self) -> TokenFuture<'_, Option<u64>> {
        Box::pin(llamacpp::context_window(
            &self.chat.http,
            &self.chat.endpoint,
        ))
    }
}

/// Builds a client for an endpoint, sharing the session's HTTP client.
pub type ChatClientBuilder =
    Box<dyn Fn(&reqwest::Client, Endpoint) -> Arc<dyn ChatClient> + Send + Sync>;

/// Client builders by endpoint `kind`. The default has:
///
/// - `openai`: an OpenAI-compatible endpoint. One on this machine may be llama.cpp, so it is
///   built as [`LlamaCpp`]; a server that refuses the tokenizer is not asked again.
/// - `llamacpp`: a llama.cpp server, always asked for its tokenizer.
pub struct ChatClients {
    builders: Vec<(String, ChatClientBuilder)>,
}

/// The kind endpoints have unless configured otherwise.
pub const DEFAULT_KIND: &str = "openai";

impl Default for ChatClients {
    fn default() -> Self {
        let mut clients = ChatClients {
            builders: Vec::new(),
        };
        clients.register(DEFAULT_KIND, |http, endpoint| {
            if is_loopback(&endpoint.base_url) {
                Arc::new(LlamaCpp::new(http.clone(), endpoint))
            } else {
                Arc::new(OpenAiCompatible::new(http.clone(), endpoint))
            }
        });
        clients.register("llamacpp", |http, endpoint| {
            Arc::new(LlamaCpp::new(http.clone(), endpoint))
        });
        clients
    }
}

impl ChatClients {
    /// Adds a client under `kind`, replacing any client already registered for it.
    pub fn register(
        &mut self,
        kind: &str,
        builder: impl Fn(&reqwest::Client, Endpoint) -> Arc<dyn ChatClient> + Send + Sync + 'static,
    ) {
        self.builders.retain(|(existing, _)| existing != kind);
        self.builders.push((kind.to_string(), Box::new(builder)));
    }

    /// A client for `endpoint`, or `None` when no client is registered for `kind`.
    pub fn build(
        &self,
        kind: &str,
        http: &reqwest::Client,
        endpoint: Endpoint,
    ) -> Option<Arc<dyn ChatClient>> {
        self.builders
            .iter()
            .find(|(existing, _)| existing == kind)
            .map(|(_, builder)| builder(http, endpoint))
    }

    /// The registered kinds, in registration order.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.builders.iter().map(|(kind, _)| kind.as_str())
    }
}

impl fmt::Debug for ChatClients {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.kinds()).finish()
    }
}

/// Whether `base_url` points at this machine, where a llama.cpp server may answer `/tokenize`.
fn is_loopback(base_url: &str) -> bool {
    let after_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = after_scheme.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(base_url: &str) -> Endpoint {
        Endpoint {
            base_url: base_url.into(),
            api_key: None,
        }
    }

    #[test]
    fn only_this_machine_counts_as_loopback() {
        assert!(is_loopback("http://127.0.0.1:8080/v1"));
        assert!(is_loopback("http://localhost:1234/v1"));
        assert!(is_loopback("http://[::1]:8080/v1"));
        assert!(!is_loopback("https://openrouter.ai/api/v1"));
        assert!(!is_loopback("http://192.168.1.4:8080/v1"));
    }

    #[test]
    fn clients_offer_a_tokenizer_only_where_llama_cpp_may_answer() {
        let clients = ChatClients::default();
        let http = reqwest::Client::new();
        let build = |kind: &str, url: &str| clients.build(kind, &http, endpoint(url)).unwrap();
        assert!(
            build("openai", "https://openrouter.ai/api/v1")
                .tokenizer()
                .is_none()
        );
        let local = build("openai", "http://127.0.0.1:8080/v1");
        assert_eq!(local.tokenizer().unwrap().server(), "http://127.0.0.1:8080");
        assert!(
            build("llamacpp", "http://gpu-box:8080/v1")
                .tokenizer()
                .is_some()
        );
        assert!(
            clients
                .build("anthropic", &http, endpoint("https://x"))
                .is_none()
        );
        assert_eq!(format!("{clients:?}"), r#"["openai", "llamacpp"]"#);
    }
}
