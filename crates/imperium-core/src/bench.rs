//! The local model benchmark: `models.bench` loads each chosen model through the same launch and
//! residency path every call uses (a background call, so it waits for a quiet machine and never
//! evicts a model in use), sends a small fixed prompt set, and measures load time, time to first
//! token, prompt-processing speed and generation speed from the real stream. Results are saved
//! per model, hardware fingerprint and date in `local-benchmarks.json` in the data folder and
//! listed by `models.bench_results`. A run is a background task that `models.bench_cancel`
//! aborts: dropping it releases the slot at once. Nothing here runs unless a client asks.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use imperium_api::{LocalModels, LocalRequest, LocalWait, Priority, holder};
use imperium_local::hardware::Hardware;
use imperium_local::monitor::{Probe, ProcessSample, SystemProbe};
use imperium_local::residency::Action;
use imperium_models::DataLines;
use imperium_protocol::jsonrpc::{ErrorObject, codes};
use imperium_protocol::methods::{
    BenchPrompt, BenchResult, BenchRun, MODEL_BENCH_PROGRESS, ModelBenchParams,
    ModelBenchResultsParams, ModelBenchResultsResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::task::{AbortHandle, JoinHandle};

use crate::DaemonState;

/// Where results are kept, in the data folder.
const STORE_FILE: &str = "local-benchmarks.json";
/// Results kept; the oldest are dropped past this.
const KEEP_RESULTS: usize = 200;
/// How often the run's step is refreshed from the runtime while a model loads or waits.
const STEP_INTERVAL: Duration = Duration::from_millis(400);
/// How often the server process's memory is read.
const MEMORY_INTERVAL: Duration = Duration::from_secs(1);
/// Longest a model may take to begin answering when its `max_wait_s` is not set.
const DEFAULT_FIRST_TOKEN_LIMIT: Duration = Duration::from_secs(600);
/// Longest a started answer may go without a token.
const IDLE_LIMIT: Duration = Duration::from_secs(120);
/// Characters counted as a token when a prompt's size is estimated for the reservation.
const CHARS_PER_TOKEN: usize = 4;

/// One prompt of the fixed set.
struct Prompt {
    name: &'static str,
    /// How many times [`PASSAGE`] is repeated before the instruction; 0 for none.
    passages: usize,
    instruction: &'static str,
    max_tokens: u32,
}

/// A passage of plain prose, repeated to make the longer prompts. Fixed, so results from
/// different days compare.
const PASSAGE: &str = "A harbour town keeps its ledger in three columns: what arrived by sea, \
what left by road, and what was promised but never came. The clerk, a patient woman named Ilsa, \
checks each entry against the tide tables, because a ship that was late for the tide was late \
for everything else. In spring the cargoes are timber and salt; in autumn they are wool, \
barrels of pickled fish, and the occasional crate of glass wrapped in straw. Merchants argue \
over weights, captains argue over berths, and the harbour master argues with the weather, \
which does not answer. Ilsa writes it all down in the same careful hand, and when the ledger \
is full she copies it fair and sends the old one to the cellar, where the damp keeps it honest. \n\n";

/// The fixed prompt set: a short question, a paragraph-sized document, and a long one. The
/// long one is left out for a model whose context is too small for it.
const PROMPTS: [Prompt; 3] = [
    Prompt {
        name: "short",
        passages: 0,
        instruction: "Explain in two sentences why the sky is blue.",
        max_tokens: 96,
    },
    Prompt {
        name: "medium",
        passages: 2,
        instruction: "Summarise the text above in three sentences.",
        max_tokens: 128,
    },
    Prompt {
        name: "long",
        passages: 10,
        instruction: "Summarise the text above in three sentences.",
        max_tokens: 128,
    },
];

impl Prompt {
    fn text(&self) -> String {
        format!("{}{}", PASSAGE.repeat(self.passages), self.instruction)
    }

    /// The prompt's size in tokens, estimated, for the runtime's reservation.
    fn estimate(&self) -> u32 {
        let chars = self.text().len();
        u32::try_from(chars.div_ceil(CHARS_PER_TOKEN)).unwrap_or(u32::MAX)
    }
}

/// Benchmark runs since the daemon started.
#[derive(Debug, Default)]
pub struct Bench {
    runs: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    /// Serialises reads and writes of the results file.
    store: Mutex<()>,
}

#[derive(Debug)]
struct Entry {
    run: BenchRun,
    /// Stops the run's task; `None` once it has ended.
    abort: Option<AbortHandle>,
}

impl Bench {
    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        // Every update is a single assignment, so the list stays consistent across a panic.
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every run, oldest first.
    pub fn runs(&self) -> Vec<BenchRun> {
        self.lock().iter().map(|entry| entry.run.clone()).collect()
    }

    /// Stops a running benchmark: its task is aborted, which drops the slot it holds. False when
    /// no running benchmark has that id.
    pub fn cancel(&self, state: &DaemonState, run_id: &str) -> bool {
        let run = {
            let mut runs = self.lock();
            let Some(entry) = runs
                .iter_mut()
                .find(|entry| entry.run.run_id == run_id && entry.run.state == "running")
            else {
                return false;
            };
            if let Some(abort) = entry.abort.take() {
                abort.abort();
            }
            entry.run.state = "cancelled".into();
            entry.run.step = None;
            entry.run.message = Some("Stopped before it finished.".into());
            entry.run.clone()
        };
        state.announce(MODEL_BENCH_PROGRESS, run);
        true
    }

    /// Runs `change` on a run, announcing the result when it returns true.
    fn update(&self, state: &DaemonState, run_id: &str, change: impl FnOnce(&mut BenchRun)) {
        let run = {
            let mut runs = self.lock();
            let Some(entry) = runs
                .iter_mut()
                .find(|entry| entry.run.run_id == run_id && entry.run.state == "running")
            else {
                return;
            };
            change(&mut entry.run);
            entry.run.clone()
        };
        state.announce(MODEL_BENCH_PROGRESS, run);
    }
}

fn invalid(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(codes::INVALID_PARAMS, message.into())
}

// ---- the saved results ----

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    results: Vec<BenchResult>,
}

fn store_path(state: &DaemonState) -> PathBuf {
    state.data_dir().join(STORE_FILE)
}

fn read_saved(path: &Path) -> Vec<BenchResult> {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Saved>(&bytes) {
            Ok(saved) => saved.results,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "benchmark results could not be read");
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

/// Appends `result` to the results file, keeping the newest [`KEEP_RESULTS`].
fn save(state: &DaemonState, result: &BenchResult) -> std::io::Result<()> {
    let bench = state.bench();
    let _guard = bench.store.lock().unwrap_or_else(PoisonError::into_inner);
    let path = store_path(state);
    let mut results = read_saved(&path);
    results.push(result.clone());
    let excess = results.len().saturating_sub(KEEP_RESULTS);
    results.drain(..excess);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&Saved { results }).map_err(std::io::Error::other)?;
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, &path)
}

/// `models.bench_results`.
pub fn results(state: &DaemonState, params: ModelBenchResultsParams) -> ModelBenchResultsResult {
    let saved = {
        let _guard = state
            .bench()
            .store
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        read_saved(&store_path(state))
    };
    let mut results: Vec<BenchResult> = saved
        .into_iter()
        .filter(|result| {
            params
                .model
                .as_ref()
                .is_none_or(|id| &result.model_id == id)
        })
        .collect();
    results.reverse();
    ModelBenchResultsResult {
        hardware_fingerprint: current_fingerprint(state).0,
        results,
        runs: state.bench().runs(),
    }
}

// ---- the hardware fingerprint ----

/// A short hash of the hardware and runtime pack the numbers belong to, and the same in words.
pub(crate) fn fingerprint(hardware: &Hardware, pack: &str) -> (String, String) {
    const GIB: u64 = 1024 * 1024 * 1024;
    let mut gpus: Vec<String> = hardware
        .gpus
        .iter()
        .map(|gpu| format!("{} {} GB", gpu.name, gpu.vram_total_bytes.div_ceil(GIB)))
        .collect();
    gpus.sort();
    let mut parts = if gpus.is_empty() {
        vec!["no GPU".to_string()]
    } else {
        gpus
    };
    let ram = hardware.ram_total_bytes.div_ceil(GIB);
    let memory = if hardware.unified_memory {
        format!("{ram} GB unified memory")
    } else {
        format!("{ram} GB RAM")
    };
    parts.push(memory);
    parts.push(format!("{} cores", hardware.cpu_cores));
    parts.push(format!("llama.cpp {pack}"));
    let label = parts.join(", ");
    let digest = Sha256::digest(label.as_bytes());
    let hash: String = digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (hash, label)
}

fn current_fingerprint(state: &DaemonState) -> (String, String) {
    match state.local_runtime() {
        Some(local) => fingerprint(local.hardware(), &local.server().0),
        None => ("none".into(), "no local models".into()),
    }
}

// ---- starting a run ----

/// `models.bench`: chooses the models, starts the run, and returns it.
pub fn start(state: Arc<DaemonState>, params: ModelBenchParams) -> Result<BenchRun, ErrorObject> {
    if state.local_runtime().is_none() {
        return Err(invalid(
            "No local models are set up; add a model folder first.",
        ));
    }
    let listing = state.local_models();
    let models: Vec<String> = match params.model.as_deref().map(str::trim) {
        Some(wanted) if !wanted.is_empty() => {
            let id = wanted
                .strip_prefix("llamacpp:")
                .or_else(|| wanted.strip_prefix("vllm:"))
                .unwrap_or(wanted);
            let Some(row) = listing.models.iter().find(|row| row.id == id) else {
                return Err(invalid(format!(
                    "no local model {wanted:?} in the model folders; see models.local"
                )));
            };
            if let Some(problem) = &row.problem {
                return Err(invalid(format!("{} cannot run here: {problem}", row.id)));
            }
            vec![row.id.clone()]
        }
        _ => listing
            .models
            .iter()
            .filter(|row| row.problem.is_none())
            .map(|row| row.id.clone())
            .collect(),
    };
    if models.is_empty() {
        return Err(invalid("There are no local models that can run here."));
    }
    let bench = state.bench();
    let run = {
        let mut runs = bench.lock();
        if runs.iter().any(|entry| entry.run.state == "running") {
            return Err(invalid(
                "A benchmark is already running; wait for it or cancel it.",
            ));
        }
        let number = bench.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let run = BenchRun {
            run_id: format!("bench-{number}"),
            state: "running".into(),
            models: models.clone(),
            current: None,
            step: Some("starting".into()),
            models_done: 0,
            results: Vec::new(),
            message: None,
        };
        runs.push(Entry {
            run: run.clone(),
            abort: None,
        });
        run
    };
    state.announce(MODEL_BENCH_PROGRESS, &run);
    let task = tokio::spawn(run_all(
        Arc::clone(&state),
        run.run_id.clone(),
        models,
        params.cold.unwrap_or(true),
    ));
    bench.update_entry(&run.run_id, |entry| match entry.run.state.as_str() {
        "running" => entry.abort = Some(task.abort_handle()),
        // Cancelled before the handle was stored.
        _ => task.abort(),
    });
    Ok(run)
}

impl Bench {
    fn update_entry(&self, run_id: &str, change: impl FnOnce(&mut Entry)) {
        let mut runs = self.lock();
        if let Some(entry) = runs.iter_mut().find(|entry| entry.run.run_id == run_id) {
            change(entry);
        }
    }
}

/// Measures each model in turn, one failing without stopping the others.
async fn run_all(state: Arc<DaemonState>, run_id: String, models: Vec<String>, cold: bool) {
    let bench = state.bench();
    for id in &models {
        bench.update(&state, &run_id, |run| {
            run.current = Some(id.clone());
            run.step = Some("starting".into());
        });
        let outcome = measure_model(&state, &run_id, id, cold).await;
        let result = match outcome {
            Ok(result) => {
                if let Err(error) = save(&state, &result) {
                    tracing::warn!(%error, "benchmark result could not be saved");
                }
                result
            }
            Err(error) => {
                tracing::warn!(model = %id, %error, "local model benchmark failed");
                failed_result(&state, &run_id, id, error)
            }
        };
        bench.update(&state, &run_id, |run| {
            run.results.push(result);
            run.models_done += 1;
        });
    }
    bench.update_entry(&run_id, |entry| {
        if entry.run.state == "running" {
            entry.abort = None;
            entry.run.state = "done".into();
            entry.run.current = None;
            entry.run.step = None;
            state.announce(MODEL_BENCH_PROGRESS, entry.run.clone());
        }
    });
}

fn failed_result(state: &DaemonState, run_id: &str, model: &str, error: String) -> BenchResult {
    let (fingerprint, hardware) = current_fingerprint(state);
    BenchResult {
        model_id: model.to_string(),
        run_id: run_id.to_string(),
        date: now(),
        hardware_fingerprint: fingerprint,
        hardware,
        runtime: String::new(),
        placement: String::new(),
        machine: state.load_view().level().label().into(),
        load_s: None,
        ttft_ms: None,
        prompt_tok_s: None,
        gen_tok_s: None,
        peak_ram_bytes: None,
        peak_vram_bytes: None,
        prompts: Vec::new(),
        error: Some(error),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---- measuring one model ----

fn wait_text(wait: Option<LocalWait>) -> &'static str {
    match wait {
        Some(LocalWait::Loading) => "loading the model",
        Some(LocalWait::GrowingPool) => "waiting for the model to restart with more memory",
        Some(LocalWait::KvRoom) => "waiting for room in the model's memory",
        Some(LocalWait::Slot) => "waiting for a free slot",
        Some(LocalWait::OtherModel) => "waiting for another model",
        Some(LocalWait::MakingRoom) => "waiting for memory to be freed",
        Some(LocalWait::SavingContext | LocalWait::RestoringContext) => "moving an agent's context",
        None => "waiting for a quiet machine",
    }
}

async fn measure_model(
    state: &Arc<DaemonState>,
    run_id: &str,
    id: &str,
    cold: bool,
) -> Result<BenchResult, String> {
    let local = state.local_runtime().ok_or("no local models are set up")?;
    let supervisor = Arc::clone(&local.supervisor);
    let bench = state.bench();
    let machine = state.load_view().level().label().to_string();
    let row = state
        .local_models()
        .models
        .into_iter()
        .find(|row| row.id == id)
        .ok_or_else(|| format!("{id} is no longer in the model folders"))?;

    // A cold start is wanted to time the load: an idle model is unloaded first, by the
    // residency controller, which refuses while anything uses it (then the load is not timed).
    let was_loaded = supervisor
        .loaded()
        .iter()
        .any(|loaded| loaded.model_id == id);
    if cold && was_loaded {
        bench.update(state, run_id, |run| {
            run.step = Some("unloading the idle model to time its load".into());
        });
        let _ = state.unload_local_model(id).await;
    }
    let loaded_before = supervisor
        .loaded()
        .iter()
        .any(|loaded| loaded.model_id == id);

    let usable: Vec<&Prompt> = {
        let context = supervisor.planned_context(id);
        PROMPTS
            .iter()
            .filter(|prompt| {
                context.is_none_or(|context| {
                    u64::from(prompt.estimate() + prompt.max_tokens) * 2 <= context
                })
            })
            .collect()
    };
    let reserve = usable
        .iter()
        .map(|prompt| prompt.estimate())
        .max()
        .unwrap_or(64);

    let mut moves = supervisor.moves();
    let caller = holder("bench", run_id);
    let request = LocalRequest {
        priority: Priority::Background,
        prompt_tokens: Some(reserve),
        uncapped: false,
    };
    let acquiring = supervisor.acquire_with(id, &caller, request);
    tokio::pin!(acquiring);
    let lease = loop {
        tokio::select! {
            acquired = &mut acquiring => break acquired.map_err(|error| {
                format!("could not load {id}: {error}")
            })?,
            () = tokio::time::sleep(STEP_INTERVAL) => {
                let text = wait_text(supervisor.wait_reason(&caller));
                bench.update(state, run_id, |run| {
                    if run.step.as_deref() != Some(text) {
                        run.step = Some(text.into());
                    }
                });
            }
        }
    };

    // The load the call made, from the runtime's own record of it.
    let mut load_s = None;
    loop {
        match moves.try_recv() {
            Ok(done) => {
                if done.ok && matches!(&done.action, Action::Load { model } if model == id) {
                    load_s = Some(load_s.unwrap_or(0.0) + done.seconds);
                }
            }
            Err(TryRecvError::Lagged(_)) => {}
            Err(TryRecvError::Empty | TryRecvError::Closed) => break,
        }
    }
    if loaded_before {
        // It was loaded and in use, so nothing was timed.
        load_s = None;
    }

    let status = supervisor
        .loaded()
        .into_iter()
        .find(|loaded| loaded.model_id == id);
    let placement = match &status {
        Some(loaded) if loaded.all_layers_on_gpu => "gpu",
        Some(loaded) if loaded.gpu_layers > 0 => "partial",
        _ => "cpu",
    };
    let memory = status
        .as_ref()
        .and_then(|loaded| loaded.pid)
        .map(MemoryWatch::start);

    let first_limit = match supervisor.launch(id).max_wait_s {
        0 => DEFAULT_FIRST_TOKEN_LIMIT,
        seconds => Duration::from_secs(seconds),
    };
    let http = reqwest::Client::new();
    let url = format!("{}/chat/completions", lease.base_url.trim_end_matches('/'));
    let mut measured = Vec::new();
    let count = usable.len();
    for (index, prompt) in usable.into_iter().enumerate() {
        bench.update(state, run_id, |run| {
            run.step = Some(format!("prompt {} of {count} ({})", index + 1, prompt.name));
        });
        let mut body = json!({
            "model": id,
            "messages": [{"role": "user", "content": prompt.text()}],
            "max_tokens": lease
                .max_tokens
                .map_or(prompt.max_tokens, |cap| cap.min(prompt.max_tokens)),
            "temperature": 0,
            "seed": 1,
            "stream": true,
            "stream_options": {"include_usage": true},
            "cache_prompt": false,
        });
        if row.runtime == "llamacpp" {
            if let Some(slot) = lease.slot {
                body["id_slot"] = json!(slot);
            }
            // A thinking model answers at once, so the numbers are of the same kind for all.
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        let streamed = stream_once(&http, &url, &body, first_limit, IDLE_LIMIT)
            .await
            .map_err(|error| format!("the {} prompt failed: {error}", prompt.name))?;
        let prompt_tokens = streamed.prompt_tokens.unwrap_or_else(|| prompt.estimate());
        let completion_tokens = streamed.completion_tokens.unwrap_or(streamed.chunks);
        measured.push(BenchPrompt {
            name: prompt.name.into(),
            prompt_tokens,
            completion_tokens,
            ttft_ms: streamed.ttft.as_secs_f64() * 1000.0,
            prompt_tok_s: rate(u64::from(prompt_tokens), streamed.ttft),
            gen_tok_s: rate(
                u64::from(completion_tokens.saturating_sub(1)),
                streamed.generating,
            ),
        });
    }
    drop(lease);
    let peak = memory.map(MemoryWatch::finish).unwrap_or_default();
    let (ttft_ms, prompt_tok_s, gen_tok_s) = summarise(&measured);
    let (fingerprint, hardware) = current_fingerprint(state);
    Ok(BenchResult {
        model_id: id.to_string(),
        run_id: run_id.to_string(),
        date: now(),
        hardware_fingerprint: fingerprint,
        hardware,
        runtime: row.runtime,
        placement: placement.into(),
        machine,
        load_s,
        ttft_ms,
        prompt_tok_s,
        gen_tok_s,
        peak_ram_bytes: peak.ram,
        peak_vram_bytes: peak.vram,
        prompts: measured,
        error: None,
    })
}

// ---- the stream ----

/// What one streamed answer showed.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Streamed {
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Chunks that carried text, which stands in for the token count when the server reports
    /// no usage (llama.cpp sends one token per chunk).
    pub chunks: u32,
    /// From sending the request to the first generated text.
    pub ttft: Duration,
    /// From the first generated text to the last.
    pub generating: Duration,
}

/// Sends one streamed chat completion and times it: the time to the first text, and the time
/// from the first text to the last. `first_limit` bounds the wait for the first text, `idle`
/// the gap between later chunks.
pub(crate) async fn stream_once(
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    first_limit: Duration,
    idle: Duration,
) -> Result<Streamed, String> {
    let sent = Instant::now();
    let response = tokio::time::timeout(first_limit, http.post(url).json(body).send())
        .await
        .map_err(|_| format!("no answer within {} s", first_limit.as_secs()))?
        .map_err(|error| error.without_url().to_string())?;
    let status = response.status();
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        let detail: String = text.chars().take(300).collect();
        return Err(format!("the server answered {status}: {detail}"));
    }
    let mut chunks = Box::pin(response.bytes_stream());
    let mut lines = DataLines::default();
    let mut streamed = Streamed::default();
    let mut first: Option<Instant> = None;
    let mut last: Option<Instant> = None;
    'read: loop {
        let limit = if first.is_none() {
            first_limit.saturating_sub(sent.elapsed())
        } else {
            idle
        };
        let chunk = match tokio::time::timeout(limit, chunks.next()).await {
            Err(_) => return Err("the model stopped answering".into()),
            Ok(None) => break,
            Ok(Some(Err(error))) => return Err(error.without_url().to_string()),
            Ok(Some(Ok(chunk))) => chunk,
        };
        let arrived = Instant::now();
        for payload in lines.push(&chunk) {
            if payload == "[DONE]" {
                break 'read;
            }
            let Ok(value) = serde_json::from_str::<Value>(&payload) else {
                continue;
            };
            if let Some(tokens) = value["usage"]["prompt_tokens"].as_u64() {
                streamed.prompt_tokens = u32::try_from(tokens).ok();
            }
            if let Some(tokens) = value["usage"]["completion_tokens"].as_u64() {
                streamed.completion_tokens = u32::try_from(tokens).ok();
            }
            let wrote = value["choices"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|choice| {
                    ["content", "reasoning_content", "reasoning"]
                        .iter()
                        .any(|key| {
                            choice["delta"][key]
                                .as_str()
                                .is_some_and(|text| !text.is_empty())
                        })
                });
            if wrote {
                streamed.chunks += 1;
                first.get_or_insert(arrived);
                last = Some(arrived);
            }
        }
    }
    let (Some(first), Some(last)) = (first, last) else {
        return Err("the model produced no text".into());
    };
    streamed.ttft = first.duration_since(sent);
    streamed.generating = last.duration_since(first);
    Ok(streamed)
}

/// `count` per second over `time`, when both are positive.
fn rate(count: u64, time: Duration) -> Option<f64> {
    (count > 0 && !time.is_zero()).then(|| count as f64 / time.as_secs_f64())
}

/// The summary of a model's prompts: median time to first token; prompt tokens over the time
/// spent before the first token; generated tokens (after the first) over the time spent
/// generating them.
pub(crate) fn summarise(prompts: &[BenchPrompt]) -> (Option<f64>, Option<f64>, Option<f64>) {
    let mut ttfts: Vec<f64> = prompts.iter().map(|prompt| prompt.ttft_ms).collect();
    ttfts.sort_by(f64::total_cmp);
    let median = match ttfts.len() {
        0 => None,
        n if n % 2 == 1 => Some(ttfts[n / 2]),
        n => Some((ttfts[n / 2 - 1] + ttfts[n / 2]) / 2.0),
    };
    let prompt_tokens: f64 = prompts.iter().map(|p| f64::from(p.prompt_tokens)).sum();
    let prompt_seconds: f64 = prompts.iter().map(|p| p.ttft_ms / 1000.0).sum();
    let (mut generated, mut generating) = (0.0, 0.0);
    for prompt in prompts {
        if let Some(speed) = prompt.gen_tok_s.filter(|speed| *speed > 0.0) {
            let tokens = f64::from(prompt.completion_tokens.saturating_sub(1));
            generated += tokens;
            generating += tokens / speed;
        }
    }
    (
        median,
        (prompt_seconds > 0.0 && prompt_tokens > 0.0).then(|| prompt_tokens / prompt_seconds),
        (generating > 0.0).then(|| generated / generating),
    )
}

// ---- memory ----

/// The most memory the model's server process held.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Peak {
    pub ram: Option<u64>,
    pub vram: Option<u64>,
}

impl Peak {
    pub(crate) fn add(&mut self, process: &ProcessSample) {
        self.ram = self.ram.max(Some(process.ram_bytes));
        if let Some(vram) = process.vram_bytes {
            self.vram = self.vram.max(Some(vram));
        }
    }
}

/// Reads the server process's memory every [`MEMORY_INTERVAL`] while a model is measured.
struct MemoryWatch {
    peak: Arc<Mutex<Peak>>,
    task: JoinHandle<()>,
}

impl MemoryWatch {
    fn start(pid: u32) -> Self {
        let peak = Arc::new(Mutex::new(Peak::default()));
        let shared = Arc::clone(&peak);
        let task = tokio::spawn(async move {
            let mut probe = SystemProbe::new();
            loop {
                let Ok((returned, sample)) = tokio::task::spawn_blocking(move || {
                    let sample = probe.sample(&[pid]);
                    (probe, sample)
                })
                .await
                else {
                    return;
                };
                probe = returned;
                if let Some(process) = sample.processes.iter().find(|process| process.pid == pid) {
                    shared
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .add(process);
                }
                tokio::time::sleep(MEMORY_INTERVAL).await;
            }
        });
        MemoryWatch { peak, task }
    }

    fn finish(self) -> Peak {
        *self.peak.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for MemoryWatch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_local::hardware::Gpu;

    fn prompt(ttft_ms: f64, prompt_tokens: u32, completion_tokens: u32, speed: f64) -> BenchPrompt {
        BenchPrompt {
            name: "x".into(),
            prompt_tokens,
            completion_tokens,
            ttft_ms,
            prompt_tok_s: None,
            gen_tok_s: Some(speed),
        }
    }

    #[test]
    fn the_summary_is_a_median_and_two_weighted_rates() {
        let prompts = [
            prompt(100.0, 100, 11, 50.0),
            prompt(300.0, 600, 11, 25.0),
            prompt(200.0, 300, 11, 100.0),
        ];
        let (ttft, prompt_rate, gen_rate) = summarise(&prompts);
        assert_eq!(ttft, Some(200.0));
        // 1000 tokens over 0.6 s.
        assert!((prompt_rate.unwrap() - 1000.0 / 0.6).abs() < 1e-6);
        // 30 tokens over 10/50 + 10/25 + 10/100 = 0.7 s.
        assert!((gen_rate.unwrap() - 30.0 / 0.7).abs() < 1e-6);
        assert_eq!(summarise(&[]), (None, None, None));
    }

    #[test]
    fn the_fingerprint_follows_the_machine_not_its_free_memory() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let mut hardware = Hardware {
            gpus: vec![Gpu {
                name: "RTX".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 24 * GIB,
                vram_free_bytes: 20 * GIB,
                driver: None,
            }],
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: 30 * GIB,
            cpu_cores: 16,
            unified_memory: false,
            overridden: false,
        };
        let (first, label) = fingerprint(&hardware, "cuda12");
        assert_eq!(label, "RTX 24 GB, 64 GB RAM, 16 cores, llama.cpp cuda12");
        hardware.gpus[0].vram_free_bytes = 2 * GIB;
        hardware.ram_available_bytes = GIB;
        assert_eq!(fingerprint(&hardware, "cuda12").0, first);
        assert_ne!(fingerprint(&hardware, "vulkan").0, first);
        hardware.cpu_cores = 8;
        assert_ne!(fingerprint(&hardware, "cuda12").0, first);
    }

    #[test]
    fn the_peak_keeps_the_highest_reading_of_each_memory() {
        let process = |ram, vram| ProcessSample {
            pid: 1,
            name: "llama-server".into(),
            cpu_percent: 0.0,
            ram_bytes: ram,
            vram_bytes: vram,
        };
        let mut peak = Peak::default();
        peak.add(&process(5, None));
        peak.add(&process(9, Some(7)));
        peak.add(&process(6, Some(3)));
        assert_eq!(
            peak,
            Peak {
                ram: Some(9),
                vram: Some(7)
            }
        );
    }

    #[test]
    fn the_fixed_prompts_grow_and_the_set_does_not_change() {
        let sizes: Vec<u32> = PROMPTS.iter().map(Prompt::estimate).collect();
        assert!(
            sizes[0] < 30 && sizes[0] < sizes[1] && sizes[1] * 3 < sizes[2],
            "{sizes:?}"
        );
        assert_eq!(
            PROMPTS.iter().map(|p| p.name).collect::<Vec<_>>(),
            ["short", "medium", "long"]
        );
    }
}
