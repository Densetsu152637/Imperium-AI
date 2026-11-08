//! llama.cpp's server extras beside the OpenAI-compatible API: counting tokens with the loaded
//! model's own tokenizer (`POST /tokenize`) and reading the context each request gets
//! (`GET /props`). Both live at the server's root, not under `/v1`.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::{Endpoint, ModelError};

/// How long one tokenizer or properties request may take.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The server's root URL: the endpoint's base URL without its trailing `/v1`.
pub fn server_root(base_url: &str) -> &str {
    let base = base_url.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base)
}

/// The number of tokens the model reads for `text`, counted by the server's tokenizer. Special
/// tokens written in the text are parsed as the chat template's output is, and no BOS is added,
/// so the counts of a request's parts add up the way the server counts the whole prompt.
pub async fn count_tokens(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    text: &str,
) -> Result<u64, ModelError> {
    #[derive(Deserialize)]
    struct Tokens {
        tokens: Vec<Value>,
    }
    let url = format!("{}/tokenize", server_root(&endpoint.base_url));
    let body = json!({"content": text, "add_special": false, "parse_special": true});
    let response = send(authorize(http.post(url).json(&body), endpoint)).await?;
    let tokens: Tokens = response
        .json()
        .await
        .map_err(|error| ModelError::Decode(format!("tokenize response: {error}")))?;
    Ok(tokens.tokens.len() as u64)
}

/// The number of tokens the server's chat template renders for `messages` and `tools` (in the
/// OpenAI wire shapes), from `POST /apply-template` and the tokenizer. Templates rewrite tool
/// definitions (their own instructions, differently spaced JSON), so this is what the model
/// reads for them rather than the JSON sent.
pub async fn template_tokens(
    http: &reqwest::Client,
    endpoint: &Endpoint,
    messages: &Value,
    tools: &[Value],
) -> Result<u64, ModelError> {
    #[derive(Deserialize)]
    struct Rendered {
        prompt: String,
    }
    let url = format!("{}/apply-template", server_root(&endpoint.base_url));
    let mut body = json!({ "messages": messages });
    if !tools.is_empty() {
        body["tools"] = Value::from(tools.to_vec());
    }
    let response = send(authorize(http.post(url).json(&body), endpoint)).await?;
    let rendered: Rendered = response
        .json()
        .await
        .map_err(|error| ModelError::Decode(format!("apply-template response: {error}")))?;
    count_tokens(http, endpoint, &rendered.prompt).await
}

/// The context window one request gets (the per-slot context), if the server reports it.
/// Versions of llama.cpp report it as `default_generation_settings.n_ctx`, as a top-level
/// `n_ctx`, or under `default_generation_settings.params`.
pub async fn context_window(
    http: &reqwest::Client,
    endpoint: &Endpoint,
) -> Result<Option<u64>, ModelError> {
    let url = format!("{}/props", server_root(&endpoint.base_url));
    let response = send(authorize(http.get(url), endpoint)).await?;
    let props: Value = response
        .json()
        .await
        .map_err(|error| ModelError::Decode(format!("props response: {error}")))?;
    Ok(window_from_props(&props))
}

fn window_from_props(props: &Value) -> Option<u64> {
    let settings = &props["default_generation_settings"];
    [
        &settings["n_ctx"],
        &props["n_ctx"],
        &settings["params"]["n_ctx"],
    ]
    .into_iter()
    .find_map(Value::as_u64)
    .filter(|window| *window > 0)
}

fn authorize(builder: reqwest::RequestBuilder, endpoint: &Endpoint) -> reqwest::RequestBuilder {
    let builder = builder.timeout(TIMEOUT);
    match &endpoint.api_key {
        Some(key) => builder.bearer_auth(key),
        None => builder,
    }
}

async fn send(builder: reqwest::RequestBuilder) -> Result<reqwest::Response, ModelError> {
    let response = builder.send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ModelError::Status {
            status: status.as_u16(),
            body: body.chars().take(500).collect(),
            retry_after_s: None,
        });
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{Json, Router};

    /// A llama.cpp stand-in whose tokenizer splits on whitespace.
    async fn fake_server(props: Value) -> Endpoint {
        let app = Router::new()
            .route(
                "/tokenize",
                post(|Json(body): Json<Value>| async move {
                    let text = body["content"].as_str().unwrap_or_default();
                    let tokens: Vec<usize> = (0..text.split_whitespace().count()).collect();
                    Json(json!({"tokens": tokens}))
                }),
            )
            .route("/props", get(move || async move { Json(props) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Endpoint {
            base_url,
            api_key: None,
        }
    }

    #[test]
    fn the_server_root_drops_the_api_prefix() {
        assert_eq!(
            server_root("http://127.0.0.1:8080/v1"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            server_root("http://127.0.0.1:8080/v1/"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(server_root("http://host:1/"), "http://host:1");
    }

    #[tokio::test]
    async fn tokens_are_counted_by_the_server() {
        let endpoint = fake_server(json!({})).await;
        let http = reqwest::Client::new();
        let count = count_tokens(&http, &endpoint, "one two  three\nfour")
            .await
            .unwrap();
        assert_eq!(count, 4);
    }

    #[tokio::test]
    async fn the_context_window_is_read_from_either_props_shape() {
        let http = reqwest::Client::new();
        let nested = fake_server(json!({"default_generation_settings": {"n_ctx": 8192}})).await;
        assert_eq!(context_window(&http, &nested).await.unwrap(), Some(8192));
        let top = fake_server(json!({"n_ctx": 4096, "default_generation_settings": {}})).await;
        assert_eq!(context_window(&http, &top).await.unwrap(), Some(4096));
        let params =
            fake_server(json!({"default_generation_settings": {"params": {"n_ctx": 2048}}})).await;
        assert_eq!(context_window(&http, &params).await.unwrap(), Some(2048));
        let none = fake_server(json!({"model_path": "m.gguf"})).await;
        assert_eq!(context_window(&http, &none).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_server_without_a_tokenizer_returns_its_status() {
        let app = Router::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        let endpoint = Endpoint {
            base_url,
            api_key: None,
        };
        let error = count_tokens(&reqwest::Client::new(), &endpoint, "x")
            .await
            .unwrap_err();
        assert!(
            matches!(error, ModelError::Status { status: 404, .. }),
            "{error}"
        );
    }
}
