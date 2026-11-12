//! `POST /v1/chat/completions`: checks the request, takes a slot on the model the way any caller
//! does (loading it, queueing, and reporting through `model.residency`), and relays the model
//! server's answer, rewriting only the model id. Streamed answers are relayed as they come.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use imperium_api::{
    Effort, LocalLease, LocalModels, LocalRequest, Pricing, Priority, Usage, holder,
};
use imperium_engine::ResolvedModel;
use imperium_local::supervisor::Supervisor;
use imperium_models::{DataLines, ReasoningStyle};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::error::ApiError;
use super::{ApiState, models};
use crate::DaemonState;
use crate::budgets::{self, CallUsage, SOURCE_API};

/// Request fields relayed to the model server as the client sent them. Everything else is
/// dropped: the server-specific fields of llama.cpp (such as `id_slot` or `grammar`) are the
/// runtime's to set, not the caller's.
const RELAYED: &[&str] = &[
    "messages",
    "temperature",
    "top_p",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "logit_bias",
    "seed",
    "response_format",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "user",
    "logprobs",
    "top_logprobs",
];

/// Characters of text counted as an inline image or file in a prompt estimate.
const INLINE_DATA_CHARS: f64 = 3000.0;

/// Chunks of a streamed answer held for a slow client before the model server is made to wait.
const STREAM_BACKLOG: usize = 32;

/// Longest the model's server may take to begin its answer (for a call that is not streamed,
/// to finish it), after the call holds its slot.
const UPSTREAM_START_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest a streamed answer may go without a chunk before the call is given up.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Longest the client may leave a streamed chunk unread before the call is given up, so a
/// client that stops reading cannot hold a slot for ever.
const CLIENT_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Tokens of reply a call without `max_tokens` is expected to use when it is admitted to the
/// budgets; what it really uses is recorded when it ends.
const DEFAULT_REPLY_RESERVE: u64 = 1024;

pub(super) async fn chat_completions(State(api): State<Arc<ApiState>>, body: Bytes) -> Response {
    match handle(&api, &body).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn handle(api: &ApiState, body: &[u8]) -> Result<Response, ApiError> {
    let daemon = api.daemon.upgrade().ok_or_else(ApiError::shutting_down)?;
    // Held for the whole call, the stream included: the cap is on work in flight.
    let permit = daemon
        .openai()
        .try_permit(daemon.settings().api.openai_compatible.max_concurrent)
        .ok_or_else(ApiError::too_many_calls)?;
    let request: Value = serde_json::from_slice(body)
        .map_err(|error| ApiError::invalid(format!("The body is not valid JSON: {error}")))?;
    let request = request
        .as_object()
        .ok_or_else(|| ApiError::invalid("The body must be a JSON object."))?;
    let requested = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("`model` is required."))?;
    if !request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| !messages.is_empty())
    {
        return Err(ApiError::invalid(
            "`messages` is required and must not be empty.",
        ));
    }
    if request.get("n").and_then(Value::as_u64).unwrap_or(1) != 1 {
        return Err(ApiError::invalid("Only n = 1 is supported."));
    }
    let served = models::find(&daemon, requested)
        .map_err(|hint| ApiError::model_not_found(requested, &hint))?;
    let resolved = daemon
        .resolve_model(Some(&served.id))
        .map_err(|error| ApiError::unavailable(error.to_string()))?;
    daemon
        .check_selectable(&resolved)
        .map_err(ApiError::unavailable)?;
    let wants_tools = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if wants_tools && !models::supports_tools(&daemon.settings(), &served.id) {
        return Err(ApiError::invalid(format!(
            "The server behind `{}` is not set up for tool calls (vLLM needs \
             --enable-auto-tool-choice in runtime.vllm.extra_args).",
            served.id
        )));
    }
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let wants_usage = request
        .get("stream_options")
        .and_then(|options| options.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let effort = match request.get("reasoning_effort").and_then(Value::as_str) {
        Some(level) => Some(
            level
                .parse::<Effort>()
                .map_err(|error| ApiError::invalid(error.to_string()))?,
        ),
        None => None,
    };
    let client_cap = request
        .get("max_tokens")
        .or_else(|| request.get("max_completion_tokens"))
        .and_then(Value::as_u64);

    let prompt_estimate = estimate_tokens(
        request.get("messages").unwrap_or(&Value::Null),
        request.get("tools").unwrap_or(&Value::Null),
    );
    // Admitted to the budgets for what the call is expected to use, which counts against them
    // for calls that arrive meanwhile: the API is a source of its own, so a budget can cap it,
    // and nobody can answer an "ask" budget here, so it refuses like a "block" one.
    let expected = Usage {
        prompt_tokens: u64::from(prompt_estimate),
        completion_tokens: client_cap.unwrap_or(DEFAULT_REPLY_RESERVE),
        cached_tokens: 0,
    };
    let reservation = budgets::reserve_api_call(
        &daemon,
        &served.id,
        expected.prompt_tokens + expected.completion_tokens,
        resolved
            .pricing
            .map_or(0.0, |pricing| pricing.cost_usd(&expected)),
    )
    .map_err(ApiError::budget)?;
    let (base_url, api_key, lease) = if resolved.local {
        let lease = take_slot(&daemon, api, &resolved, prompt_estimate).await?;
        (lease.base_url.clone(), None, Some(lease))
    } else {
        (
            resolved.endpoint.base_url.clone(),
            resolved.endpoint.api_key.clone(),
            None,
        )
    };
    let mut upstream = relayed_body(request, &resolved.model, streaming);
    if let Some(lease) = &lease {
        // The reservation the runtime made for this call caps its reply; the slot pins the
        // request to the cache of the prompt the holder used last.
        if let Some(cap) = lease.max_tokens {
            let cap = u64::from(cap);
            upstream["max_tokens"] = json!(client_cap.map_or(cap, |asked| asked.min(cap)));
        }
        if let Some(slot) = lease.slot {
            upstream["id_slot"] = json!(slot);
            upstream["cache_prompt"] = json!(true);
        }
    }
    if upstream.get("max_tokens").is_none()
        && let Some(asked) = client_cap
    {
        upstream["max_tokens"] = json!(asked);
    }
    if let Some(effort) = effort
        && daemon.model_reasons(&served.id)
    {
        ReasoningStyle::for_base_url(&base_url).apply(&mut upstream, effort);
    }

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut sending = api.http.post(url).json(&upstream);
    if let Some(key) = &api_key {
        sending = sending.bearer_auth(key);
    }
    // From here the call counts, whether it ends or the client goes away first (see `Call`).
    let call = Call {
        local: resolved
            .local
            .then(|| {
                daemon
                    .local_runtime()
                    .map(|local| Arc::clone(&local.supervisor))
            })
            .flatten(),
        daemon,
        id: served.id,
        model: resolved.model.clone(),
        pricing: resolved.pricing,
        prompt_estimate,
        started: Instant::now(),
        finished: AtomicBool::new(false),
        _reservation: reservation,
        _permit: permit,
    };
    let started = call.started;
    let sent = tokio::time::timeout(UPSTREAM_START_TIMEOUT, sending.send()).await;
    let response = match sent {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            call.abandon();
            return Err(ApiError::unreachable(format!(
                "The model's server did not answer: {}",
                error.without_url()
            )));
        }
        Err(_) => {
            call.abandon();
            return Err(ApiError::unreachable(format!(
                "The model's server did not begin its answer within {} s.",
                UPSTREAM_START_TIMEOUT.as_secs()
            )));
        }
    };
    let status = response.status();
    if !status.is_success() {
        call.abandon();
        let text = response.text().await.unwrap_or_default();
        return Err(ApiError::upstream(status.as_u16(), &failure_detail(&text)));
    }
    if streaming {
        return Ok(stream(call, response, lease, wants_usage));
    }
    let mut answer: Value = match response.json().await {
        Ok(answer) => answer,
        Err(error) => {
            call.abandon();
            return Err(ApiError::upstream(
                502,
                &format!("the reply was not JSON: {error}"),
            ));
        }
    };
    drop(lease);
    let usage = read_usage(&answer);
    let reply_chars = answer["choices"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|choice| text_length(&choice["message"]))
        .sum();
    call.finish(usage, reply_chars, Some(started.elapsed()));
    if let Some(object) = answer.as_object_mut() {
        object.insert("model".into(), json!(call.id));
        // llama.cpp's timing report is the runtime's own business.
        object.remove("timings");
    }
    Ok(axum::Json(answer).into_response())
}

/// Takes a slot on a local model like any caller: the model is loaded if needed, and the call
/// queues behind the others while every slot is busy. The request is an interactive one.
async fn take_slot(
    daemon: &Arc<DaemonState>,
    api: &ApiState,
    model: &ResolvedModel,
    prompt_estimate: u32,
) -> Result<LocalLease, ApiError> {
    let local = daemon.local_runtime().ok_or_else(|| {
        ApiError::unavailable("No local models are set up; add a model folder in Imperium first.")
    })?;
    let caller = holder("api", &api.next_request().to_string());
    let request = LocalRequest {
        priority: Priority::Interactive,
        prompt_tokens: Some(prompt_estimate),
        uncapped: false,
    };
    local
        .supervisor
        .acquire_with(&model.model, &caller, request)
        .await
        .map_err(|error| {
            ApiError::unavailable(format!(
                "Could not load local model {}: {error}",
                model.model
            ))
        })
}

/// The request body for the model server: the relayed fields, the model's own id, and a
/// streaming request that also asks for usage, which budgets need.
fn relayed_body(request: &Map<String, Value>, model: &str, streaming: bool) -> Value {
    let mut body = Map::new();
    for key in RELAYED {
        if let Some(value) = request.get(*key).filter(|value| !value.is_null()) {
            body.insert((*key).to_string(), value.clone());
        }
    }
    // An empty tool list is refused by some servers and means nothing.
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        body.remove("tools");
        body.remove("tool_choice");
        body.remove("parallel_tool_calls");
    }
    body.insert("model".into(), json!(model));
    body.insert("stream".into(), json!(streaming));
    if streaming {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Value::Object(body)
}

/// One call to the model, with what it takes to account for it when it ends.
struct Call {
    daemon: Arc<DaemonState>,
    /// The Imperium spec the client used.
    id: String,
    /// The model's own name at its server.
    model: String,
    pricing: Option<Pricing>,
    prompt_estimate: u32,
    started: Instant,
    /// The local runtime, when the model is local: it keeps the measured speed.
    local: Option<Arc<Supervisor>>,
    /// Whether the call has been recorded (or given up as not having used anything).
    finished: AtomicBool,
    /// What the call was admitted to the budgets for; released after the call is recorded.
    _reservation: budgets::ApiReservation,
    /// The call's place among the calls in flight.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Drop for Call {
    /// A call that ends without being finished, such as one whose client disconnected, has
    /// already used the model: it is recorded by estimate (the prompt it sent and nothing of
    /// the reply), so a client that always hangs up cannot use the models unmetered.
    fn drop(&mut self) {
        self.finish(None, 0, None);
    }
}

impl Call {
    /// Gives the call up as one that used nothing (the model's server refused it).
    fn abandon(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    /// Counts the call against the budgets as an `api` call, once, and, for a local model,
    /// records how fast it generated. Without usage from the server, the call counts by
    /// estimate.
    fn finish(&self, usage: Option<Usage>, reply_chars: usize, generating: Option<Duration>) {
        if self.finished.swap(true, Ordering::SeqCst) {
            return;
        }
        let usage = usage.unwrap_or(Usage {
            prompt_tokens: u64::from(self.prompt_estimate),
            completion_tokens: (reply_chars as u64).div_ceil(3),
            cached_tokens: 0,
        });
        budgets::record(
            &self.daemon,
            &CallUsage {
                environment_id: None,
                source: Some(SOURCE_API.into()),
                spec: self.id.clone(),
                tokens: usage.prompt_tokens.saturating_add(usage.completion_tokens),
                cost_usd: self.pricing.map(|pricing| pricing.cost_usd(&usage)),
            },
            None,
        );
        if let (Some(local), Some(generating)) = (&self.local, generating)
            && usage.completion_tokens > 0
            && !generating.is_zero()
        {
            local.record_throughput(&self.model, usage.completion_tokens, generating);
        }
    }
}

/// Relays a streamed answer. The model server's connection and the slot are held by a task that
/// ends with the answer or when the client goes away, whichever comes first.
fn stream(
    call: Call,
    response: reqwest::Response,
    lease: Option<LocalLease>,
    wants_usage: bool,
) -> Response {
    let (send, receive) = mpsc::channel::<Bytes>(STREAM_BACKLOG);
    tokio::spawn(pump(call, response, lease, send, wants_usage));
    let body = Body::from_stream(futures_util::stream::unfold(
        receive,
        |mut receive| async move {
            receive
                .recv()
                .await
                .map(|chunk| (Ok::<_, Infallible>(chunk), receive))
        },
    ));
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        "text/event-stream".parse().expect("a header"),
    );
    headers.insert(header::CACHE_CONTROL, "no-cache".parse().expect("a header"));
    headers.insert("x-accel-buffering", "no".parse().expect("a header"));
    response
}

async fn pump(
    call: Call,
    response: reqwest::Response,
    lease: Option<LocalLease>,
    send: mpsc::Sender<Bytes>,
    wants_usage: bool,
) {
    let mut lines = DataLines::default();
    let mut chunks = response.bytes_stream();
    let mut usage = None;
    let mut reply_chars = 0usize;
    let mut first_token: Option<Instant> = None;
    'relay: loop {
        let chunk = match tokio::time::timeout(STREAM_IDLE_TIMEOUT, chunks.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(None) => break,
            Ok(Some(Err(error))) => {
                let failure = ApiError::unreachable(format!(
                    "The model's server stopped answering: {}",
                    error.without_url()
                ));
                deliver(&send, event(&failure.to_json())).await;
                break;
            }
            Err(_) => {
                let failure = ApiError::unreachable(format!(
                    "The model's server sent nothing for {} s; the call was given up.",
                    STREAM_IDLE_TIMEOUT.as_secs()
                ));
                deliver(&send, event(&failure.to_json())).await;
                break;
            }
        };
        for payload in lines.push(&chunk) {
            if payload == "[DONE]" {
                deliver(&send, Bytes::from_static(b"data: [DONE]\n\n")).await;
                break 'relay;
            }
            let Ok(mut value) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            let carries_usage = value.get("usage").is_some_and(|usage| !usage.is_null());
            if let Some(read) = read_usage(&value) {
                usage = Some(read);
            }
            let produced: usize = value["choices"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|choice| text_length(&choice["delta"]))
                .sum();
            if produced > 0 {
                first_token.get_or_insert_with(Instant::now);
                reply_chars += produced;
            }
            // The usage chunk was asked for by Imperium; clients get it only if they asked.
            let usage_only = carries_usage && value["choices"].as_array().is_none_or(Vec::is_empty);
            if usage_only && !wants_usage {
                continue;
            }
            if let Some(object) = value.as_object_mut() {
                object.insert("model".into(), json!(call.id));
                object.remove("timings");
                if !wants_usage {
                    object.remove("usage");
                }
            }
            if !deliver(&send, event(&value)).await {
                // The client went away or stopped reading: dropping the model server's
                // connection stops the generation, and the call counts for what it produced.
                break 'relay;
            }
        }
    }
    drop(lease);
    call.finish(
        usage,
        reply_chars,
        first_token
            .map(|first| first.elapsed())
            .or_else(|| Some(call.started.elapsed())),
    );
    // The call's claim on the budgets and its place among the calls in flight end before the
    // client sees the end of the answer, so a call it sends next finds them free.
    drop(call);
    drop(send);
}

/// Hands a chunk to the client's response body. False when the client has gone, or has left
/// the chunk unread for [`CLIENT_STALL_TIMEOUT`].
async fn deliver(send: &mpsc::Sender<Bytes>, chunk: Bytes) -> bool {
    matches!(
        tokio::time::timeout(CLIENT_STALL_TIMEOUT, send.send(chunk)).await,
        Ok(Ok(()))
    )
}

fn event(value: &Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

/// The characters of text a message or delta carries, reasoning included.
fn text_length(message: &Value) -> usize {
    ["content", "reasoning_content"]
        .iter()
        .filter_map(|key| message[*key].as_str())
        .map(|text| text.chars().count())
        .sum()
}

/// The usage a response or stream chunk reports, if it does.
fn read_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?.as_object()?;
    let count = |value: Option<&Value>| value.and_then(Value::as_u64).unwrap_or(0);
    Some(Usage {
        prompt_tokens: usage.get("prompt_tokens").and_then(Value::as_u64)?,
        completion_tokens: count(usage.get("completion_tokens")),
        cached_tokens: count(
            usage
                .get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens")),
        ),
    })
}

/// What a model server's error body says, in its own words when it has them.
fn failure_detail(body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let message = parsed.as_ref().and_then(|value| {
        let error = value.get("error")?;
        error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .map(str::to_string)
    });
    message.unwrap_or_else(|| body.chars().take(300).collect())
}

/// A rough size of the prompt in tokens, for the reservation a local model makes: three
/// characters to a token with a quarter more as margin, an inline image or file as a fixed
/// allowance, and the chat template's own tokens.
fn estimate_tokens(messages: &Value, tools: &Value) -> u32 {
    fn characters(value: &Value) -> f64 {
        match value {
            Value::String(text) if text.starts_with("data:") => INLINE_DATA_CHARS,
            Value::String(text) => text.chars().count() as f64,
            Value::Array(items) => items.iter().map(characters).sum(),
            Value::Object(fields) => fields
                .iter()
                .map(|(key, value)| key.len() as f64 + characters(value))
                .sum(),
            _ => 1.0,
        }
    }
    let content = characters(messages) + characters(tools);
    (content / 3.0 * 1.25 + 64.0)
        .ceil()
        .min(f64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_relayed_fields_reach_the_model_server() {
        let request = json!({
            "model": "llamacpp:m",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "stop": ["\n"],
            "id_slot": 3,
            "grammar": "root ::= x",
            "chat_template_kwargs": {"enable_thinking": true},
            "tools": [],
            "tool_choice": "auto",
            "top_p": null,
        });
        let body = relayed_body(request.as_object().unwrap(), "m", false);
        assert_eq!(
            body,
            json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "temperature": 0.2,
                "stop": ["\n"],
                "stream": false,
            })
        );
        let streamed = relayed_body(request.as_object().unwrap(), "m", true);
        assert_eq!(streamed["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn usage_is_read_from_a_response_or_a_final_chunk() {
        let chunk = json!({"choices": [], "usage": {
            "prompt_tokens": 30, "completion_tokens": 5,
            "prompt_tokens_details": {"cached_tokens": 10}}});
        assert_eq!(
            read_usage(&chunk),
            Some(Usage {
                prompt_tokens: 30,
                completion_tokens: 5,
                cached_tokens: 10
            })
        );
        assert_eq!(read_usage(&json!({"choices": [], "usage": null})), None);
        assert_eq!(read_usage(&json!({"choices": []})), None);
    }

    #[test]
    fn inline_images_do_not_swamp_the_prompt_estimate() {
        let big = "x".repeat(2_000_000);
        let messages = json!([{"role": "user", "content": [
            {"type": "text", "text": "what is this?"},
            {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{big}")}},
        ]}]);
        let estimate = estimate_tokens(&messages, &Value::Null);
        assert!((1000..3000).contains(&estimate), "{estimate}");
        assert!(estimate_tokens(&json!([]), &Value::Null) >= 64);
    }

    #[tokio::test(start_paused = true)]
    async fn a_client_that_stops_reading_is_given_up_on() {
        let (send, mut receive) = mpsc::channel::<Bytes>(1);
        assert!(deliver(&send, Bytes::from_static(b"a")).await);
        // The body is full and nobody reads it: the next chunk waits, then gives up.
        assert!(!deliver(&send, Bytes::from_static(b"b")).await);
        // A reader that has gone is given up on at once.
        receive.close();
        assert!(!deliver(&send, Bytes::from_static(b"c")).await);
    }

    #[test]
    fn a_servers_error_message_is_passed_on() {
        assert_eq!(
            failure_detail(r#"{"error":{"message":"context too long","type":"x"}}"#),
            "context too long"
        );
        assert_eq!(failure_detail(r#"{"error":"nope"}"#), "nope");
        assert_eq!(failure_detail("plain text"), "plain text");
    }
}
