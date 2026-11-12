//! The local model benchmark, against a fake local runtime: a launcher that "starts" a model by
//! pointing at an in-process stand-in for llama-server that streams tokens on a schedule the test
//! sets. The numbers must come from the stream's real timings, the benchmark must take its slot
//! through the same launch path as any call, results must be saved and listed, nothing may run
//! unasked, and a running benchmark must be cancellable (its slot freed).

use std::convert::Infallible;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use imperium_config::MemorySecrets;
use imperium_core::{Connection, DaemonState, LocalRuntime};
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, Supervisor,
};
use imperium_protocol::jsonrpc::{Outcome, Response as RpcResponse};
use serde_json::{Value, json};
use tokio::net::TcpListener;

const GIB: u64 = 1024 * 1024 * 1024;

/// How the fake model server answers, changed by the test while it runs.
#[derive(Default)]
struct Behaviour {
    /// Milliseconds before the first token (the prompt "processing").
    first_ms: AtomicU64,
    /// Milliseconds between later tokens.
    gap_ms: AtomicU64,
    /// Never sends the second token: the answer hangs part-way.
    stall: AtomicBool,
    /// The bodies received.
    received: Mutex<Vec<Value>>,
}

const TOKENS: u64 = 5;
const PROMPT_TOKENS: u64 = 100;

/// A stand-in for llama-server: streams `TOKENS` tokens, then usage and `[DONE]`.
async fn fake_model_server() -> (String, Arc<Behaviour>) {
    let behaviour = Arc::new(Behaviour::default());
    async fn chat(
        State(behaviour): State<Arc<Behaviour>>,
        axum::Json(body): axum::Json<Value>,
    ) -> Response {
        behaviour.received.lock().unwrap().push(body);
        let first = behaviour.first_ms.load(Ordering::SeqCst);
        let gap = behaviour.gap_ms.load(Ordering::SeqCst);
        let stall = behaviour.stall.load(Ordering::SeqCst);
        let event = |value: Value| Bytes::from(format!("data: {value}\n\n"));
        let mut items: Vec<(Duration, Bytes)> = Vec::new();
        for index in 0..TOKENS {
            let delay = match index {
                0 => Duration::from_millis(first),
                1 if stall => Duration::from_secs(3600),
                _ => Duration::from_millis(gap),
            };
            items.push((
                delay,
                event(
                    json!({"choices": [{"index": 0, "delta": {"content": format!("t{index} ")}}]}),
                ),
            ));
        }
        items.push((
            Duration::ZERO,
            event(json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        ));
        items.push((
            Duration::ZERO,
            event(json!({"choices": [], "usage": {
                "prompt_tokens": PROMPT_TOKENS, "completion_tokens": TOKENS}})),
        ));
        items.push((Duration::ZERO, Bytes::from_static(b"data: [DONE]\n\n")));
        let stream = futures_util::stream::unfold(items.into_iter(), |mut items| async move {
            let (delay, chunk) = items.next()?;
            tokio::time::sleep(delay).await;
            Some((Ok::<_, Infallible>(chunk), items))
        });
        Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap()
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(Arc::clone(&behaviour));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await });
    (url, behaviour)
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
    behaviour: Arc<Behaviour>,
    supervisor: Arc<Supervisor>,
    data_dir: std::path::PathBuf,
    next_id: i64,
}

async fn fixture(home: &Path) -> Fixture {
    let (url, behaviour) = fake_model_server().await;
    behaviour.first_ms.store(200, Ordering::SeqCst);
    behaviour.gap_ms.store(50, Ordering::SeqCst);
    let config = "[models]\ndefault = \"llamacpp:tiny\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n";
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
    let local = LocalRuntime::with_supervisor(Arc::clone(&supervisor), hardware);
    let data_dir = paths.data_dir().to_path_buf();
    let state =
        DaemonState::with_secrets(paths, settings, Some(local), Arc::new(MemorySecrets::new()));
    let connection = Connection::new(Arc::clone(&state));
    let mut fixture = Fixture {
        state,
        connection,
        behaviour,
        supervisor,
        data_dir,
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
        let RpcResponse { outcome, .. } = match (handled.response, handled.deferred) {
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

    async fn results(&mut self) -> Value {
        self.call("models.bench_results", json!({})).await
    }

    /// Waits for the first run to leave `running`; returns it.
    async fn finished_run(&mut self) -> Value {
        for _ in 0..200 {
            let results = self.results().await;
            let run = results["runs"][0].clone();
            if run["state"] != "running" {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the benchmark did not finish");
    }
}

#[tokio::test]
async fn nothing_runs_until_a_client_asks() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let results = fixture.results().await;
    assert_eq!(results["runs"].as_array().unwrap().len(), 0);
    assert_eq!(results["results"].as_array().unwrap().len(), 0);
    assert!(fixture.behaviour.received.lock().unwrap().is_empty());
    assert!(
        fixture.supervisor.loaded().is_empty(),
        "no model was loaded"
    );
}

#[tokio::test]
async fn a_benchmark_measures_the_stream_through_the_normal_launch_path_and_saves_the_result() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path()).await;
    let mut announced = fixture.state.announcements();

    let run = fixture.call("models.bench", json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "running");
    assert_eq!(run["models"], json!(["tiny"]));
    // Only one run at a time.
    let again = fixture.outcome("models.bench", json!({})).await;
    assert!(matches!(again, Outcome::Error(_)), "{again:?}");

    let run = fixture.finished_run().await;
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(run["models_done"], 1);
    let results = fixture.results().await;
    let saved = results["results"].as_array().unwrap();
    assert_eq!(saved.len(), 1, "{results}");
    let result = &saved[0];
    assert_eq!(result["model_id"], "tiny");
    assert_eq!(result["run_id"], run["run_id"]);
    assert_eq!(result["runtime"], "llamacpp");
    assert!(result["error"].is_null(), "{result}");
    assert_eq!(
        result["hardware_fingerprint"],
        results["hardware_fingerprint"]
    );
    assert!(
        result["hardware"].as_str().unwrap().contains("test 24 GB"),
        "{result}"
    );
    assert!(result["date"].as_str().unwrap().ends_with('Z'));
    // The model was not loaded before, so its load was timed.
    assert!(result["load_s"].as_f64().is_some(), "{result}");

    // The numbers follow the schedule the server streamed: 200 ms to the first token, then four
    // more at 50 ms. Sleeps only run long, so the speeds are at most these.
    let prompts = result["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 3, "{result}");
    assert_eq!(
        prompts
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["short", "medium", "long"]
    );
    for prompt in prompts {
        let ttft = prompt["ttft_ms"].as_f64().unwrap();
        assert!((195.0..900.0).contains(&ttft), "ttft {ttft}");
        assert_eq!(prompt["prompt_tokens"], PROMPT_TOKENS);
        assert_eq!(prompt["completion_tokens"], TOKENS);
        let prompt_rate = prompt["prompt_tok_s"].as_f64().unwrap();
        assert!(
            (100.0..=520.0).contains(&prompt_rate),
            "prompt tok/s {prompt_rate}"
        );
        let gen_rate = prompt["gen_tok_s"].as_f64().unwrap();
        assert!((6.0..=22.0).contains(&gen_rate), "gen tok/s {gen_rate}");
    }
    let ttft = result["ttft_ms"].as_f64().unwrap();
    assert!((195.0..900.0).contains(&ttft), "median ttft {ttft}");
    assert!((100.0..=520.0).contains(&result["prompt_tok_s"].as_f64().unwrap()));
    assert!((6.0..=22.0).contains(&result["gen_tok_s"].as_f64().unwrap()));
    // The fake server has no process to read, so no peak memory is claimed.
    assert!(result["peak_ram_bytes"].is_null() && result["peak_vram_bytes"].is_null());

    // What was sent: streamed with usage, no prompt cache, a fixed seed and temperature.
    let received = fixture.behaviour.received.lock().unwrap().clone();
    assert_eq!(received.len(), 3);
    for body in &received {
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["cache_prompt"], false);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["seed"], 1);
    }

    // Through the residency path: the model is loaded in the supervisor (no second launcher),
    // clients were told of the load, and the slot is free again.
    let loaded = fixture.supervisor.loaded();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].model_id, "tiny");
    assert_eq!(loaded[0].slots_in_use, 0);
    let mut load_announced = false;
    let mut progress = Vec::new();
    while let Ok(notification) = announced.try_recv() {
        match notification.method.as_str() {
            "model.residency" => {
                load_announced |= notification.params["action"] == "load"
                    && notification.params["model_id"] == "tiny";
            }
            "models.bench_progress" => progress.push(notification.params),
            _ => {}
        }
    }
    assert!(load_announced, "the load was announced as model.residency");
    assert_eq!(progress.first().unwrap()["state"], "running");
    assert_eq!(progress.last().unwrap()["state"], "done");
    assert!(
        progress.iter().any(|run| run["step"]
            .as_str()
            .is_some_and(|step| step.starts_with("prompt 2 of 3"))),
        "{progress:?}"
    );

    // Saved to the data folder, per model, and filterable.
    let file = std::fs::read_to_string(fixture.data_dir.join("local-benchmarks.json")).unwrap();
    let file: Value = serde_json::from_str(&file).unwrap();
    assert_eq!(file["results"][0]["model_id"], "tiny");
    let other = fixture
        .call("models.bench_results", json!({"model": "other"}))
        .await;
    assert!(other["results"].as_array().unwrap().is_empty());

    // A second run on the now-loaded idle model: cold (the default) unloads it first, so the
    // load is timed again; the results list is newest first.
    fixture.call("models.bench", json!({"model": "tiny"})).await;
    for _ in 0..200 {
        if fixture.results().await["runs"][1]["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let results = fixture.results().await;
    let saved = results["results"].as_array().unwrap();
    assert_eq!(saved.len(), 2, "{results}");
    assert_eq!(saved[0]["run_id"], "bench-2");
    assert_eq!(saved[1]["run_id"], "bench-1");
    assert!(saved[0]["load_s"].as_f64().is_some(), "{}", saved[0]);
}

#[tokio::test]
async fn cancelling_stops_the_run_and_frees_the_slot() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path()).await;
    fixture.behaviour.stall.store(true, Ordering::SeqCst);

    let run = fixture.call("models.bench", json!({"model": "tiny"})).await;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    // Wait until the first prompt is in flight and holding the slot.
    for _ in 0..100 {
        if fixture
            .supervisor
            .loaded()
            .first()
            .is_some_and(|m| m.slots_in_use == 1)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fixture.supervisor.loaded()[0].slots_in_use,
        1,
        "the benchmark holds a slot"
    );

    let cancelled = fixture
        .call("models.bench_cancel", json!({"run_id": run_id}))
        .await;
    assert_eq!(cancelled["found"], true);
    let results = fixture.results().await;
    assert_eq!(results["runs"][0]["state"], "cancelled");
    assert!(
        results["results"].as_array().unwrap().is_empty(),
        "nothing is saved"
    );
    let again = fixture
        .call("models.bench_cancel", json!({"run_id": run_id}))
        .await;
    assert_eq!(again["found"], false);
    let missing = fixture
        .call("models.bench_cancel", json!({"run_id": "bench-99"}))
        .await;
    assert_eq!(missing["found"], false);

    for _ in 0..100 {
        if fixture.supervisor.loaded()[0].slots_in_use == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fixture.supervisor.loaded()[0].slots_in_use,
        0,
        "the slot was released"
    );
    assert!(!fixture.data_dir.join("local-benchmarks.json").exists());

    // The daemon is not left blocked: a new run can start and finish.
    fixture.behaviour.stall.store(false, Ordering::SeqCst);
    fixture.call("models.bench", json!({"model": "tiny"})).await;
    for _ in 0..200 {
        if fixture.results().await["runs"][1]["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let results = fixture.results().await;
    assert_eq!(results["runs"][1]["state"], "done", "{results}");
    assert_eq!(results["results"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unknown_model_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path()).await;
    let refused = fixture
        .outcome("models.bench", json!({"model": "nonesuch"}))
        .await;
    assert!(matches!(refused, Outcome::Error(_)), "{refused:?}");
    assert!(
        fixture.results().await["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
