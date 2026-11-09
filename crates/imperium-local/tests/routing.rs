//! Phase 4 exit criterion: delegation reuses a loaded local model, and falls back to a hosted
//! model when local memory is short. llama-server is replaced by in-process fake servers.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::routing::post;
use futures_util::StreamExt;
use imperium_config::{LlamaCppSettings, Location, Preference, RoutingSettings, Tier};
use imperium_core::{CatalogModel, LocalModels, ResolvedModel, Session, SessionConfig};
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::LocalModel;
use imperium_local::supervisor::{LaunchFuture, LaunchSpec, Launcher, Server, Supervisor};
use imperium_models::{Endpoint, Pricing};
use imperium_protocol::session::SessionEvent;
use imperium_tools::Workspace;

const GIB: u64 = 1024 * 1024 * 1024;

fn sse(chunk: serde_json::Value) -> String {
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#
    )
}

fn text(text: &str) -> String {
    sse(serde_json::json!({"choices":[{"delta":{"content":text}}]}))
}

/// A reply whose text is generated over 150 ms, as a real server streams it: the first token,
/// then the rest. A call that streams for under 100 ms is not recorded as the model's speed.
fn generated(first: &str, rest: &str) -> axum::body::Body {
    let first = format!(
        "data: {}

",
        serde_json::json!({"choices":[{"delta":{"content":first}}]})
    );
    let rest = text(rest);
    let parts =
        futures_util::stream::iter([(first, Duration::ZERO), (rest, Duration::from_millis(150))])
            .then(|(part, pause)| async move {
                tokio::time::sleep(pause).await;
                Ok::<_, std::convert::Infallible>(part)
            });
    axum::body::Body::from_stream(parts)
}

fn tool(name: &str, arguments: &str) -> String {
    sse(serde_json::json!({"choices":[{"delta":{"tool_calls":[{
        "index":0,"id":"c","function":{"name":name,"arguments":arguments}}]}}]}))
}

/// Serves chat completions: a root that delegates twice, one task at a time, and subagents that
/// reply with a report. Counts the subagent calls it answers.
async fn serve(root_script: bool, sub_calls: Arc<AtomicUsize>) -> String {
    let root_calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let root_calls = Arc::clone(&root_calls);
            let sub_calls = Arc::clone(&sub_calls);
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default();
                let body = if !root_script || system.contains("subagent") {
                    sub_calls.fetch_add(1, Ordering::SeqCst);
                    generated("re", "port")
                } else {
                    axum::body::Body::from(match root_calls.fetch_add(1, Ordering::SeqCst) {
                        0 | 2 => tool("spawn_agent", r#"{"brief":"summarise the docs"}"#),
                        1 | 3 => tool("await_tasks", "{}"),
                        _ => text("All done."),
                    })
                };
                ([("content-type", "text/event-stream")], body)
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    url
}

/// Starts a fake llama-server per launch.
#[derive(Debug, Default)]
struct FakeLlama {
    launches: AtomicUsize,
    calls: Arc<AtomicUsize>,
}

struct Running(String);

impl Server for Running {
    fn base_url(&self) -> &str {
        &self.0
    }
}

impl Launcher for FakeLlama {
    fn launch<'a>(&'a self, _spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            self.launches.fetch_add(1, Ordering::SeqCst);
            let url = serve(false, Arc::clone(&self.calls)).await;
            Ok(Box::new(Running(url)) as Box<dyn Server>)
        })
    }
}

fn supervisor(launcher: Arc<FakeLlama>, vram_gib: u64) -> Arc<Supervisor> {
    let model = LocalModel {
        id: "small".into(),
        path: PathBuf::from("small.gguf"),
        parts: 1,
        size_bytes: GIB,
        info: GgufInfo {
            block_count: Some(16),
            embedding_length: Some(2048),
            head_count: Some(16),
            head_count_kv: Some(4),
            ..GgufInfo::default()
        },
        mmproj: None,
        format: imperium_local::scan::Format::Gguf,
    };
    let hardware = Hardware {
        gpus: vec![Gpu {
            name: "test".into(),
            vendor: "nvidia".into(),
            vram_total_bytes: vram_gib * GIB,
            vram_free_bytes: vram_gib * GIB,
            driver: None,
        }],
        ram_total_bytes: 0,
        ram_available_bytes: 0,
        cpu_cores: 4,
        unified_memory: false,
        overridden: true,
    };
    let settings = LlamaCppSettings {
        backend: "auto".into(),
        custom_path: None,
        idle_unload_s: 900,
        context: 4096,
        parallel: 1,
        cache_type: "f16".into(),
        slots: Some(1),
        pool_tokens: None,
        flash_attn: "auto".into(),
        max_output_tokens: 8192,
        models: Default::default(),
        ram_headroom_mb: 0,
        cache_ram_mb: 0,
        swap_cache_mb: 0,
        min_residency_s: 30,
        max_swaps_per_min: 6,
        swap_gain: 2.0,
        max_wait_s: 600,
        extra_args: Vec::new(),
    };
    Supervisor::start(
        vec![model],
        settings,
        imperium_local::supervisor::Runtimes::default(),
        &hardware,
        launcher,
    )
}

/// Runs one delegating turn and returns the model each task was routed to.
async fn delegate(local: Arc<Supervisor>) -> Vec<String> {
    let hosted_calls = Arc::new(AtomicUsize::new(0));
    let root_url = serve(true, Arc::clone(&hosted_calls)).await;
    let hosted = |spec: &str| ResolvedModel {
        spec: spec.into(),
        endpoint: Endpoint {
            base_url: root_url.clone(),
            api_key: None,
        },
        model: spec.into(),
        pricing: Some(Pricing {
            input_usd_per_mtok: 1.0,
            output_usd_per_mtok: 1.0,
            cached_input_usd_per_mtok: None,
        }),
        local: false,
        context_window: None,
        kind: "openai".into(),
    };
    let catalog = vec![
        CatalogModel {
            id: "small".into(),
            model: ResolvedModel {
                spec: "llamacpp:small".into(),
                endpoint: Endpoint {
                    base_url: String::new(),
                    api_key: None,
                },
                model: "small".into(),
                pricing: None,
                local: true,
                context_window: None,
                kind: "openai".into(),
            },
            tier: Tier::Standard,
            location: Location::Local,
            tags: Vec::new(),
            cpu_lane: false,
        },
        CatalogModel {
            id: "big".into(),
            model: hosted("cloud:big"),
            tier: Tier::Strong,
            location: Location::Cloud,
            tags: Vec::new(),
            cpu_lane: false,
        },
    ];
    let dir = tempfile::tempdir().unwrap();
    let local: Arc<dyn LocalModels> = local;
    let session = Session::spawn(SessionConfig {
        id: "routing".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: hosted("cloud:root"),
        catalog,
        clients: Default::default(),
        routing: RoutingSettings {
            prefer: Preference::Local,
            ..RoutingSettings::default()
        },
        local: Some(local),
        tools: Vec::new(),
        notices: Vec::new(),
        extensions: Default::default(),
        memory: None,
        context: None,
        enforce: imperium_config::Enforce::Warn,
        pre_approved: Vec::new(),
        launcher: None,
        resume: false,
        resume_policy: imperium_core::default_resume_policy(),
        instructions: None,
        max_steps: 10,
        background: None,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("summarise the docs twice".into()).unwrap();
    let mut models = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .expect("an event within 10 s")
            .unwrap();
        match event.event {
            SessionEvent::TaskSpawned { model, .. } => models.push(model),
            SessionEvent::TaskFinished { ok, report, .. } => assert!(ok, "{report}"),
            SessionEvent::TurnFailed { message } => panic!("{} failed: {message}", event.agent_id),
            SessionEvent::TurnCompleted { .. } if event.agent_id == "root" => break,
            _ => {}
        }
    }
    models
}

#[tokio::test]
async fn delegation_reuses_the_loaded_local_model() {
    let launcher = Arc::new(FakeLlama::default());
    let models = delegate(supervisor(Arc::clone(&launcher), 8)).await;
    assert_eq!(models, ["llamacpp:small", "llamacpp:small"]);
    // Loaded for the first task, reused for the second.
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    assert_eq!(launcher.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn delegation_falls_back_to_hosted_when_memory_is_short() {
    let launcher = Arc::new(FakeLlama::default());
    let models = delegate(supervisor(Arc::clone(&launcher), 1)).await;
    assert_eq!(models, ["cloud:big", "cloud:big"]);
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cpu_lane_speed_of_delegated_local_calls_is_measured() {
    let launcher = Arc::new(FakeLlama::default());
    let local = supervisor(Arc::clone(&launcher), 8);
    assert_eq!(local.tokens_per_s("small"), None);
    delegate(Arc::clone(&local)).await;
    // Each subagent call generated 5 tokens; the agent loop reported them with how long it
    // took, so the supervisor knows the model's speed (what `models.local` lists).
    let rate = local.tokens_per_s("small").expect("a measured speed");
    assert!(rate > 0.0 && rate.is_finite(), "{rate}");
}
