//! The OpenAI-compatible local API server, against a fake local runtime: a launcher that
//! "starts" a model by pointing at an in-process stand-in for llama-server. The server lists
//! models, answers chat completions streamed and not, refuses callers without the token, calls
//! through the residency controller, counts against budgets as the `api` source, refuses a
//! non-loopback bind, serves hosted models only when opted in, and never writes its token to
//! the config or the logs.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use imperium_config::MemorySecrets;
use imperium_core::{Connection, DaemonState, LocalRuntime};
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, Supervisor,
};
use imperium_protocol::jsonrpc::{Outcome, Response};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const GIB: u64 = 1024 * 1024 * 1024;

/// What the fake model server received: the `Authorization` header and the JSON body.
type Received = Arc<Mutex<Vec<(Option<String>, Value)>>>;

/// A stand-in for llama-server (and for a hosted endpoint): answers every chat completion
/// with "Hello" and 12 prompt plus 5 completion tokens. A user message of "reject" is refused
/// as llama.cpp refuses a prompt that does not fit.
async fn fake_model_server() -> (String, Received) {
    let received: Received = Arc::default();
    async fn chat(
        State(received): State<Received>,
        headers: HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        let auth = headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        received.lock().unwrap().push((auth, body.clone()));
        // A user message of "slow" makes the server take a while; "hang" never answers.
        match body["messages"][0]["content"].as_str() {
            Some("slow") => tokio::time::sleep(Duration::from_millis(800)).await,
            Some("hang") => tokio::time::sleep(Duration::from_secs(120)).await,
            _ => {}
        }
        let rejected = body["messages"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|m| m["content"] == "reject"));
        if rejected {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                axum::Json(json!({"error": {"message": "the prompt does not fit the context"}})),
            )
                .into_response();
        }
        let usage = json!({"prompt_tokens": 12, "completion_tokens": 5});
        if body["stream"] == true {
            let chunk = |delta: Value, finish: Value| {
                json!({"id": "c1", "object": "chat.completion.chunk", "model": "upstream",
                       "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
            };
            let events = [
                chunk(json!({"role": "assistant", "content": "Hel"}), Value::Null),
                chunk(json!({"content": "lo"}), Value::Null),
                chunk(json!({}), json!("stop")),
                json!({"id": "c1", "object": "chat.completion.chunk", "model": "upstream",
                       "choices": [], "usage": usage, "timings": {"predicted_per_second": 40.0}}),
            ];
            let mut text = String::new();
            for event in events {
                text.push_str(&format!("data: {event}\n\n"));
            }
            text.push_str("data: [DONE]\n\n");
            return ([("content-type", "text/event-stream")], text).into_response();
        }
        axum::Json(json!({
            "id": "c1", "object": "chat.completion", "created": 1, "model": "upstream",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "Hello"}}],
            "usage": usage, "timings": {"predicted_per_second": 40.0},
        }))
        .into_response()
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(Arc::clone(&received));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, received)
}

#[derive(Debug)]
struct FakeLauncher(String);

struct FakeServer(String);

impl Server for FakeServer {
    fn base_url(&self) -> &str {
        &self.0
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, _spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move { Ok(Box::new(FakeServer(self.0.clone())) as Box<dyn Server>) })
    }
}

fn tiny_model() -> LocalModel {
    LocalModel {
        id: "tiny".into(),
        path: "tiny.gguf".into(),
        parts: 1,
        size_bytes: GIB,
        info: GgufInfo {
            block_count: Some(32),
            embedding_length: Some(4096),
            head_count: Some(32),
            head_count_kv: Some(8),
            ..GgufInfo::default()
        },
        mmproj: None,
        repo: None,
        format: Format::Gguf,
    }
}

struct Fixture {
    state: Arc<DaemonState>,
    connection: Connection,
    received: Received,
    secrets: Arc<MemorySecrets>,
    next_id: i64,
}

/// A daemon with one local model, "tiny", served by the fake server, and one hosted row,
/// "fake:big", served by it too. `extra` is more user config.
async fn fixture(home: &Path, extra: &str) -> Fixture {
    let (url, received) = fake_model_server().await;
    let config = format!(
        "[models]\ndefault = \"llamacpp:tiny\"\n\
         [[models.catalog]]\nid = \"big\"\nspec = \"fake:big\"\nalias = \"Big hosted\"\n\
         location = \"cloud\"\ntier = \"strong\"\n\
         [endpoints.fake]\nbase_url = \"{url}\"\napi_key = {{ secret = \"fake-key\" }}\n\
         [endpoints.fake.pricing.big]\ninput_usd_per_mtok = 1.0\noutput_usd_per_mtok = 2.0\n\
         [runtime.llamacpp.models.tiny]\nalias = \"Tiny local\"\n\
         [workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}"
    );
    let paths = imperium_config::Paths::under(home);
    std::fs::create_dir_all(paths.config_dir()).unwrap();
    std::fs::write(paths.user_config_file(), config).unwrap();
    let settings = imperium_config::load(&paths, None, Vec::new())
        .unwrap()
        .settings()
        .unwrap();
    let hardware = Hardware {
        gpus: vec![Gpu {
            name: "test".into(),
            vendor: "nvidia".into(),
            vram_total_bytes: 24 * GIB,
            vram_free_bytes: 24 * GIB,
            driver: None,
        }],
        ram_total_bytes: 64 * GIB,
        ram_available_bytes: 64 * GIB,
        cpu_cores: 16,
        unified_memory: false,
        overridden: true,
    };
    let supervisor = Supervisor::start(
        vec![tiny_model()],
        settings.runtime.llamacpp.clone(),
        Runtimes::default(),
        &hardware,
        Arc::new(FakeLauncher(url)),
    );
    let local = LocalRuntime::with_supervisor(supervisor, hardware);
    let secrets = Arc::new(MemorySecrets::new());
    imperium_config::SecretStore::store(&*secrets, &"fake-key".parse().unwrap(), "sk-hosted-key")
        .unwrap();
    let state = DaemonState::with_secrets(paths, settings, Some(local), secrets.clone());
    let connection = Connection::new(Arc::clone(&state));
    let mut fixture = Fixture {
        state,
        connection,
        received,
        secrets,
        next_id: 1,
    };
    fixture
        .call(
            "system.handshake",
            json!({"client": {"name": "test", "version": "0"},
                   "protocol": imperium_protocol::PROTOCOL_VERSION}),
        )
        .await;
    fixture
}

impl Fixture {
    async fn outcome(&mut self, method: &str, params: Value) -> Outcome {
        self.next_id += 1;
        let frame =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        let handled = self.connection.handle_text(&frame.to_string());
        let Response { outcome, .. } = match (handled.response, handled.deferred) {
            (Some(response), _) => response,
            (None, Some(deferred)) => tokio::time::timeout(Duration::from_secs(20), deferred)
                .await
                .expect("an answer within 20 s"),
            (None, None) => panic!("no reply to {method}"),
        };
        outcome
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        match self.outcome(method, params).await {
            Outcome::Result(value) => value,
            Outcome::Error(error) => panic!("{method}: {error:?}"),
        }
    }

    /// Enables the API and returns its base URL and token.
    async fn enable(&mut self) -> (String, String) {
        let status = self.call("api.set", json!({"enabled": true})).await;
        assert_eq!(status["running"], true, "{status}");
        let token = self.call("api.token", json!({})).await["token"]
            .as_str()
            .unwrap()
            .to_string();
        (status["base_url"].as_str().unwrap().to_string(), token)
    }
}

fn chat_body(model: &str, extra: Value) -> Value {
    let mut body = json!({"model": model, "messages": [{"role": "user", "content": "hi"}]});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

async fn post_chat(base: &str, token: &str, body: &Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap()
}

/// The JSON payloads of a server-sent-events body, `[DONE]` as the string "[DONE]".
fn events(body: &str) -> Vec<Value> {
    body.split("\n\n")
        .filter_map(|event| event.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).unwrap_or_else(|_| json!(data)))
        .collect()
}

#[tokio::test]
async fn models_and_chat_completions_answer_in_the_openai_shape() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let status = fixture.call("api.get", json!({})).await;
    assert_eq!(status["enabled"], false);
    assert_eq!(status["running"], false);
    let (base, token) = fixture.enable().await;
    assert!(base.starts_with("http://127.0.0.1:"), "{base}");
    let http = reqwest::Client::new();

    // No token, or the wrong one: 401 in OpenAI's error shape, for every route.
    for (path, token) in [
        ("/models", None),
        ("/models", Some("wrong")),
        ("/nope", None),
    ] {
        let mut request = http.get(format!("{base}{path}"));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let refused = request.send().await.unwrap();
        assert_eq!(refused.status(), 401, "{path}");
        let body: Value = refused.json().await.unwrap();
        assert_eq!(body["error"]["code"], "invalid_api_key");
        assert!(body["error"]["message"].is_string());
    }
    let unauthenticated_chat = http
        .post(format!("{base}/chat/completions"))
        .json(&chat_body("llamacpp:tiny", json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated_chat.status(), 401);
    assert!(
        fixture.received.lock().unwrap().is_empty(),
        "nothing reached the model"
    );

    // The local model is listed under its spec with its alias; the hosted row is not.
    let listed: Value = http
        .get(format!("{base}/models"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["object"], "list");
    assert_eq!(listed["data"].as_array().unwrap().len(), 1, "{listed}");
    let model = &listed["data"][0];
    assert_eq!(model["id"], "llamacpp:tiny");
    assert_eq!(model["object"], "model");
    assert_eq!(model["name"], "Tiny local");
    assert!(model["created"].is_i64());
    let one: Value = http
        .get(format!("{base}/models/llamacpp:tiny"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(one["id"], "llamacpp:tiny");

    // A non-streaming completion: the model's id is the client's, not the server's.
    let reply = post_chat(
        &base,
        &token,
        &chat_body(
            "llamacpp:tiny",
            json!({"temperature": 0.2, "max_tokens": 64, "stop": ["\n"],
                   "id_slot": 7, "grammar": "root ::= x"}),
        ),
    )
    .await;
    assert_eq!(reply.status(), 200);
    let reply: Value = reply.json().await.unwrap();
    assert_eq!(reply["object"], "chat.completion");
    assert_eq!(reply["model"], "llamacpp:tiny");
    assert_eq!(reply["choices"][0]["message"]["content"], "Hello");
    assert_eq!(reply["usage"]["completion_tokens"], 5);
    assert!(reply.get("timings").is_none());
    {
        let received = fixture.received.lock().unwrap();
        let (auth, sent) = received.last().unwrap();
        assert_eq!(*auth, None, "no key goes to a local server");
        assert_eq!(sent["model"], "tiny");
        assert_eq!(sent["temperature"], 0.2);
        assert_eq!(sent["stop"], json!(["\n"]));
        assert_eq!(sent["stream"], false);
        assert!(sent["max_tokens"].as_u64().unwrap() <= 64);
        assert_ne!(sent["id_slot"], 7, "the slot is the runtime's to choose");
        assert!(
            sent.get("grammar").is_none(),
            "server-specific fields are not relayed"
        );
    }

    // Tools are passed through to a backend that takes them.
    let tools =
        json!([{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}]);
    let with_tools = post_chat(
        &base,
        &token,
        &chat_body(
            "llamacpp:tiny",
            json!({"tools": tools, "tool_choice": "auto"}),
        ),
    )
    .await;
    assert_eq!(with_tools.status(), 200);
    assert_eq!(
        fixture.received.lock().unwrap().last().unwrap().1["tools"],
        tools
    );

    // A streamed completion: server-sent events ending in [DONE]. The usage chunk Imperium
    // asked for to count the call is dropped unless the client asked for it too.
    let streamed = post_chat(
        &base,
        &token,
        &chat_body("llamacpp:tiny", json!({"stream": true})),
    )
    .await;
    assert_eq!(streamed.status(), 200);
    assert_eq!(streamed.headers()["content-type"], "text/event-stream");
    let text = streamed.text().await.unwrap();
    let chunks = events(&text);
    assert_eq!(chunks.last().unwrap(), "[DONE]");
    let content: String = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(content, "Hello");
    assert!(
        chunks[..chunks.len() - 1]
            .iter()
            .all(|chunk| chunk["model"] == "llamacpp:tiny")
    );
    assert!(
        chunks.iter().all(|chunk| chunk.get("usage").is_none()),
        "{text}"
    );
    let with_usage = post_chat(
        &base,
        &token,
        &chat_body(
            "llamacpp:tiny",
            json!({"stream": true, "stream_options": {"include_usage": true}}),
        ),
    )
    .await
    .text()
    .await
    .unwrap();
    let usage_chunk = events(&with_usage)
        .into_iter()
        .find(|chunk| chunk["usage"].is_object())
        .expect("the usage chunk");
    assert_eq!(usage_chunk["usage"]["prompt_tokens"], 12);
    assert_eq!(usage_chunk["choices"], json!([]));

    // Refusals are OpenAI-shaped too.
    let unknown = post_chat(&base, &token, &chat_body("llamacpp:nothing", json!({}))).await;
    assert_eq!(unknown.status(), 404);
    assert_eq!(
        unknown.json::<Value>().await.unwrap()["error"]["code"],
        "model_not_found"
    );
    let no_messages = post_chat(&base, &token, &json!({"model": "llamacpp:tiny"})).await;
    assert_eq!(no_messages.status(), 400);
    assert_eq!(
        no_messages.json::<Value>().await.unwrap()["error"]["type"],
        "invalid_request_error"
    );
    let rejected = post_chat(
        &base,
        &token,
        &json!({"model": "llamacpp:tiny", "messages": [{"role": "user", "content": "reject"}]}),
    )
    .await;
    assert_eq!(rejected.status(), 400);
    assert!(
        rejected.json::<Value>().await.unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not fit")
    );
}

#[tokio::test]
async fn calls_take_their_slot_through_the_residency_controller() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    let mut announcements = fixture.state.announcements();
    assert!(fixture.state.loaded_local_models().is_empty());

    let reply = post_chat(&base, &token, &chat_body("llamacpp:tiny", json!({}))).await;
    assert_eq!(reply.status(), 200);

    // The API call loaded the model through the supervisor and said so like any other load.
    let loaded = fixture.state.loaded_local_models();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].model_id, "tiny");
    let residency = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let notification = announcements.recv().await.unwrap();
            if notification.method == "model.residency" && notification.params["action"] == "load" {
                break notification.params;
            }
        }
    })
    .await
    .expect("a model.residency load");
    assert_eq!(residency["model_id"], "tiny", "{residency}");
    assert_eq!(residency["reason"], "call", "a call needed it: {residency}");
    assert_eq!(residency["ok"], true);
}

#[tokio::test]
async fn a_used_up_api_budget_refuses_calls_in_an_openai_shaped_error() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    // 30 tokens through the API per day, then block. Each call here is 17 tokens.
    let saved = fixture
        .call(
            "budgets.set",
            json!({
                "week_start": "monday", "month_start_day": 1, "thresholds": [50, 80, 100],
                "limits": [{
                    "id": "", "name": "", "scope": "source", "target": "api",
                    "window": "daily", "tokens": 30, "cost_usd": null,
                    "thresholds": null, "action": "block"
                }]
            }),
        )
        .await;
    assert_eq!(saved["status"][0]["label"], "API calls, daily");

    let body = chat_body("llamacpp:tiny", json!({}));
    assert_eq!(post_chat(&base, &token, &body).await.status(), 200);
    assert_eq!(post_chat(&base, &token, &body).await.status(), 200);
    let status = fixture.call("budgets.get", json!({})).await;
    assert_eq!(status["status"][0]["tokens_used"], 34);
    assert_eq!(status["status"][0]["exceeded"], true);

    let before = fixture.received.lock().unwrap().len();
    let refused = post_chat(&base, &token, &body).await;
    assert_eq!(refused.status(), 429);
    let error: Value = refused.json().await.unwrap();
    assert_eq!(error["error"]["type"], "insufficient_quota");
    assert_eq!(error["error"]["code"], "budget_exceeded");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("API calls")
    );
    assert_eq!(
        fixture.received.lock().unwrap().len(),
        before,
        "a refused call never reaches the model"
    );
    // The same for a streamed request.
    let streamed = post_chat(
        &base,
        &token,
        &chat_body("llamacpp:tiny", json!({"stream": true})),
    )
    .await;
    assert_eq!(streamed.status(), 429);

    // A budget that asks cannot be asked through the API, so it refuses too; and a session-only
    // scope does not touch API calls.
    let mut ask = saved["settings"].clone();
    ask["limits"][0]["action"] = json!("ask");
    fixture.call("budgets.set", ask).await;
    let asked = post_chat(&base, &token, &body).await;
    assert_eq!(asked.status(), 429);
    let message = asked.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("cannot be asked"), "{message}");
    assert!(!message.contains("  "), "no stray spaces: {message:?}");
    let mut elsewhere = saved["settings"].clone();
    elsewhere["limits"][0]["scope"] = json!("environment");
    elsewhere["limits"][0]["target"] = json!("some-environment");
    fixture.call("budgets.set", elsewhere).await;
    assert_eq!(post_chat(&base, &token, &body).await.status(), 200);
}

#[tokio::test]
async fn a_non_loopback_bind_is_refused_unless_the_lan_is_allowed() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(
        home.path(),
        "[api.openai_compatible]\nenabled = true\nbind = \"0.0.0.0\"\n",
    )
    .await;
    // Started from the config, as the daemon does at launch: refused, and nothing listens.
    let started = fixture.state.start_openai().await;
    let error = started.unwrap_err();
    assert!(error.contains("allow_lan"), "{error}");
    assert_eq!(fixture.state.openai_address(), None);
    let status = fixture.call("api.get", json!({})).await;
    assert_eq!(status["running"], false);
    assert!(
        status["error"].as_str().unwrap().contains("allow_lan"),
        "{status}"
    );

    // Through the protocol the change is refused before it is saved.
    let refused = fixture
        .outcome("api.set", json!({"bind": "192.168.1.5"}))
        .await;
    let Outcome::Error(refused) = refused else {
        panic!("a LAN address was accepted")
    };
    assert_eq!(
        refused.code,
        imperium_protocol::jsonrpc::codes::INVALID_PARAMS
    );
    let saved =
        std::fs::read_to_string(imperium_config::Paths::under(home.path()).user_config_file())
            .unwrap();
    assert!(!saved.contains("192.168.1.5"), "{saved}");

    // Back on loopback it starts.
    let status = fixture.call("api.set", json!({"bind": "127.0.0.1"})).await;
    assert_eq!(status["running"], true, "{status}");
    assert_eq!(status["error"], Value::Null);
}

#[tokio::test]
async fn the_port_is_chosen_once_kept_and_disabling_stops_the_server() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    let status = fixture.call("api.get", json!({})).await;
    let port = status["port"].as_u64().unwrap();
    assert!(port > 0);
    assert!(base.ends_with(&format!(":{port}/v1")));
    let saved =
        std::fs::read_to_string(imperium_config::Paths::under(home.path()).user_config_file())
            .unwrap();
    assert!(saved.contains(&format!("port = {port}")), "{saved}");
    assert_eq!(
        fixture.state.settings().api.openai_compatible.port as u64,
        port
    );

    let off = fixture.call("api.set", json!({"enabled": false})).await;
    assert_eq!(off["running"], false);
    assert_eq!(off["enabled"], false);
    let gone = reqwest::Client::new()
        .get(format!("{base}/models"))
        .bearer_auth(&token)
        .send()
        .await;
    assert!(gone.is_err(), "nothing listens after disabling");

    // Enabling again takes the same port.
    let on = fixture.call("api.set", json!({"enabled": true})).await;
    assert_eq!(on["port"].as_u64().unwrap(), port);
    assert_eq!(on["running"], true, "{on}");
}

#[tokio::test]
async fn hosted_models_are_served_only_after_an_explicit_opt_in() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    let hosted = chat_body("fake:big", json!({}));

    // Off by default: not listed, and refused with a pointer to the opt-in.
    let status = fixture.call("api.get", json!({})).await;
    let big = status["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "fake:big")
        .expect("the hosted row is offered to opt in");
    assert_eq!(
        (big["hosted"].clone(), big["exposed"].clone()),
        (json!(true), json!(false))
    );
    let refused = post_chat(&base, &token, &hosted).await;
    assert_eq!(refused.status(), 404);
    let message = refused.json::<Value>().await.unwrap()["error"]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("Settings → API"), "{message}");
    assert!(fixture.received.lock().unwrap().is_empty());

    // Only rows of the model table that are hosted can be opted in.
    for bad in ["fake:not-in-the-table", "llamacpp:tiny"] {
        let outcome = fixture.outcome("api.set", json!({"expose": [bad]})).await;
        assert!(matches!(outcome, Outcome::Error(_)), "{bad} was accepted");
    }

    let status = fixture
        .call("api.set", json!({"expose": ["fake:big"]}))
        .await;
    assert_eq!(status["expose"], json!(["fake:big"]));
    let listed: Value = reqwest::Client::new()
        .get(format!("{base}/models"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["llamacpp:tiny", "fake:big"]);
    assert_eq!(listed["data"][1]["name"], "Big hosted");

    let reply = post_chat(&base, &token, &hosted).await;
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.json::<Value>().await.unwrap()["model"], "fake:big");
    {
        let received = fixture.received.lock().unwrap();
        let (auth, sent) = received.last().unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer sk-hosted-key"));
        assert_eq!(sent["model"], "big");
    }
    // It costs money, so a budget on its endpoint counts it: 12 in, 5 out at $1 and $2 per M.
    let usage = fixture.call("budgets.usage", json!({"days": 1})).await;
    let model = usage["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "fake:big")
        .unwrap();
    assert_eq!(model["tokens"], 17);
    assert!((model["cost_usd"].as_f64().unwrap() - 0.000_022).abs() < 1e-9);

    // Taking it away serves it no more.
    fixture.call("api.set", json!({"expose": []})).await;
    assert_eq!(post_chat(&base, &token, &hosted).await.status(), 404);
}

/// Collects what `tracing` writes.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn files_under(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_under(&path, found);
        } else {
            found.push(path);
        }
    }
}

#[tokio::test]
async fn the_token_is_never_written_to_the_config_the_data_folder_or_the_logs() {
    let logs = LogBuffer::default();
    let _logging = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish(),
    );
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    // Use it, refuse a wrong one, rotate it: all of it logged at the most verbose level.
    assert_eq!(
        post_chat(&base, &token, &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        200
    );
    assert_eq!(
        post_chat(&base, "imp-wrong", &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        401
    );
    let rotated = fixture.call("api.token", json!({"rotate": true})).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(rotated, token);
    assert_eq!(
        post_chat(&base, &token, &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        401,
        "the old token stops working"
    );
    assert_eq!(
        post_chat(&base, &rotated, &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        200
    );
    let status = fixture.call("api.get", json!({})).await;
    let described = fixture.call("config.describe", json!({})).await;

    // The token is in the secret store, and only there.
    let stored =
        imperium_config::SecretStore::load(&*fixture.secrets, &"api.openai-token".parse().unwrap())
            .unwrap();
    assert_eq!(stored.as_deref(), Some(rotated.as_str()));
    let mut files = Vec::new();
    files_under(home.path(), &mut files);
    let config = imperium_config::Paths::under(home.path()).user_config_file();
    assert!(files.contains(&config));
    for file in files {
        let bytes = std::fs::read(&file).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for secret in [&token, &rotated] {
            assert!(
                !text.contains(secret.as_str()),
                "the token is in {}",
                file.display()
            );
        }
    }
    let config_text = std::fs::read_to_string(&config).unwrap();
    assert!(config_text.contains("enabled = true"), "{config_text}");
    for secret in [&token, &rotated] {
        assert!(!status.to_string().contains(secret.as_str()));
        assert!(!described.to_string().contains(secret.as_str()));
        assert!(!format!("{:?}", fixture.state.openai_address()).contains(secret.as_str()));
    }
    let logged = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("OpenAI-compatible API"),
        "the test saw the logs: {logged}"
    );
    for secret in [&token, &rotated, &"sk-hosted-key".to_string()] {
        assert!(!logged.contains(secret.as_str()), "a secret is in the logs");
    }
}

fn slow_body() -> Value {
    json!({"model": "llamacpp:tiny", "messages": [{"role": "user", "content": "slow"}]})
}

#[tokio::test]
async fn no_more_calls_run_at_once_than_the_cap_allows() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "[api.openai_compatible]\nmax_concurrent = 2\n").await;
    let (base, token) = fixture.enable().await;
    let body = slow_body();
    let (a, b, c) = tokio::join!(
        post_chat(&base, &token, &body),
        post_chat(&base, &token, &body),
        post_chat(&base, &token, &body)
    );
    let mut statuses = vec![
        a.status().as_u16(),
        b.status().as_u16(),
        c.status().as_u16(),
    ];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 200, 429]);
    let refused = [a, b, c]
        .into_iter()
        .find(|reply| reply.status() == 429)
        .unwrap();
    let error: Value = refused.json().await.unwrap();
    assert_eq!(error["error"]["code"], "rate_limit_exceeded");
    // The places are free again once the calls end, a streamed one's included.
    assert_eq!(post_chat(&base, &token, &slow_body()).await.status(), 200);
    let mut streamed = slow_body();
    streamed["stream"] = json!(true);
    let reply = post_chat(&base, &token, &streamed).await;
    assert_eq!(reply.status(), 200);
    reply.text().await.unwrap();
    let (d, e) = tokio::join!(
        post_chat(&base, &token, &body),
        post_chat(&base, &token, &body)
    );
    assert_eq!((d.status().as_u16(), e.status().as_u16()), (200, 200));
}

#[tokio::test]
async fn calls_that_arrive_together_cannot_all_pass_a_budget_with_room_for_one() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    // 30 tokens through the API per day, blocking. A call is expected to use more than that
    // when it is admitted, so while one is running the others find the budget spoken for.
    fixture
        .call(
            "budgets.set",
            json!({
                "week_start": "monday", "month_start_day": 1, "thresholds": [50, 80, 100],
                "limits": [{
                    "id": "", "name": "", "scope": "source", "target": "api",
                    "window": "daily", "tokens": 30, "cost_usd": null,
                    "thresholds": null, "action": "block"
                }]
            }),
        )
        .await;
    let body = slow_body();
    let (a, b, c) = tokio::join!(
        post_chat(&base, &token, &body),
        post_chat(&base, &token, &body),
        post_chat(&base, &token, &body)
    );
    let mut statuses = vec![
        a.status().as_u16(),
        b.status().as_u16(),
        c.status().as_u16(),
    ];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 429, 429], "exactly one call passes");
    let refused = [a, b, c]
        .into_iter()
        .find(|reply| reply.status() == 429)
        .unwrap();
    assert_eq!(
        refused.json::<Value>().await.unwrap()["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(
        fixture.received.lock().unwrap().len(),
        1,
        "one reached the model"
    );
    // What counts afterwards is what the call used, not what it was expected to.
    let status = fixture.call("budgets.get", json!({})).await;
    assert_eq!(status["status"][0]["tokens_used"], 17);
    assert_eq!(
        post_chat(&base, &token, &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        200
    );
    assert_eq!(
        post_chat(&base, &token, &chat_body("llamacpp:tiny", json!({})))
            .await
            .status(),
        429,
        "used up at 34 of 30"
    );
}

#[tokio::test]
async fn a_client_that_hangs_up_before_the_answer_still_counts() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let (base, token) = fixture.enable().await;
    fixture
        .call(
            "budgets.set",
            json!({
                "week_start": "monday", "month_start_day": 1, "thresholds": [50, 80, 100],
                "limits": [{
                    "id": "", "name": "", "scope": "source", "target": "api",
                    "window": "daily", "tokens": 1000000, "cost_usd": null,
                    "thresholds": null, "action": "warn"
                }]
            }),
        )
        .await;
    // The model never answers; the client gives up after a moment.
    let hung = reqwest::Client::new()
        .post(format!("{base}/chat/completions"))
        .bearer_auth(&token)
        .timeout(Duration::from_millis(400))
        .json(&json!({"model": "llamacpp:tiny", "messages": [{"role": "user", "content": "hang"}]}))
        .send()
        .await;
    assert!(hung.is_err());
    let used = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status = fixture.call("budgets.get", json!({})).await;
            let used = status["status"][0]["tokens_used"].as_u64().unwrap();
            if used > 0 {
                break used;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the abandoned call is recorded");
    assert!(used > 0, "counted by its estimate: {used}");
    assert_eq!(fixture.received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_server_that_never_answers_is_given_up_on_after_the_models_max_wait_and_the_call_counts()
{
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "[runtime.llamacpp]\nmax_wait_s = 2\n").await;
    let (base, token) = fixture.enable().await;
    fixture
        .call(
            "budgets.set",
            json!({
                "week_start": "monday", "month_start_day": 1, "thresholds": [50, 80, 100],
                "limits": [{
                    "id": "", "name": "", "scope": "source", "target": "api",
                    "window": "daily", "tokens": 1000000, "cost_usd": null,
                    "thresholds": null, "action": "warn"
                }]
            }),
        )
        .await;
    let began = std::time::Instant::now();
    let reply = post_chat(
        &base,
        &token,
        &json!({"model": "llamacpp:tiny", "messages": [{"role": "user", "content": "hang"}]}),
    )
    .await;
    assert_eq!(reply.status(), 502);
    let error: Value = reply.json().await.unwrap();
    assert_eq!(error["error"]["code"], "upstream_unreachable");
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("within 2 s"),
        "{error}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(30),
        "the limit is the model's, not 10 minutes"
    );
    // The server held the request, so the call is counted by its estimate.
    let status = fixture.call("budgets.get", json!({})).await;
    assert!(
        status["status"][0]["tokens_used"].as_u64().unwrap() > 0,
        "{status}"
    );
}

#[tokio::test]
async fn turning_the_network_off_keeps_a_loopback_address_the_user_chose() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(
        home.path(),
        "[api.openai_compatible]\nallow_lan = true\nbind = \"::1\"\n",
    )
    .await;
    let status = fixture.call("api.set", json!({"allow_lan": false})).await;
    assert_eq!(status["allow_lan"], false);
    assert_eq!(status["bind"], "::1");
    let saved =
        std::fs::read_to_string(imperium_config::Paths::under(home.path()).user_config_file())
            .unwrap();
    assert!(saved.contains("bind = \"::1\""), "{saved}");
}

#[tokio::test]
async fn turning_the_network_off_puts_the_address_back_on_this_computer() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(
        home.path(),
        "[api.openai_compatible]\nallow_lan = true\nbind = \"0.0.0.0\"\n",
    )
    .await;
    let status = fixture.call("api.get", json!({})).await;
    assert_eq!(
        (status["allow_lan"].clone(), status["bind"].clone()),
        (json!(true), json!("0.0.0.0"))
    );
    let status = fixture.call("api.set", json!({"allow_lan": false})).await;
    assert_eq!(status["allow_lan"], false);
    assert_eq!(status["bind"], "127.0.0.1");
    let saved =
        std::fs::read_to_string(imperium_config::Paths::under(home.path()).user_config_file())
            .unwrap();
    assert!(saved.contains("bind = \"127.0.0.1\""), "{saved}");
    assert!(saved.contains("allow_lan = false"), "{saved}");
}
