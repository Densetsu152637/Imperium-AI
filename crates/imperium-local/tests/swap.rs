//! Stage 2d: subagent-first model swapping on a GPU that holds one model at a time. An
//! orchestrator waiting for its subagents is swapped out with its slots saved, the subagents
//! run, and the orchestrator comes back with its slots restored. llama-server is replaced by
//! in-process fakes that record what they were asked to do; agents are played by the test
//! through the same calls the agent loop makes (attach for a turn, waiting in `await_tasks`, a
//! lease per model call).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use imperium_api::{Availability, LocalModels};
use imperium_config::LlamaCppSettings;
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchError, LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, SlotFuture, SlotTransfer,
    StopFuture, Supervisor, SwapDirection, SwapEvent,
};
use tokio::sync::Notify;

const GIB: u64 = 1024 * 1024 * 1024;
/// How long a fake server takes to start.
const LOAD: Duration = Duration::from_millis(20);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What the fake servers did and saw.
#[derive(Debug, Default)]
struct Log {
    /// In order: `launch <model>`, `save <model> <slot> <file>`, `stop <model>`,
    /// `restore <model> <slot> <file>`.
    ops: Mutex<Vec<String>>,
    /// Leases held on each model right now.
    in_use: Mutex<HashMap<String, u32>>,
    /// Models stopped while a lease on them was held.
    stopped_in_use: Mutex<Vec<String>>,
    /// Launches that fail quickly before the next one succeeds.
    launch_failures: AtomicU32,
    /// Launches that time out before the next one succeeds.
    launch_timeouts: AtomicU32,
    /// The folder slots are saved to, as llama-server writes them.
    dir: OnceLock<PathBuf>,
    /// How long a save takes, in milliseconds.
    save_ms: AtomicU64,
    /// Raised when a save starts.
    saving: Notify,
    /// Restores never finish.
    hang_restores: AtomicBool,
    /// Raised when a restore starts.
    restoring: Notify,
}

impl Log {
    fn push(&self, op: String) {
        lock(&self.ops).push(op);
    }

    /// The operations without file names.
    fn steps(&self) -> Vec<String> {
        lock(&self.ops)
            .iter()
            .map(|op| op.split(' ').take(3).collect::<Vec<_>>().join(" "))
            .collect()
    }

    /// The slot files on disk.
    fn files(&self) -> Vec<String> {
        let mut files: Vec<String> = std::fs::read_dir(self.dir.get().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".slot"))
            .collect();
        files.sort();
        files
    }

    fn file(&self, prefix: &str) -> String {
        lock(&self.ops)
            .iter()
            .find(|op| op.starts_with(prefix))
            .and_then(|op| op.split(' ').nth(3))
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Debug)]
struct FakeLauncher {
    log: Arc<Log>,
    slots: tempfile::TempDir,
}

struct FakeServer {
    model: String,
    url: String,
    log: Arc<Log>,
}

impl Server for FakeServer {
    fn base_url(&self) -> &str {
        &self.url
    }

    fn save_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(async move {
            self.log.saving.notify_one();
            let save_ms = self.log.save_ms.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(save_ms)).await;
            std::fs::write(self.log.dir.get().unwrap().join(file), b"kv").unwrap();
            self.log.push(format!("save {} {slot} {file}", self.model));
            Ok(SlotTransfer {
                tokens: 1000,
                bytes: 1024 * 1024,
            })
        })
    }

    /// Takes a moment to exit, so a launch that does not wait for it would come first.
    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move {
            if lock(&self.log.in_use)
                .get(&self.model)
                .copied()
                .unwrap_or(0)
                > 0
            {
                lock(&self.log.stopped_in_use).push(self.model.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.log.push(format!("stop {}", self.model));
        })
    }

    fn restore_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(async move {
            self.log.restoring.notify_one();
            if self.log.hang_restores.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if !self.log.dir.get().unwrap().join(file).exists() {
                return Err(format!("no file {file}"));
            }
            self.log
                .push(format!("restore {} {slot} {file}", self.model));
            Ok(SlotTransfer {
                tokens: 1000,
                bytes: 1024 * 1024,
            })
        })
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            assert_eq!(
                spec.slot_save_path.as_deref(),
                Some(self.slots.path()),
                "llama-server is told where to save slots"
            );
            self.log.push(format!("launch {}", spec.model_id));
            tokio::time::sleep(LOAD).await;
            let failures = &self.log.launch_failures;
            if failures.load(Ordering::SeqCst) > 0 {
                failures.fetch_sub(1, Ordering::SeqCst);
                return Err("the server exited while loading".into());
            }
            let timeouts = &self.log.launch_timeouts;
            if timeouts.load(Ordering::SeqCst) > 0 {
                timeouts.fetch_sub(1, Ordering::SeqCst);
                return Err(LaunchError::timed_out(
                    "the model did not load in time".into(),
                ));
            }
            Ok(Box::new(FakeServer {
                model: spec.model_id.clone(),
                url: format!("http://fake/{}/v1", spec.model_id),
                log: Arc::clone(&self.log),
            }) as Box<dyn Server>)
        })
    }

    fn slot_dir(&self) -> Option<PathBuf> {
        Some(self.slots.path().to_path_buf())
    }
}

fn model(id: &str) -> LocalModel {
    LocalModel {
        id: id.into(),
        path: PathBuf::from(format!("{id}.gguf")),
        parts: 1,
        size_bytes: 6 * GIB,
        info: GgufInfo {
            block_count: Some(32),
            embedding_length: Some(4096),
            head_count: Some(32),
            head_count_kv: Some(8),
            ..GgufInfo::default()
        },
        mmproj: None,
        format: Format::Gguf,
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
        ram_headroom_mb: 0,
        cache_ram_mb: 0,
        swap_cache_mb: 1024,
        min_residency_s: 30,
        max_swaps_per_min: 6,
        swap_gain: 2.0,
        max_wait_s: 600,
        extra_args: Vec::new(),
    }
}

/// A 10 GiB GPU and no spare RAM: one of the two 6 GiB models at a time.
fn start(settings: LlamaCppSettings) -> (Arc<Supervisor>, Arc<Log>) {
    let log = Arc::new(Log::default());
    let slots = tempfile::tempdir().unwrap();
    log.dir.set(slots.path().to_path_buf()).unwrap();
    let launcher = Arc::new(FakeLauncher {
        log: Arc::clone(&log),
        slots,
    });
    let hardware = Hardware {
        gpus: vec![Gpu {
            name: "test".into(),
            vendor: "nvidia".into(),
            vram_total_bytes: 10 * GIB,
            vram_free_bytes: 10 * GIB,
            driver: None,
        }],
        ram_total_bytes: 0,
        ram_available_bytes: 0,
        cpu_cores: 8,
        unified_memory: false,
        overridden: true,
    };
    let supervisor = Supervisor::start(
        vec![model("emperor"), model("worker"), model("helper")],
        settings,
        Runtimes::default(),
        &hardware,
        launcher,
    );
    (supervisor, log)
}

/// One model call: takes a slot, works for `work`, and releases it. Returns how long the call
/// waited for its slot and which slot it got.
async fn call(
    supervisor: &Supervisor,
    log: &Log,
    model: &str,
    holder: &str,
    work: Duration,
) -> (Duration, Option<u32>) {
    let asked = Instant::now();
    let lease = supervisor
        .acquire(model, holder)
        .await
        .unwrap_or_else(|error| panic!("{holder} could not get {model}: {error}"));
    let waited = asked.elapsed();
    *lock(&log.in_use).entry(model.to_string()).or_default() += 1;
    tokio::time::sleep(work).await;
    *lock(&log.in_use).get_mut(model).unwrap() -= 1;
    let slot = lease.slot;
    drop(lease);
    (waited, slot)
}

/// A subagent's turn on the worker model: two calls with a tool run between them.
async fn subagent(supervisor: Arc<Supervisor>, log: Arc<Log>, holder: String) -> Vec<Duration> {
    let _turn = supervisor.attach("worker", &holder);
    let work = Duration::from_millis(20);
    let (first, _) = call(&supervisor, &log, "worker", &holder, work).await;
    tokio::time::sleep(Duration::from_millis(10)).await;
    let (second, _) = call(&supervisor, &log, "worker", &holder, work).await;
    vec![first, second]
}

/// Collects swap events with when they happened.
fn record(supervisor: &Supervisor) -> Arc<Mutex<Vec<(Instant, SwapEvent)>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut receiver = supervisor.swap_events();
    let sink = Arc::clone(&events);
    tokio::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            lock(&sink).push((Instant::now(), event));
        }
    });
    events
}

fn swaps(events: &[(Instant, SwapEvent)], direction: SwapDirection) -> Vec<String> {
    events
        .iter()
        .filter(|(_, event)| event.direction == direction)
        .map(|(_, event)| event.model_id.clone())
        .collect()
}

#[tokio::test]
async fn swap_out_an_awaiting_orchestrator_for_its_subagent_and_restore_it() {
    let (supervisor, log) = start(settings());
    let events = record(&supervisor);
    let root = "session/root";
    let _turn = supervisor.attach("emperor", root);
    let work = Duration::from_millis(10);
    let (_, slot) = call(&supervisor, &log, "emperor", root, work).await;
    assert!(matches!(
        supervisor.availability("worker"),
        Availability::Swap { .. }
    ));

    // The orchestrator delegates; its subagent asks for the worker before it starts waiting.
    let subagent = tokio::spawn(subagent(
        Arc::clone(&supervisor),
        Arc::clone(&log),
        "session/t1".into(),
    ));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        log.steps(),
        ["launch emperor"],
        "nothing is swapped out while the orchestrator is mid-turn"
    );
    let waiting = supervisor.waiting("emperor", root);
    subagent.await.unwrap();
    drop(waiting);
    let (_, slot_back) = call(&supervisor, &log, "emperor", root, work).await;

    assert_eq!(slot_back, slot, "the orchestrator gets its own slot back");
    assert_eq!(
        log.steps(),
        [
            "launch emperor",
            "save emperor 0",
            "stop emperor",
            "launch worker",
            // The subagent's turn is over, so its slot is not saved.
            "stop worker",
            "launch emperor",
            "restore emperor 0",
        ]
    );
    assert_eq!(
        log.file("save emperor"),
        log.file("restore emperor"),
        "the orchestrator's own saved slot is restored"
    );
    assert!(lock(&log.stopped_in_use).is_empty());
    tokio::task::yield_now().await;
    let events = lock(&events);
    assert_eq!(swaps(&events, SwapDirection::Out), ["emperor", "worker"]);
    assert_eq!(swaps(&events, SwapDirection::In), ["worker", "emperor"]);
    let back = &events.last().unwrap().1;
    assert_eq!(
        (back.slots, back.reason),
        (1, "swap"),
        "one saved slot waits for its agent"
    );
}

#[tokio::test]
async fn swap_never_evicts_a_model_in_use() {
    let (supervisor, log) = start(settings());
    let root = "session/root";
    let _turn = supervisor.attach("emperor", root);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    let waiting = supervisor.waiting("emperor", root);
    // A subagent mid-call on the worker.
    let (started, mut working) = tokio::sync::oneshot::channel();
    let busy = {
        let supervisor = Arc::clone(&supervisor);
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            let _turn = supervisor.attach("worker", "session/t1");
            let lease = supervisor.acquire("worker", "session/t1").await.unwrap();
            *lock(&log.in_use).entry("worker".into()).or_default() += 1;
            started.send(()).unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await;
            *lock(&log.in_use).get_mut("worker").unwrap() -= 1;
            drop(lease);
        })
    };
    (&mut working).await.unwrap();
    // The orchestrator wakes while the subagent is still mid-call: it waits for the call.
    drop(waiting);
    assert_eq!(
        supervisor.availability("emperor"),
        Availability::Unavailable
    );
    let (waited, _) = call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    busy.await.unwrap();
    assert!(waited >= Duration::from_millis(250), "waited {waited:?}");
    assert!(
        lock(&log.stopped_in_use).is_empty(),
        "stopped in use: {:?}",
        lock(&log.stopped_in_use)
    );
}

#[tokio::test]
async fn swap_reloads_a_model_evicted_between_calls() {
    // No residency: another session's agent may take the GPU between this agent's calls.
    let (supervisor, log) = start(LlamaCppSettings {
        min_residency_s: 0,
        ..settings()
    });
    let agent = "first/root";
    let _turn = supervisor.attach("emperor", agent);
    call(&supervisor, &log, "emperor", agent, Duration::ZERO).await;
    // Between the agent's calls, another session's agent swaps its model out and is mid-call.
    let other = {
        let supervisor = Arc::clone(&supervisor);
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            let _turn = supervisor.attach("worker", "second/root");
            call(
                &supervisor,
                &log,
                "worker",
                "second/root",
                Duration::from_millis(100),
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        while supervisor.loaded().first().map(|m| m.model_id.clone()) != Some("worker".into()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the worker is loaded");
    // The agent's next call waits for room and reloads its model rather than failing.
    let reloaded = supervisor.acquire("emperor", agent).await;
    assert!(reloaded.is_ok(), "{reloaded:?}");
    other.await.unwrap();
    assert_eq!(
        log.steps().last().map(String::as_str),
        Some("restore emperor 0"),
        "{:?}",
        log.steps()
    );
    assert!(lock(&log.stopped_in_use).is_empty());
}

#[tokio::test(start_paused = true)]
async fn swap_limit_holds_a_call_until_the_minute_moves_on() {
    let (supervisor, log) = start(LlamaCppSettings {
        max_swaps_per_min: 1,
        ..settings()
    });
    call(&supervisor, &log, "emperor", "a/root", Duration::ZERO).await;
    call(&supervisor, &log, "worker", "b/root", Duration::ZERO).await;
    // One swap this minute: the router is told to look elsewhere, but an orchestrator coming
    // back to its own model waits for the window instead of losing its turn.
    assert_eq!(
        supervisor.availability("emperor"),
        Availability::Unavailable
    );
    let asked = tokio::time::Instant::now();
    let lease = supervisor.acquire("emperor", "a/root").await.unwrap();
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_secs(59) && waited <= Duration::from_secs(61),
        "{waited:?}"
    );
    assert_eq!(supervisor.loaded()[0].model_id, "emperor");
    drop(lease);
}

#[tokio::test]
async fn swap_off_refuses_a_model_that_does_not_fit_beside_the_loaded_ones() {
    let (supervisor, log) = start(LlamaCppSettings {
        max_swaps_per_min: 0,
        ..settings()
    });
    call(&supervisor, &log, "emperor", "a/root", Duration::ZERO).await;
    assert_eq!(supervisor.availability("worker"), Availability::Unavailable);
    let error = supervisor.acquire("worker", "b/root").await.unwrap_err();
    assert!(error.contains("not enough memory for worker"), "{error}");
    assert_eq!(log.steps(), ["launch emperor"]);
}

#[tokio::test]
async fn swap_waits_are_capped_by_max_wait() {
    let (supervisor, log) = start(LlamaCppSettings {
        max_wait_s: 1,
        ..settings()
    });
    // Another session holds the worker for longer than the cap.
    let busy = supervisor.acquire("worker", "b/root").await.unwrap();
    let asked = Instant::now();
    let error = supervisor.acquire("emperor", "a/root").await.unwrap_err();
    assert!(error.contains("max_wait_s"), "{error}");
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_secs(1) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
    drop(busy);
    assert_eq!(log.steps(), ["launch worker"]);
}

#[tokio::test]
async fn swap_waits_for_the_old_server_to_exit_and_retries_a_failed_launch() {
    let (supervisor, log) = start(settings());
    call(&supervisor, &log, "emperor", "a/root", Duration::ZERO).await;
    log.launch_failures.store(1, Ordering::SeqCst);
    call(&supervisor, &log, "worker", "b/root", Duration::ZERO).await;
    assert_eq!(
        log.steps(),
        [
            "launch emperor",
            "stop emperor",
            "launch worker",
            "launch worker"
        ]
    );
}

#[tokio::test]
async fn swap_does_not_retry_a_launch_that_timed_out() {
    let (supervisor, log) = start(settings());
    call(&supervisor, &log, "emperor", "a/root", Duration::ZERO).await;
    log.launch_timeouts.store(1, Ordering::SeqCst);
    let error = supervisor.acquire("worker", "b/root").await.unwrap_err();
    assert!(error.contains("did not load in time"), "{error}");
    assert_eq!(
        log.steps(),
        ["launch emperor", "stop emperor", "launch worker"]
    );
}

#[tokio::test]
async fn swap_cache_keeps_a_waiting_orchestrators_slot_over_a_finished_subagents() {
    // Room on disk for two saved slots of 1 MiB; nothing is kept loaded for long.
    let (supervisor, log) = start(LlamaCppSettings {
        swap_cache_mb: 2,
        min_residency_s: 0,
        ..settings()
    });
    let root = "s/root";
    let _turn = supervisor.attach("emperor", root);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    let waiting = supervisor.waiting("emperor", root);
    // The subagent swaps the orchestrator out (saving its slot) and is mid-turn when another
    // session's agent swaps the worker out (saving the subagent's slot too).
    let subagent = supervisor.attach("worker", "s/t1");
    call(&supervisor, &log, "worker", "s/t1", Duration::ZERO).await;
    let _other = supervisor.attach("helper", "x/root");
    call(&supervisor, &log, "helper", "x/root", Duration::ZERO).await;
    // The subagent finishes and the orchestrator wakes; saving the other agent's slot needs
    // room, and the finished subagent's file goes even though the orchestrator's is older.
    drop(subagent);
    drop(waiting);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    assert_eq!(
        log.steps(),
        [
            "launch emperor",
            "save emperor 0",
            "stop emperor",
            "launch worker",
            "save worker 0",
            "stop worker",
            "launch helper",
            "save helper 0",
            "stop helper",
            "launch emperor",
            "restore emperor 0",
        ]
    );
    assert_eq!(log.file("save emperor"), log.file("restore emperor"));
}

/// An orchestrator: one call, two subagents on the worker model, a wait for them, one more
/// call. Returns how long each of its and its subagents' calls waited for a slot.
async fn orchestrator(
    supervisor: Arc<Supervisor>,
    log: Arc<Log>,
    session: String,
) -> Vec<Duration> {
    let root = format!("{session}/root");
    let _turn = supervisor.attach("emperor", &root);
    let work = Duration::from_millis(30);
    let mut waits = vec![call(&supervisor, &log, "emperor", &root, work).await.0];
    let tasks: Vec<_> = (1..=2)
        .map(|n| {
            tokio::spawn(subagent(
                Arc::clone(&supervisor),
                Arc::clone(&log),
                format!("{session}/t{n}"),
            ))
        })
        .collect();
    {
        let _waiting = supervisor.waiting("emperor", &root);
        for task in tasks {
            waits.extend(task.await.unwrap());
        }
    }
    waits.push(call(&supervisor, &log, "emperor", &root, work).await.0);
    waits
}

#[tokio::test]
async fn swap_parallel_sessions_batch_by_model() {
    let (supervisor, log) = start(settings());
    let events = record(&supervisor);
    let began = Instant::now();
    let sessions: Vec<_> = ["one", "two", "three"]
        .into_iter()
        .map(|session| {
            tokio::spawn(orchestrator(
                Arc::clone(&supervisor),
                Arc::clone(&log),
                session.into(),
            ))
        })
        .collect();
    let mut waits = Vec::new();
    for session in sessions {
        let session = tokio::time::timeout(Duration::from_secs(10), session)
            .await
            .expect("every session finishes");
        waits.extend(session.unwrap());
    }
    tokio::task::yield_now().await;
    let events = lock(&events);

    // Three sessions' six tasks run in one batch: one swap out and back each way.
    let out = swaps(&events, SwapDirection::Out);
    let back = swaps(&events, SwapDirection::In);
    assert!(
        out.len() <= 2 && back.len() <= 2,
        "out {out:?}, in {back:?}"
    );
    assert_eq!(out, ["emperor", "worker"]);
    assert_eq!(back, ["worker", "emperor"]);
    assert!(
        lock(&log.stopped_in_use).is_empty(),
        "stopped in use: {:?}",
        lock(&log.stopped_in_use)
    );

    // No call waited longer than one batch plus one swap.
    let at = |direction, model: &str| {
        events
            .iter()
            .find(|(_, event)| event.direction == direction && event.model_id == model)
            .map(|(at, _)| *at)
            .unwrap()
    };
    let emperor_batch = at(SwapDirection::Out, "emperor") - began;
    let worker_batch = at(SwapDirection::Out, "worker") - at(SwapDirection::In, "worker");
    let swap = events
        .iter()
        .map(|(_, event)| Duration::from_secs_f64(event.seconds))
        .max()
        .unwrap()
        + LOAD;
    let bound = emperor_batch.max(worker_batch) + swap + Duration::from_millis(100);
    let longest = waits.iter().max().unwrap();
    assert!(
        *longest <= bound,
        "longest wait {longest:?}, bound {bound:?} (batches {emperor_batch:?}, \
         {worker_batch:?})"
    );
}

#[tokio::test]
async fn swap_out_finishes_when_the_call_that_started_it_is_cancelled() {
    let (supervisor, log) = start(LlamaCppSettings {
        min_residency_s: 0,
        ..settings()
    });
    let root = "s/root";
    let _turn = supervisor.attach("emperor", root);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    let waiting = supervisor.waiting("emperor", root);
    // The subagent's call is cancelled while the orchestrator's slot is being saved.
    log.save_ms.store(100, Ordering::SeqCst);
    let saving = log.saving.notified();
    let cancelled = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("worker", "s/t1").await.map(drop) })
    };
    saving.await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    // The next call for the worker loads it only once the orchestrator's server has exited.
    call(&supervisor, &log, "worker", "s/t1", Duration::ZERO).await;
    assert_eq!(
        log.steps(),
        [
            "launch emperor",
            "save emperor 0",
            "stop emperor",
            "launch worker"
        ]
    );
    // The save was recorded, so the orchestrator gets its slot back and the file is gone.
    assert_eq!(log.files().len(), 1);
    drop(waiting);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    assert_eq!(
        log.steps().last().map(String::as_str),
        Some("restore emperor 0")
    );
    assert_eq!(log.files(), Vec::<String>::new());
}

#[tokio::test]
async fn swap_cancelled_restore_leaves_no_file_behind() {
    let (supervisor, log) = start(LlamaCppSettings {
        min_residency_s: 0,
        ..settings()
    });
    let root = "s/root";
    let _turn = supervisor.attach("emperor", root);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    let waiting = supervisor.waiting("emperor", root);
    call(&supervisor, &log, "worker", "s/t1", Duration::ZERO).await;
    assert_eq!(log.files().len(), 1, "the orchestrator's slot is saved");
    drop(waiting);
    // The orchestrator's call is cancelled while its slot is being restored.
    log.hang_restores.store(true, Ordering::SeqCst);
    let restoring = log.restoring.notified();
    let cancelled = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("emperor", root).await.map(drop) })
    };
    restoring.await;
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());
    assert_eq!(log.files(), Vec::<String>::new(), "the file is removed");
    // It is forgotten too: the next call reads its prompt again instead of restoring.
    log.hang_restores.store(false, Ordering::SeqCst);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    assert!(
        !log.steps().iter().any(|step| step.starts_with("restore")),
        "{:?}",
        log.steps()
    );
}

#[tokio::test]
async fn swap_restores_a_saved_slot_only_into_its_own_agent() {
    // One slot, so another agent can only take the one kept for the orchestrator.
    let (supervisor, log) = start(LlamaCppSettings {
        min_residency_s: 0,
        parallel: 1,
        ..settings()
    });
    let root = "s/root";
    let _turn = supervisor.attach("emperor", root);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    let waiting = supervisor.waiting("emperor", root);
    call(&supervisor, &log, "worker", "s/t1", Duration::ZERO).await;
    // Another session's agent brings the model back and takes the slot kept for the
    // orchestrator: the orchestrator's saved prompt is not loaded into it.
    let (_, slot) = call(&supervisor, &log, "emperor", "x/root", Duration::ZERO).await;
    assert_eq!(slot, Some(0));
    assert!(
        !log.steps().iter().any(|step| step.starts_with("restore")),
        "{:?}",
        log.steps()
    );
    // The orchestrator gets its own prompt back when it calls.
    drop(waiting);
    call(&supervisor, &log, "emperor", root, Duration::ZERO).await;
    assert_eq!(
        log.steps().last().map(String::as_str),
        Some("restore emperor 0")
    );
    assert_eq!(log.file("save emperor"), log.file("restore emperor"));
    assert_eq!(log.files(), Vec::<String>::new());
}
