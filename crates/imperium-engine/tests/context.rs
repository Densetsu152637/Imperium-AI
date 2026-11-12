//! Context breakdowns against fake OpenAI-compatible servers: exact counts from a llama.cpp-style
//! tokenizer, and calibrated or estimated ones where there is none.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::routing::{get, post};
use axum::{Json, Router};
use imperium_engine::{
    AcquireFuture, Availability, LocalLease, LocalModels, ResolvedModel, Session, SessionConfig,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{ContextCategory, SessionEvent, TokenCountMethod};
use imperium_tools::Workspace;
use serde_json::{Value, json};

const INSTRUCTIONS: &str = "Prefer small, reviewable changes.";
/// What the fake chat template adds per message, and once per request.
const PER_MESSAGE: u64 = 5;
const PER_REQUEST: u64 = 10;

fn words(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

/// Words a template that rewrites tool definitions adds before them.
const TOOL_INTRO: u64 = 9;

/// The tool block as the fake template renders it: as sent, or (`rewrites`) introduced and
/// spaced out to twice the words, as real templates re-serialise tool JSON.
fn tool_words(tools: &Value, rewrites: bool) -> u64 {
    let sent = words(&tools.to_string());
    match tools.as_array() {
        Some(list) if !list.is_empty() && rewrites => TOOL_INTRO + 2 * sent,
        Some(list) if !list.is_empty() => sent,
        _ => 0,
    }
}

/// The prompt size the fake server reports: every word the request carries, as its tokenizer
/// counts them, plus what its "template" adds.
fn prompt_tokens(body: &Value) -> u64 {
    rendered_tokens(body, false)
}

fn rendered_tokens(body: &Value, rewrites: bool) -> u64 {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let mut total = PER_REQUEST + PER_MESSAGE * messages.len() as u64;
    for message in &messages {
        total += words(message["content"].as_str().unwrap_or_default());
        for call in message["tool_calls"].as_array().into_iter().flatten() {
            total += words(call["function"]["name"].as_str().unwrap_or_default());
            total += words(call["function"]["arguments"].as_str().unwrap_or_default());
        }
    }
    total + tool_words(&body["tools"], rewrites)
}

fn stream(chunk: Value, usage: Option<Value>) -> String {
    let usage = usage.map_or(String::new(), |usage| {
        format!("data: {}\n\n", json!({"choices": [], "usage": usage}))
    });
    format!("data: {chunk}\n\n{usage}data: [DONE]\n\n")
}

/// Answers the first call with a `read_file` call and the second with text. The first always
/// reports usage; the second only `with_usage`, with some of the prompt cached.
fn reply(index: usize, body: &Value, with_usage: bool, rewrites: bool) -> String {
    let usage = |cached: u64| {
        json!({"prompt_tokens": rendered_tokens(body, rewrites), "completion_tokens": 3,
               "prompt_tokens_details": {"cached_tokens": cached}})
    };
    if index == 0 {
        let call = json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-1",
            "function": {"name": "read_file", "arguments": "{\"path\":\"a.txt\"}"}}]}}]});
        stream(call, Some(usage(0)))
    } else {
        stream(
            json!({"choices": [{"delta": {"content": "It says alpha."}}]}),
            with_usage.then(|| usage(7)),
        )
    }
}

#[derive(Default)]
struct Server {
    requests: Mutex<Vec<Value>>,
    tokenize_calls: AtomicUsize,
    props_calls: AtomicUsize,
}

#[derive(Clone, Copy)]
enum Tokenizer {
    /// Splits on whitespace, as the fake template counts.
    Words,
    /// Answers every tokenize request with a server error.
    Broken,
    /// No `/tokenize` route at all (a hosted-style or non-llama.cpp server).
    Missing,
    /// Splits on whitespace, and renders templates (`/apply-template`) with a tool block that
    /// is not the JSON sent.
    Template,
}

async fn serve(server: Arc<Server>, tokenizer: Tokenizer, with_usage: bool) -> String {
    let chat = {
        let server = Arc::clone(&server);
        post(move |Json(body): Json<Value>| {
            let server = Arc::clone(&server);
            async move {
                let index = {
                    let mut requests = server.requests.lock().unwrap();
                    requests.push(body.clone());
                    requests.len() - 1
                };
                let rewrites = matches!(tokenizer, Tokenizer::Template);
                (
                    [("content-type", "text/event-stream")],
                    reply(index, &body, with_usage, rewrites),
                )
            }
        })
    };
    let mut app = Router::new().route("/v1/chat/completions", chat);
    match tokenizer {
        Tokenizer::Words | Tokenizer::Broken | Tokenizer::Template => {
            let tokenize = {
                let server = Arc::clone(&server);
                post(move |Json(body): Json<Value>| {
                    let server = Arc::clone(&server);
                    async move {
                        server.tokenize_calls.fetch_add(1, Ordering::SeqCst);
                        if matches!(tokenizer, Tokenizer::Broken) {
                            return Err(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
                        }
                        let text = body["content"].as_str().unwrap_or_default();
                        let tokens: Vec<u64> = (0..words(text)).collect();
                        Ok(Json(json!({"tokens": tokens})))
                    }
                })
            };
            let props = {
                let server = Arc::clone(&server);
                get(move || {
                    let server = Arc::clone(&server);
                    async move {
                        server.props_calls.fetch_add(1, Ordering::SeqCst);
                        Json(json!({"default_generation_settings": {"n_ctx": 4096}}))
                    }
                })
            };
            app = app.route("/tokenize", tokenize).route("/props", props);
            if matches!(tokenizer, Tokenizer::Template) {
                let render = post(|Json(body): Json<Value>| async move {
                    let prompt = vec!["w"; rendered_tokens(&body, true) as usize].join(" ");
                    Json(json!({ "prompt": prompt }))
                });
                app = app.route("/apply-template", render);
            }
        }
        Tokenizer::Missing => {}
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    base_url
}

/// One llama.cpp model, always loaded, served by the fake server.
#[derive(Debug)]
struct FakeLocal(String);

impl LocalModels for FakeLocal {
    fn availability(&self, _model: &str) -> Availability {
        Availability::Loaded
    }

    fn acquire<'a>(&'a self, _model: &'a str, _holder: &'a str) -> AcquireFuture<'a> {
        let base_url = self.0.clone();
        Box::pin(async move { Ok(LocalLease::new(base_url, None, None, ())) })
    }
}

fn start(dir: &std::path::Path, base_url: String, local: bool) -> Session {
    let fake_local: Arc<dyn LocalModels> = Arc::new(FakeLocal(base_url.clone()));
    Session::spawn(SessionConfig {
        id: "context".into(),
        workspace: Workspace::new(dir).unwrap(),
        model: Some(ResolvedModel {
            spec: if local { "llamacpp:q" } else { "hosted:big" }.into(),
            endpoint: Endpoint {
                base_url: if local { String::new() } else { base_url },
                api_key: None,
            },
            model: "q".into(),
            pricing: None,
            local,
            context_window: Some(16_384),
            kind: if local { "llamacpp" } else { "openai" }.into(),
            reasoning: None,
        }),
        catalog: Vec::new(),
        routing: imperium_config::RoutingSettings::default(),
        clients: Default::default(),
        local: local.then_some(fake_local),
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: Some(INSTRUCTIONS.into()),
        personality: None,
        max_steps: 5,
        background: None,
        checkpoints: None,
        budget_gate: None,
        auto_compact: false,
        auto_compact_at: None,
        keep_agent_state: true,
        effort: Default::default(),
        journal_path: dir.join(".journal/context.jsonl"),
    })
    .unwrap()
}

struct Breakdown {
    context_window: Option<u64>,
    method: TokenCountMethod,
    categories: Vec<ContextCategory>,
    prompt_tokens: Option<u64>,
    cached_tokens: u64,
}

impl Breakdown {
    fn tokens(&self, key: &str) -> i64 {
        self.categories
            .iter()
            .find(|category| category.key == key)
            .map_or(0, |category| category.tokens)
    }

    fn sum(&self) -> i64 {
        self.categories.iter().map(|category| category.tokens).sum()
    }
}

/// Sends a message and collects the root agent's breakdowns until its turn completes.
async fn run_turn(session: &Session, text: &str) -> Vec<Breakdown> {
    let mut events = session.subscribe();
    session.send(text.into()).unwrap();
    let mut breakdowns = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::ContextBreakdown {
                context_window,
                method,
                categories,
                prompt_tokens,
                cached_tokens,
                ..
            } => breakdowns.push(Breakdown {
                context_window,
                method,
                categories,
                prompt_tokens,
                cached_tokens,
            }),
            SessionEvent::TurnCompleted { .. } => return breakdowns,
            SessionEvent::TurnFailed { message } => panic!("turn failed: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_llamacpp_model_is_counted_exactly_with_its_tokenizer() {
    let server = Arc::new(Server::default());
    let base_url = serve(Arc::clone(&server), Tokenizer::Words, true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha beta gamma\n").unwrap();
    let session = start(dir.path(), base_url, true);

    let breakdowns = run_turn(&session, "please read a.txt").await;
    assert_eq!(breakdowns.len(), 2);
    let requests = server.requests.lock().unwrap().clone();

    for (breakdown, request) in breakdowns.iter().zip(&requests) {
        assert_eq!(breakdown.method, TokenCountMethod::Exact);
        // The server's window, not the model table's.
        assert_eq!(breakdown.context_window, Some(4096));
        let reported = prompt_tokens(request);
        assert_eq!(breakdown.prompt_tokens, Some(reported));
        // The invariant: the categories, overhead included, add up to the reported size.
        assert_eq!(breakdown.sum(), reported as i64);

        let system = request["messages"][0]["content"].as_str().unwrap();
        assert_eq!(
            breakdown.tokens("system_project"),
            words(INSTRUCTIONS) as i64
        );
        assert_eq!(
            breakdown.tokens("system_base"),
            (words(system) - words(INSTRUCTIONS)) as i64
        );
        assert_eq!(
            breakdown.tokens("tools_builtin"),
            words(&request["tools"].to_string()) as i64
        );
        assert_eq!(breakdown.tokens("tools_mcp"), 0);
        assert_eq!(breakdown.tokens("user"), 3);
        let messages = request["messages"].as_array().unwrap().len() as u64;
        assert_eq!(
            breakdown.tokens("overhead"),
            (PER_REQUEST + PER_MESSAGE * messages) as i64
        );
    }

    // The first call read no reply yet; the second also read the tool call and its result.
    let (first, second) = (&breakdowns[0], &breakdowns[1]);
    assert_eq!(first.tokens("assistant"), 0);
    assert_eq!(first.tokens("tool_results"), 0);
    // `read_file` and its arguments `{"path":"a.txt"}`, one word each.
    assert_eq!(second.tokens("assistant"), 2);
    let result = requests[1]["messages"][3]["content"].as_str().unwrap();
    assert!(result.contains("alpha beta gamma"), "{result}");
    assert_eq!(second.tokens("tool_results"), words(result) as i64);
    assert_eq!(second.cached_tokens, 7);

    // Unchanged parts were tokenized once: four on the first call (base, instructions, tools,
    // user), then only the tool call's name and arguments and the result. Props once.
    assert_eq!(server.tokenize_calls.load(Ordering::SeqCst), 7);
    assert_eq!(server.props_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tools_count_as_the_template_renders_them() {
    let server = Arc::new(Server::default());
    let base_url = serve(Arc::clone(&server), Tokenizer::Template, true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.txt"),
        "alpha beta gamma
",
    )
    .unwrap();
    let session = start(dir.path(), base_url, true);

    let breakdowns = run_turn(&session, "please read a.txt").await;
    let requests = server.requests.lock().unwrap().clone();
    for (breakdown, request) in breakdowns.iter().zip(&requests) {
        assert_eq!(breakdown.method, TokenCountMethod::Exact);
        assert_eq!(breakdown.sum(), rendered_tokens(request, true) as i64);
        // The template's tool instructions and spacing are tools, not formatting.
        assert_eq!(
            breakdown.tokens("tools_builtin"),
            tool_words(&request["tools"], true) as i64
        );
        let messages = request["messages"].as_array().unwrap().len() as u64;
        assert_eq!(
            breakdown.tokens("overhead"),
            (PER_REQUEST + PER_MESSAGE * messages) as i64
        );
    }
}

#[tokio::test]
async fn without_a_tokenizer_the_estimates_are_calibrated_to_the_reported_total() {
    let server = Arc::new(Server::default());
    let base_url = serve(Arc::clone(&server), Tokenizer::Missing, true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha beta gamma\n").unwrap();
    let session = start(dir.path(), base_url, false);

    let breakdowns = run_turn(&session, "please read a.txt").await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(breakdowns.len(), 2);
    for (breakdown, request) in breakdowns.iter().zip(&requests) {
        assert_eq!(breakdown.method, TokenCountMethod::Calibrated);
        let reported = prompt_tokens(request);
        assert_eq!(breakdown.prompt_tokens, Some(reported));
        assert_eq!(breakdown.sum(), reported as i64);
        // From the model table; never invented.
        assert_eq!(breakdown.context_window, Some(16_384));
        for key in [
            "system_base",
            "system_project",
            "tools_builtin",
            "user",
            "overhead",
        ] {
            assert!(breakdown.tokens(key) > 0, "{key} is empty");
        }
    }
    assert!(breakdowns[1].tokens("assistant") > 0);
    assert!(breakdowns[1].tokens("tool_results") > 0);
}

#[tokio::test]
async fn a_failing_tokenizer_falls_back_to_calibrating_without_failing_the_turn() {
    let server = Arc::new(Server::default());
    let base_url = serve(Arc::clone(&server), Tokenizer::Broken, true).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
    let session = start(dir.path(), base_url, true);

    let breakdowns = run_turn(&session, "please read a.txt").await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(breakdowns.len(), 2);
    for (breakdown, request) in breakdowns.iter().zip(&requests) {
        assert_eq!(breakdown.method, TokenCountMethod::Calibrated);
        assert_eq!(breakdown.sum(), prompt_tokens(request) as i64);
        // The server still reports its window.
        assert_eq!(breakdown.context_window, Some(4096));
    }
    // A server error may be passing, so the tokenizer is tried again on the next call.
    assert_eq!(server.tokenize_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn without_reported_usage_the_breakdown_is_an_estimate() {
    let server = Arc::new(Server::default());
    let base_url = serve(Arc::clone(&server), Tokenizer::Words, false).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "alpha\n").unwrap();
    let session = start(dir.path(), base_url, true);

    let breakdowns = run_turn(&session, "please read a.txt").await;
    let last = breakdowns.last().unwrap();
    assert_eq!(last.method, TokenCountMethod::Estimated);
    assert_eq!(last.prompt_tokens, None);
    // The parts are still the tokenizer's counts; only the formatting is a guess.
    assert_eq!(last.tokens("user"), 3);
    assert_eq!(last.tokens("assistant"), 2);
    assert!(last.tokens("overhead") > 0);
}
