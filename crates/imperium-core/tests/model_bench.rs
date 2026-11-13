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
    /// Sends no usage report.
    no_usage: AtomicBool,
    /// Answers every request with a server error.
    fail: AtomicBool,
    /// Milliseconds the fake launcher takes to "start" a model.
    launch_ms: Arc<AtomicU64>,
    /// The bodies received.
    received: Mutex<Vec<Value>>,
}

const TOKENS: u64 = 5;
/// The fake model's schedule: 400 ms to the first token, then 150 ms between tokens.
const FIRST_MS: u64 = 400;
const GAP_MS: u64 = 150;
/// How long the fake launcher takes to "start" a model.
const LAUNCH_MS: u64 = 300;
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
        if behaviour.fail.load(Ordering::SeqCst) {
            return Response::builder()
                .status(500)
                .body(Body::from("the model crashed"))
                .unwrap();
        }
        let no_usage = behaviour.no_usage.load(Ordering::SeqCst);
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
        if !no_usage {
            items.push((
                Duration::ZERO,
                event(json!({"choices": [], "usage": {
                    "prompt_tokens": PROMPT_TOKENS, "completion_tokens": TOKENS}})),
            ));
        }
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
struct FakeLauncher(String, Arc<AtomicU64>);

struct FakeServer(String);

impl Server for FakeServer {
    fn base_url(&self) -> &str {
        &self.0
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, _spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(self.1.load(Ordering::SeqCst))).await;
            Ok(Box::new(FakeServer(self.0.clone())) as Box<dyn Server>)
        })
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

async fn fixture(home: &Path, extra: &str) -> Fixture {
    let (url, behaviour) = fake_model_server().await;
    behaviour.first_ms.store(FIRST_MS, Ordering::SeqCst);
    behaviour.gap_ms.store(GAP_MS, Ordering::SeqCst);
    behaviour.launch_ms.store(LAUNCH_MS, Ordering::SeqCst);
    let config = format!(
        "[models]\ndefault = \"llamacpp:tiny\"\n[workflow]\nenabled = false\n[memory]\nenabled = false\n{extra}"
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
        vram_overridden: false,
    };
    let supervisor = Supervisor::start(
        vec![tiny_model()],
        settings.runtime.llamacpp.clone(),
        Runtimes::default(),
        &hardware,
        Arc::new(FakeLauncher(url, Arc::clone(&behaviour.launch_ms))),
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

    /// Waits for the last run to leave `running`; returns it.
    async fn finished_run(&mut self) -> Value {
        for _ in 0..300 {
            let results = self.results().await;
            let run = results["runs"].as_array().unwrap().last().unwrap().clone();
            if run["state"] != "running" {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the benchmark did not finish");
    }

    /// Starts a benchmark of "tiny" and waits for it to end.
    async fn bench(&mut self, params: Value) -> Value {
        self.call("models.bench", params).await;
        self.finished_run().await
    }
}

fn saved_result(index: usize) -> Value {
    json!({"model_id": format!("old-{index}"), "run_id": "bench-0", "date": "2026-01-01T00:00:00Z",
           "hardware_fingerprint": "old", "hardware": "old", "runtime": "llamacpp",
           "placement": "gpu", "machine": "quiet", "prompts": []})
}

/// Whether a result's numbers are within what the fake server's schedule allows, tightly: a
/// formula over the whole request would give about 100 tok/s for the prompt and 5 tok/s for the
/// reply, outside these ranges.
fn timing_in_range(result: &Value) -> bool {
    let within = |value: &Value, low: f64, high: f64| {
        value
            .as_f64()
            .is_some_and(|number| (low..=high).contains(&number))
    };
    within(&result["ttft_ms"], 395.0, 700.0)
        && within(&result["prompt_tok_s"], 150.0, 256.0)
        && within(&result["gen_tok_s"], 5.6, 6.7)
        && result["prompts"].as_array().is_some_and(|prompts| {
            prompts.iter().all(|prompt| {
                within(&prompt["ttft_ms"], 395.0, 700.0)
                    && within(&prompt["prompt_tok_s"], 150.0, 256.0)
                    && within(&prompt["gen_tok_s"], 5.6, 6.7)
            })
        })
}

fn range(value: &Value, low: f64, high: f64) -> f64 {
    let number = value
        .as_f64()
        .unwrap_or_else(|| panic!("{value} is not a number"));
    assert!(
        (low..=high).contains(&number),
        "{number} is not in {low}..={high}"
    );
    number
}

#[tokio::test]
async fn nothing_runs_until_a_client_asks() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
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
    let mut fixture = fixture(home.path(), "").await;
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
        results["hardware_fingerprints"]
            .as_array()
            .unwrap()
            .contains(&result["hardware_fingerprint"])
    );
    let hardware = result["hardware"].as_str().unwrap();
    assert!(
        hardware.contains("test 24 GB") && hardware.contains("llama.cpp "),
        "{hardware}"
    );
    assert!(result["date"].as_str().unwrap().ends_with('Z'));
    assert_eq!(result["machine"], "quiet");
    // The model was not loaded before, so its load was timed: at least the launcher's delay.
    range(&result["load_s"], LAUNCH_MS as f64 / 1000.0 - 0.01, 10.0);

    // The numbers follow the schedule the server streamed: 400 ms to the first token, then four
    // more at 150 ms. Sleeps only run long, so the speeds are at most these; a formula over the
    // whole request would give 100/1.0 = 100 tok/s and 5/1.0 = 5 tok/s, outside these ranges.
    let prompts = result["prompts"].as_array().unwrap();
    assert_eq!(
        prompts
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["short", "medium", "long"]
    );
    for prompt in prompts {
        assert_eq!(prompt["prompt_tokens"], PROMPT_TOKENS);
        assert_eq!(prompt["prompt_tokens_estimated"], false);
        assert_eq!(prompt["completion_tokens"], TOKENS);
        // Sleeps only run long: nothing is quicker than the schedule.
        assert!(prompt["ttft_ms"].as_f64().unwrap() >= 395.0, "{prompt}");
        assert!(prompt["gen_tok_s"].as_f64().unwrap() <= 6.7, "{prompt}");
        assert!(
            prompt["prompt_tok_s"].as_f64().unwrap() <= 256.0,
            "{prompt}"
        );
    }
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

    // A second run at once: the model was used just now, inside the interactive hold, so it is
    // not unloaded to time a load, and the results list is newest first.
    let second = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(second["state"], "done");
    let results = fixture.results().await;
    let saved = results["results"].as_array().unwrap();
    assert_eq!(saved.len(), 2, "{results}");
    assert_eq!(saved[0]["run_id"], "bench-2");
    assert_eq!(saved[1]["run_id"], "bench-1");
    assert!(
        saved[0]["load_s"].is_null(),
        "no load was timed: {}",
        saved[0]
    );
}

#[tokio::test]
async fn the_numbers_follow_the_stream_not_the_whole_request() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    // A slow or busy machine can only make the sleeps run long, which the tight ranges mind; so
    // they must hold for one of three runs. A formula over the whole request fails every run.
    let mut latest = Value::Null;
    for _ in 0..3 {
        let run = fixture.bench(json!({"model": "tiny"})).await;
        assert_eq!(run["state"], "done", "{run}");
        latest = run["results"][0].clone();
        if timing_in_range(&latest) {
            return;
        }
    }
    panic!("none of three runs matched the stream's schedule: {latest}");
}

#[tokio::test]
async fn a_saturated_machine_makes_the_benchmark_wait_and_does_not_unload_the_model() {
    use imperium_engine::Load;
    let home = tempfile::tempdir().unwrap();
    // No interactive hold: the model would be unloaded to time its load, were the machine quiet.
    let mut fixture = fixture(home.path(), "[load]\ninteractive_hold_min = 0\n").await;
    fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(fixture.supervisor.loaded().len(), 1);

    let load = imperium_core::ManualLoad::new(Load::Saturated);
    fixture.state.use_load_view(load.clone());
    let mut announced = fixture.state.announcements();
    fixture.call("models.bench", json!({"model": "tiny"})).await;
    // It waits, and says why.
    let mut waiting = false;
    for _ in 0..60 {
        let results = fixture.results().await;
        let run = results["runs"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(run["state"], "running", "{run}");
        if run["step"] == "waiting while the machine is saturated" {
            waiting = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(waiting, "the run never said it was waiting for the machine");
    assert_eq!(
        fixture.supervisor.loaded().len(),
        1,
        "the model was not unloaded while the machine was saturated"
    );
    // Once the machine is quiet again it goes on; the load was not timed.
    load.set(Load::Quiet);
    let run = fixture.finished_run().await;
    assert_eq!(run["state"], "done", "{run}");
    assert!(
        run["results"][0]["load_s"].is_null(),
        "{}",
        run["results"][0]
    );
    loop {
        let note = match announced.try_recv() {
            Ok(note) => note,
            // Missed some: look at the rest.
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        if note.method == "models.bench_progress" {
            assert_ne!(
                note.params["step"], "unloading the idle model to time its load",
                "{:?}",
                note.params
            );
        }
    }
}

#[tokio::test]
async fn a_benchmark_whose_task_panics_is_failed_not_left_running() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    fixture.state.bench().inject_panic();
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "failed", "{run}");
    assert_eq!(run["message"], "The benchmark stopped unexpectedly.");
    assert!(run["current"].is_null() && run["step"].is_null(), "{run}");
    // Nothing was saved, and the next run works.
    assert!(
        fixture.results().await["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
}

#[tokio::test]
async fn a_model_idle_past_the_hold_is_unloaded_to_time_its_load_unless_asked_not_to() {
    let home = tempfile::tempdir().unwrap();
    // No interactive hold: a model counts as idle as soon as it is not in use.
    let mut fixture = fixture(home.path(), "[load]\ninteractive_hold_min = 0\n").await;
    fixture.bench(json!({"model": "tiny"})).await;
    // Cold (the default): unloaded, then loaded again, and the load timed.
    fixture.bench(json!({"model": "tiny"})).await;
    // Warm: left loaded, so no load is timed.
    fixture.bench(json!({"model": "tiny", "cold": false})).await;
    let results = fixture.results().await;
    let saved = results["results"].as_array().unwrap();
    assert_eq!(saved.len(), 3, "{results}");
    // Newest first.
    assert!(saved[0]["load_s"].is_null(), "{}", saved[0]);
    range(&saved[1]["load_s"], LAUNCH_MS as f64 / 1000.0 - 0.01, 10.0);
    range(&saved[2]["load_s"], LAUNCH_MS as f64 / 1000.0 - 0.01, 10.0);
}

#[tokio::test]
async fn a_server_without_usage_claims_no_prompt_speed() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    fixture.behaviour.no_usage.store(true, Ordering::SeqCst);
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
    let result = &run["results"][0];
    assert!(result["prompt_tok_s"].is_null(), "{result}");
    for prompt in result["prompts"].as_array().unwrap() {
        assert!(prompt["prompt_tok_s"].is_null(), "{prompt}");
        // The size is an estimate, and says so.
        assert_eq!(prompt["prompt_tokens_estimated"], true, "{prompt}");
        // The streamed chunks stand in for the reply tokens.
        assert_eq!(prompt["completion_tokens"], TOKENS);
        range(&prompt["gen_tok_s"], 5.6, 6.7);
    }
    range(&result["gen_tok_s"], 5.6, 6.7);
}

#[tokio::test]
async fn a_failed_model_is_reported_not_saved_and_fails_the_run() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    fixture.behaviour.fail.store(true, Ordering::SeqCst);
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["message"]
            .as_str()
            .unwrap()
            .contains("No model could be measured")
    );
    let result = &run["results"][0];
    assert!(
        result["error"].as_str().unwrap().contains("500"),
        "{result}"
    );
    assert!(!fixture.data_dir.join("local-benchmarks.json").exists());
    assert!(
        fixture.results().await["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // The slot was released, and a later run works.
    assert_eq!(fixture.supervisor.loaded()[0].slots_in_use, 0);
    fixture.behaviour.fail.store(false, Ordering::SeqCst);
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
}

#[tokio::test]
async fn only_the_newest_runs_are_kept_in_memory() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    fixture.behaviour.fail.store(true, Ordering::SeqCst);
    for _ in 0..23 {
        fixture.bench(json!({"model": "tiny"})).await;
    }
    let runs = fixture.results().await["runs"].as_array().unwrap().clone();
    assert_eq!(runs.len(), 20);
    assert_eq!(runs.last().unwrap()["run_id"], "bench-23");
    assert_eq!(runs[0]["run_id"], "bench-4");
}

#[tokio::test]
async fn cancelling_during_the_load_stops_the_run() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    fixture.behaviour.launch_ms.store(60_000, Ordering::SeqCst);
    let run = fixture.call("models.bench", json!({"model": "tiny"})).await;
    let run_id = run["run_id"].as_str().unwrap().to_string();
    // The call waits for the model to load.
    for _ in 0..100 {
        let step = fixture.results().await["runs"][0]["step"].clone();
        if step == "loading the model" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fixture.results().await["runs"][0]["step"],
        "loading the model"
    );
    let cancelled = fixture
        .call("models.bench_cancel", json!({"run_id": run_id}))
        .await;
    assert_eq!(cancelled["found"], true);
    let results = fixture.results().await;
    assert_eq!(results["runs"][0]["state"], "cancelled");
    assert!(results["results"].as_array().unwrap().is_empty());
    assert!(
        fixture.behaviour.received.lock().unwrap().is_empty(),
        "nothing was sent"
    );
    assert!(!fixture.data_dir.join("local-benchmarks.json").exists());
}

#[tokio::test]
async fn cancelling_stops_the_run_and_frees_the_slot() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
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
    // Not even a result that finishes after the cancel is saved.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!fixture.data_dir.join("local-benchmarks.json").exists());

    // The daemon is not left blocked: a new run can start and finish.
    fixture.behaviour.stall.store(false, Ordering::SeqCst);
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(
        fixture.results().await["results"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn a_restricted_connection_cannot_benchmark_or_hear_about_it() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let mut phone = Connection::restricted(Arc::clone(&fixture.state), vec!["session.list"]);
    let handshake = json!({"jsonrpc": "2.0", "id": 1, "method": "system.handshake",
        "params": {"client": {"name": "phone", "version": "0"},
                   "protocol": imperium_protocol::PROTOCOL_VERSION}});
    phone.handle_text(&handshake.to_string());
    for method in [
        "models.bench",
        "models.bench_results",
        "models.bench_cancel",
    ] {
        let frame = json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": {"run_id": "x"}});
        let handled = phone.handle_text(&frame.to_string());
        let RpcResponse { outcome, .. } = handled.response.expect("an immediate answer");
        assert!(
            matches!(outcome, Outcome::Error(_)),
            "{method}: {outcome:?}"
        );
    }
    assert!(
        fixture.results().await["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // Progress (model ids, error text) goes to a full connection and not to the phone.
    let progress = imperium_protocol::jsonrpc::Notification::new(
        "models.bench_progress",
        json!({"run_id": "bench-1"}),
    );
    assert!(fixture.connection.receives(&progress));
    assert!(!phone.receives(&progress));
    let other = imperium_protocol::jsonrpc::Notification::new("model.residency", json!({}));
    assert!(phone.receives(&other));
}

#[tokio::test]
async fn results_are_capped_at_200_and_the_oldest_go_first() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    let old: Vec<Value> = (0..200).map(saved_result).collect();
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    std::fs::write(
        fixture.data_dir.join("local-benchmarks.json"),
        json!({"results": old}).to_string(),
    )
    .unwrap();
    assert_eq!(
        fixture.results().await["results"].as_array().unwrap().len(),
        200
    );
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
    let results = fixture.results().await;
    let saved = results["results"].as_array().unwrap();
    assert_eq!(saved.len(), 200);
    assert_eq!(saved[0]["model_id"], "tiny", "newest first");
    assert_eq!(saved[199]["model_id"], "old-1", "old-0 was dropped");
}

#[tokio::test]
async fn an_unreadable_results_file_is_set_aside_not_overwritten() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
    std::fs::create_dir_all(&fixture.data_dir).unwrap();
    let file = fixture.data_dir.join("local-benchmarks.json");
    std::fs::write(&file, "{ \"results\": [ this is not json").unwrap();
    // Listing works and shows nothing; the file is set aside.
    assert!(
        fixture.results().await["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let aside = fixture.data_dir.join("local-benchmarks.json.bad");
    assert_eq!(
        std::fs::read_to_string(&aside).unwrap(),
        "{ \"results\": [ this is not json"
    );
    // Damage that appears after that is kept too when the next result is saved.
    std::fs::write(&file, "garbage again").unwrap();
    let run = fixture.bench(json!({"model": "tiny"})).await;
    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(std::fs::read_to_string(&aside).unwrap(), "garbage again");
    assert_eq!(
        fixture.results().await["results"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn an_unknown_model_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let mut fixture = fixture(home.path(), "").await;
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
