//! The unified KV pool: per-model launch settings, settings changes that restart a model once
//! nothing uses it, unlimited sessions beyond a model's old slot count, calls that queue in
//! Imperium beyond the slots and the pool without failing, and an agent parked on an approval
//! holding no claim. llama-server is replaced by in-process fakes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::post;
use imperium_api::{LocalModels, LocalRequest};
use imperium_config::{LlamaCppSettings, LocalModelSettings, RoutingSettings};
use imperium_engine::{ResolvedModel, Session, SessionConfig};
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, StopFuture, Supervisor, SwapDirection,
};
use imperium_models::Endpoint;
use imperium_protocol::session::{PermissionDecision, SessionEvent};
use imperium_tools::Workspace;

const GIB: u64 = 1024 * 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What the fake servers were asked to do.
#[derive(Debug, Default)]
struct Log {
    /// `(model, arguments)` of each launch.
    launched: Mutex<Vec<(String, String)>>,
    stopped: Mutex<Vec<String>>,
    /// How long a launch takes, in milliseconds.
    launch_ms: AtomicU32,
}

impl Log {
    fn launches(&self, model: &str) -> usize {
        lock(&self.launched)
            .iter()
            .filter(|(id, _)| id == model)
            .count()
    }

    /// The arguments of the latest launch of `model`.
    fn args(&self, model: &str) -> String {
        lock(&self.launched)
            .iter()
            .rev()
            .find(|(id, _)| id == model)
            .map(|(_, args)| args.clone())
            .unwrap_or_default()
    }

    fn stopped(&self) -> Vec<String> {
        lock(&self.stopped).clone()
    }
}

fn sse(chunk: serde_json::Value) -> String {
    format!(
        "data: {chunk}\n\ndata: {}\n\ndata: [DONE]\n\n",
        r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#
    )
}

/// Serves chat completions: two writes that need approval, then a reply.
async fn serve(calls: Arc<AtomicUsize>) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                let body = match n {
                    0 | 1 => sse(serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                        "index":0,"id":"c","function":{"name":"write_file",
                        "arguments":format!(r#"{{"path":"f{n}.txt","content":"x"}}"#)}}]}}]})),
                    _ => sse(serde_json::json!({"choices":[{"delta":{"content":"done"}}]})),
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

#[derive(Debug)]
struct FakeLauncher {
    log: Arc<Log>,
    calls: Arc<AtomicUsize>,
}

struct Running {
    model: String,
    url: String,
    log: Arc<Log>,
}

impl Server for Running {
    fn base_url(&self) -> &str {
        &self.url
    }

    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move { lock(&self.log.stopped).push(self.model.clone()) })
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            lock(&self.log.launched).push((spec.model_id.clone(), spec.args().join(" ")));
            let delay = u64::from(self.log.launch_ms.load(Ordering::SeqCst));
            tokio::time::sleep(Duration::from_millis(delay)).await;
            Ok(Box::new(Running {
                model: spec.model_id.clone(),
                url: serve(Arc::clone(&self.calls)).await,
                log: Arc::clone(&self.log),
            }) as Box<dyn Server>)
        })
    }
}

/// A small model: 8 KiB of KV cache per token.
fn model(id: &str, format: Format) -> LocalModel {
    LocalModel {
        id: id.into(),
        path: PathBuf::from(format!("{id}.gguf")),
        parts: 1,
        size_bytes: 6 * GIB,
        info: GgufInfo {
            block_count: Some(8),
            embedding_length: Some(1024),
            head_count: Some(8),
            head_count_kv: Some(2),
            ..GgufInfo::default()
        },
        mmproj: None,
        repo: None,
        format,
    }
}

fn settings() -> LlamaCppSettings {
    LlamaCppSettings {
        backend: "auto".into(),
        custom_path: None,
        idle_unload_s: 900,
        context: 4096,
        parallel: 2,
        cache_type: "f16".into(),
        slots: None,
        pool_tokens: None,
        flash_attn: "auto".into(),
        max_output_tokens: 256,
        models: BTreeMap::new(),
        ram_headroom_mb: 0,
        cache_ram_mb: 0,
        swap_cache_mb: 0,
        min_residency_s: 0,
        max_swaps_per_min: 100,
        swap_gain: 2.0,
        max_wait_s: 600,
        extra_args: Vec::new(),
    }
}

fn start_with(
    models: Vec<LocalModel>,
    settings: LlamaCppSettings,
    vram_gib: u64,
    runtimes: Runtimes,
) -> (Arc<Supervisor>, Arc<Log>) {
    let log = Arc::new(Log::default());
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
        cpu_cores: 8,
        unified_memory: false,
        overridden: true,
    };
    let launcher = Arc::new(FakeLauncher {
        log: Arc::clone(&log),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let supervisor = Supervisor::start(models, settings, runtimes, &hardware, launcher);
    (supervisor, log)
}

/// Models `a`, `b` and `c` that fit a 64 GiB GPU together.
fn start(settings: LlamaCppSettings) -> (Arc<Supervisor>, Arc<Log>) {
    let models = ["a", "b", "c"]
        .map(|id| model(id, Format::Gguf))
        .into_iter()
        .map(|mut model| {
            model.size_bytes = GIB;
            model
        })
        .collect();
    start_with(models, settings, 64, Runtimes::default())
}

fn small(prompt_tokens: u32) -> LocalRequest {
    LocalRequest {
        prompt_tokens: Some(prompt_tokens),
        ..LocalRequest::default()
    }
}

fn only(id: &str, own: LocalModelSettings) -> BTreeMap<String, LocalModelSettings> {
    BTreeMap::from([(id.to_string(), own)])
}

#[tokio::test]
async fn per_model_settings_reach_the_launch_arguments() {
    let (supervisor, log) = start(settings());
    supervisor.set_model_settings(only(
        "a",
        LocalModelSettings {
            context: Some(2048),
            pool_tokens: Some(16384),
            slots: Some(4),
            cache_type_k: Some("q8_0".into()),
            cache_type_v: Some("q8_0".into()),
            extra_args: Some(vec!["--n-cpu-moe".into(), "8".into()]),
            ..LocalModelSettings::default()
        },
    ));
    drop(supervisor.acquire("a", "s/root").await.unwrap());
    let args = log.args("a");
    assert!(
        args.contains(
            "--ctx-size 16384 --kv-unified --kv-unified-per-slot 2048 --parallel 4 \
             --no-cache-idle-slots"
        ),
        "{args}"
    );
    assert!(
        args.contains("--cache-type-k q8_0 --cache-type-v q8_0 --flash-attn auto"),
        "{args}"
    );
    assert!(args.ends_with("--n-cpu-moe 8"), "{args}");
    let loaded = supervisor.loaded();
    assert_eq!(
        (loaded[0].slots, loaded[0].pool_tokens, loaded[0].context),
        (4, 16384, 2048)
    );
    // A model without settings of its own gets the defaults: the pool is context x parallel,
    // and each slot is capped at the default context.
    drop(supervisor.acquire("b", "s/root").await.unwrap());
    let args = log.args("b");
    assert!(
        args.contains(
            "--ctx-size 8192 --kv-unified --kv-unified-per-slot 4096 --parallel 8 \
             --no-cache-idle-slots"
        ),
        "{args}"
    );
}

#[tokio::test]
async fn a_stale_model_restarts_only_when_free() {
    let (supervisor, log) = start(settings());
    let mut swaps = supervisor.swap_events();
    drop(supervisor.acquire("a", "first/root").await.unwrap());
    let more_slots = only(
        "a",
        LocalModelSettings {
            slots: Some(3),
            ..LocalModelSettings::default()
        },
    );

    // Free: nobody uses it, so it restarts with reason `config`, and the next call gets the
    // new shape.
    supervisor.set_model_settings(more_slots.clone());
    assert!(supervisor.loaded()[0].stale);
    supervisor.unload_stale_now().await;
    assert!(supervisor.loaded().is_empty());
    let event = tokio::time::timeout(Duration::from_secs(5), swaps.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (event.direction, event.model_id.as_str(), event.reason),
        (SwapDirection::Out, "a", "config")
    );
    drop(supervisor.acquire("a", "first/root").await.unwrap());
    assert!(log.args("a").contains("--parallel 3"));
    assert_eq!(log.launches("a"), 2);

    // Claimed by another session: it stays, and a newcomer waits without launching anything.
    let turn = supervisor.attach("a", "first/root");
    drop(supervisor.acquire("a", "first/root").await.unwrap());
    supervisor.set_model_settings(only(
        "a",
        LocalModelSettings {
            slots: Some(5),
            ..LocalModelSettings::default()
        },
    ));
    supervisor.unload_stale_now().await;
    assert_eq!(supervisor.loaded().len(), 1, "a claimed model is kept");
    let newcomer = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("a", "second/root").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!newcomer.is_finished());
    assert_eq!(log.launches("a"), 2, "the newcomer did not spin or launch");
    // Dropping the claim lets it restart, and the newcomer loads the new shape.
    drop(turn);
    supervisor.unload_stale_now().await;
    let lease = tokio::time::timeout(Duration::from_secs(5), newcomer)
        .await
        .expect("the newcomer loads once the model is free")
        .unwrap()
        .unwrap();
    assert!(log.args("a").contains("--parallel 5"));
    assert_eq!(log.launches("a"), 3);
    drop(lease);

    // Changing settings never waits for a load in flight.
    log.launch_ms.store(400, Ordering::SeqCst);
    let loading = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("b", "third/root").await })
    };
    while log.launches("b") == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let began = Instant::now();
    supervisor.set_model_settings(more_slots);
    assert!(began.elapsed() < Duration::from_millis(100));
    drop(loading.await.unwrap().unwrap());
}

#[tokio::test]
async fn settings_changed_during_a_load_restart_the_model_once_it_is_free() {
    let (supervisor, log) = start(settings());
    log.launch_ms.store(400, Ordering::SeqCst);
    let loading = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("a", "first/root").await })
    };
    while log.launches("a") == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    supervisor.set_model_settings(only(
        "a",
        LocalModelSettings {
            slots: Some(3),
            extra_args: Some(vec!["--n-cpu-moe".into(), "4".into()]),
            ..LocalModelSettings::default()
        },
    ));
    let lease = loading.await.unwrap().unwrap();
    // It started with the shape the load planned, arguments included, and is stale.
    let args = log.args("a");
    assert!(args.contains("--parallel 8"), "{args}");
    assert!(!args.contains("--n-cpu-moe"), "{args}");
    assert!(supervisor.loaded()[0].stale);
    drop(lease);
    log.launch_ms.store(0, Ordering::SeqCst);
    supervisor.unload_stale_now().await;
    assert!(supervisor.loaded().is_empty());
    drop(supervisor.acquire("a", "first/root").await.unwrap());
    let args = log.args("a");
    assert!(args.contains("--parallel 3"), "{args}");
    assert!(args.ends_with("--n-cpu-moe 4"), "{args}");
    assert_eq!(log.launches("a"), 2);

    // The output cap only sizes reservations: it applies without a restart.
    supervisor.set_model_settings(only(
        "a",
        LocalModelSettings {
            slots: Some(3),
            extra_args: Some(vec!["--n-cpu-moe".into(), "4".into()]),
            max_output_tokens: Some(1000),
            ..LocalModelSettings::default()
        },
    ));
    assert!(!supervisor.loaded()[0].stale);
    let lease = supervisor
        .acquire_with("a", "first/root", small(100))
        .await
        .unwrap();
    assert_eq!(lease.max_tokens, Some(1000));
    assert_eq!(log.launches("a"), 2);
}

#[tokio::test]
async fn more_sessions_than_the_old_parallel_value_run_at_once() {
    // `parallel = 2` no longer caps anything: the default is 8 slots, and six small calls
    // fit the pool together.
    let (supervisor, _) = start(settings());
    let mut leases = Vec::new();
    for n in 0..6 {
        let holder = format!("session{n}/root");
        let lease = tokio::time::timeout(
            Duration::from_secs(5),
            supervisor.acquire_with("a", &holder, small(100)),
        )
        .await
        .expect("no session waits")
        .unwrap();
        assert!(
            lease.waited < Duration::from_millis(50),
            "{:?}",
            lease.waited
        );
        leases.push(lease);
    }
    let loaded = supervisor.loaded();
    assert_eq!((loaded[0].slots, loaded[0].slots_in_use), (8, 6));
    assert_eq!(loaded[0].queued, 0);
}

#[tokio::test]
async fn calls_beyond_a_models_slots_queue_and_all_complete() {
    let mut settings = settings();
    settings.models = only(
        "a",
        LocalModelSettings {
            slots: Some(2),
            ..LocalModelSettings::default()
        },
    );
    let (supervisor, _) = start(settings);
    let live = Arc::new(AtomicU32::new(0));
    let peak = Arc::new(AtomicU32::new(0));
    let callers: Vec<_> = (0..8)
        .map(|n| {
            let supervisor = Arc::clone(&supervisor);
            let (live, peak) = (Arc::clone(&live), Arc::clone(&peak));
            let model = if n < 5 { "a" } else { "b" };
            tokio::spawn(async move {
                let holder = format!("session{n}/root");
                let lease = supervisor.acquire_with(model, &holder, small(100)).await?;
                if model == "a" {
                    let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                }
                tokio::time::sleep(Duration::from_millis(150)).await;
                if model == "a" {
                    live.fetch_sub(1, Ordering::SeqCst);
                }
                drop(lease);
                Ok::<_, String>(())
            })
        })
        .collect();
    let observer = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move {
            for _ in 0..1000 {
                if supervisor
                    .loaded()
                    .iter()
                    .any(|m| m.model_id == "a" && m.queued == 3)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            false
        })
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        for caller in callers {
            caller.await.unwrap().expect("no call is rejected");
        }
    })
    .await
    .expect("all eight calls complete");
    assert!(
        observer.await.unwrap(),
        "three calls queued behind two slots"
    );
    assert_eq!(peak.load(Ordering::SeqCst), 2, "at most two leases on a");
}

#[tokio::test]
async fn a_full_pool_makes_calls_wait_not_fail() {
    let mut settings = settings();
    settings.models = BTreeMap::from([
        (
            "a".to_string(),
            LocalModelSettings {
                max_output_tokens: Some(3000),
                ..LocalModelSettings::default()
            },
        ),
        (
            "c".to_string(),
            LocalModelSettings {
                max_output_tokens: Some(0),
                ..LocalModelSettings::default()
            },
        ),
    ]);
    let (supervisor, _) = start(settings);
    // The pool is 8192 tokens (4096 x parallel 2): two calls reserving 1000 + 3000 fill it.
    let first = supervisor
        .acquire_with("a", "s1/root", small(1000))
        .await
        .unwrap();
    assert_eq!(
        first.max_tokens,
        Some(3000),
        "the cap is the reservation less the prompt"
    );
    let second = supervisor
        .acquire_with("a", "s2/root", small(1000))
        .await
        .unwrap();
    assert_eq!(supervisor.loaded()[0].pool_reserved_tokens, 8000);
    let third = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire_with("a", "s3/root", small(1000)).await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!third.is_finished(), "the third call waits for the pool");
    assert_eq!(supervisor.loaded()[0].queued, 1);
    drop(first);
    let third = tokio::time::timeout(Duration::from_secs(5), third)
        .await
        .expect("it runs once tokens are released")
        .unwrap()
        .expect("a full pool is a wait, not an error");
    drop((second, third));

    // An estimate that fills the context is clamped to it, and leaves no room to cap: the
    // request is sent uncapped, so the server's own over-context error would surface.
    for (prompt, expected) in [(100_000, None), (4096, None), (4095, Some(1))] {
        let lease = supervisor
            .acquire_with("a", "s4/root", small(prompt))
            .await
            .unwrap();
        assert_eq!(lease.max_tokens, expected, "prompt of {prompt} tokens");
        assert!(supervisor.loaded()[0].pool_reserved_tokens <= 4096);
    }

    // No output cap reserves the whole context and sends no `max_tokens`.
    let uncapped = supervisor
        .acquire_with("c", "s5/root", small(100))
        .await
        .unwrap();
    assert_eq!(uncapped.max_tokens, None);
    let c = supervisor
        .loaded()
        .into_iter()
        .find(|m| m.model_id == "c")
        .unwrap();
    assert_eq!(c.pool_reserved_tokens, 4096);
    drop(uncapped);

    // A retry without the cap reserves the whole context too.
    let retry = supervisor
        .acquire_with(
            "a",
            "s6/root",
            LocalRequest {
                uncapped: true,
                ..small(100)
            },
        )
        .await
        .unwrap();
    assert_eq!(retry.max_tokens, None);
}

#[tokio::test]
async fn a_vllm_model_reserves_no_tokens_and_carries_no_cap() {
    let runtimes = Runtimes {
        vllm_available: true,
        ..Runtimes::default()
    };
    let (supervisor, log) = start_with(
        vec![model("hf", Format::Safetensors)],
        settings(),
        64,
        runtimes,
    );
    let mut leases = Vec::new();
    for n in 0..3 {
        let holder = format!("s{n}/root");
        leases.push(
            supervisor
                .acquire_with("hf", &holder, small(1_000_000))
                .await
                .unwrap(),
        );
    }
    assert!(leases.iter().all(|lease| lease.max_tokens.is_none()));
    assert_eq!(supervisor.loaded()[0].pool_reserved_tokens, 0);
    let args = log.args("hf");
    assert!(args.contains("--max-model-len 4096"), "{args}");
}

fn local_model(model: &str) -> ResolvedModel {
    ResolvedModel {
        spec: format!("llamacpp:{model}"),
        endpoint: Endpoint {
            base_url: String::new(),
            api_key: None,
        },
        model: model.into(),
        pricing: None,
        local: true,
        context_window: None,
        kind: "openai".into(),
    }
}

async fn next_event(
    events: &mut tokio::sync::broadcast::Receiver<imperium_protocol::session::AgentEvent>,
) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .expect("an event within 10 s")
        .unwrap()
        .event
}

#[tokio::test]
async fn a_session_parked_on_an_approval_holds_no_claim() {
    // Two 6 GiB models on a 10 GiB GPU: one at a time.
    let (supervisor, log) = start_with(
        vec![
            model("emperor", Format::Gguf),
            model("worker", Format::Gguf),
        ],
        settings(),
        10,
        Runtimes::default(),
    );
    let local: Arc<dyn LocalModels> = supervisor.clone();
    let dir = tempfile::tempdir().unwrap();
    let session = Session::spawn(SessionConfig {
        id: "parked".into(),
        workspace: Workspace::new(dir.path()).unwrap(),
        model: Some(local_model("emperor")),
        catalog: Vec::new(),
        clients: Default::default(),
        routing: RoutingSettings::default(),
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
        resume_policy: imperium_engine::default_resume_policy(),
        instructions: None,
        max_steps: 10,
        background: None,
        auto_compact: false,
        keep_agent_state: true,
        journal_path: dir.path().join("journal.jsonl"),
    })
    .unwrap();
    let mut events = session.subscribe();
    session.send("write two files".into()).unwrap();

    // First approval: the agent is asleep, so another session's load takes the GPU.
    let request_id = loop {
        if let SessionEvent::PermissionRequested { request_id, .. } = next_event(&mut events).await
        {
            break request_id;
        }
    };
    let lease = tokio::time::timeout(
        Duration::from_secs(5),
        supervisor.acquire("worker", "other/root"),
    )
    .await
    .expect("a session parked on an approval does not hold the GPU")
    .unwrap();
    assert_eq!(log.stopped(), ["emperor"]);
    drop(lease);
    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();

    // Second approval: the turn reloaded its model; asleep again, it is unloaded when idle.
    let request_id = loop {
        if let SessionEvent::PermissionRequested { request_id, .. } = next_event(&mut events).await
        {
            break request_id;
        }
    };
    assert!(
        supervisor.loaded().iter().any(|m| m.model_id == "emperor"),
        "the answered approval reloaded the model"
    );
    supervisor.unload_idle_now(Duration::ZERO).await;
    assert!(supervisor.loaded().is_empty());
    session
        .respond(request_id, PermissionDecision::Allow)
        .unwrap();
    loop {
        match next_event(&mut events).await {
            SessionEvent::TurnCompleted { .. } => break,
            SessionEvent::TurnFailed { message } => panic!("the turn failed: {message}"),
            _ => {}
        }
    }
    assert_eq!(log.launches("emperor"), 3, "reloaded after each park");
}
