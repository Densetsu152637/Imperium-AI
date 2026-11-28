//! The supervised local whisper: starting on demand, health, idle stop, failures, and memory
//! accounting beside the language models. whisper-server is replaced by an in-process fake that
//! records what it was asked to do, and llama-server by the same kind of fake; nothing here
//! starts a real server, reads a GPU, or downloads a model.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use imperium_api::LocalModels;
use imperium_config::LlamaCppSettings;
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::speech::{
    LaunchFuture, SpeechConfig, SpeechError, SpeechLaunch, SpeechLauncher, SpeechProcess,
    SpeechServer, StopFuture, footprint,
};
use imperium_local::supervisor::{
    LaunchFuture as ModelLaunchFuture, LaunchSpec, Launcher, Runtimes, Server, SideLoad,
    SlotFuture, SlotTransfer, StopFuture as ModelStopFuture, Supervisor,
};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What the fake whisper-server did.
#[derive(Debug, Default)]
struct Log {
    /// `(gpu, language)` of each launch.
    launches: Mutex<Vec<(bool, String)>>,
    stops: AtomicU32,
    /// Extra milliseconds a launch takes.
    delay_ms: AtomicU64,
    /// Launches that fail before the next one succeeds, with this message.
    failure: Mutex<Option<SpeechError>>,
    /// The latest server has exited on its own.
    exited: Mutex<Option<Arc<AtomicBool>>>,
}

impl Log {
    fn launches(&self) -> usize {
        lock(&self.launches).len()
    }

    fn crash(&self) {
        lock(&self.exited)
            .as_ref()
            .expect("a server is running")
            .store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct FakeSpeechLauncher {
    log: Arc<Log>,
}

struct FakeSpeech {
    log: Arc<Log>,
    exited: Arc<AtomicBool>,
    stopped: AtomicBool,
}

impl SpeechProcess for FakeSpeech {
    fn base_url(&self) -> &str {
        "http://127.0.0.1:1/v1"
    }

    fn pid(&self) -> Option<u32> {
        Some(4242)
    }

    fn exited(&self) -> Option<String> {
        self.exited
            .load(Ordering::SeqCst)
            .then(|| "whisper-server exited (exit code: 1)".to_string())
    }

    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move {
            if !self.stopped.swap(true, Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
                self.log.stops.fetch_add(1, Ordering::SeqCst);
            }
        })
    }
}

impl SpeechLauncher for FakeSpeechLauncher {
    fn launch<'a>(&'a self, spec: &'a SpeechLaunch) -> LaunchFuture<'a> {
        Box::pin(async move {
            lock(&self.log.launches).push((spec.gpu, spec.language.clone()));
            let delay = self.log.delay_ms.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(10 + delay)).await;
            if let Some(error) = lock(&self.log.failure).clone() {
                return Err(error);
            }
            let exited = Arc::new(AtomicBool::new(false));
            *lock(&self.log.exited) = Some(Arc::clone(&exited));
            Ok(Box::new(FakeSpeech {
                log: Arc::clone(&self.log),
                exited,
                stopped: AtomicBool::new(false),
            }) as Box<dyn SpeechProcess>)
        })
    }
}

/// A server over a model file of `model_mib` MiB, stopping after `idle_ms` unused.
fn server(model_mib: u64, idle_ms: u64) -> (Arc<SpeechServer>, Arc<Log>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("ggml-test.bin");
    std::fs::File::create(&model)
        .unwrap()
        .set_len(model_mib * MIB)
        .unwrap();
    let config = SpeechConfig {
        binary: PathBuf::from("whisper-server"),
        model,
        gpu: true,
        threads: 0,
        language: "en".into(),
        convert: false,
        extra_args: Vec::new(),
        idle_stop: Duration::from_millis(idle_ms),
        start_timeout: Duration::from_secs(5),
        log: dir.path().join("whisper.log"),
        watch: Duration::from_millis(20),
    };
    let log = Arc::new(Log::default());
    let launcher = Arc::new(FakeSpeechLauncher {
        log: Arc::clone(&log),
    });
    (SpeechServer::new(config, launcher), log, dir)
}

/// Waits until `check` holds, for at most five seconds.
async fn until(what: &str, check: impl Fn() -> bool) {
    for _ in 0..500 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn it_starts_on_the_first_recording_and_is_reused_after() {
    let (server, log, _dir) = server(150, 60_000);
    assert!(!server.status().running, "nothing starts until it is used");
    let first = server.acquire().await.unwrap();
    assert_eq!(first.base_url(), "http://127.0.0.1:1/v1");
    let second = server.acquire().await.unwrap();
    assert_eq!(log.launches(), 1, "one server serves both");
    assert_eq!(server.status().active, 2);
    assert_eq!(server.status().pid, Some(4242));
    assert_eq!(
        lock(&log.launches)[0].1,
        "en",
        "the language reaches the server"
    );
    drop((first, second));
    assert_eq!(server.status().active, 0);
}

#[tokio::test]
async fn an_idle_server_stops_and_the_next_recording_starts_a_new_one() {
    let (server, log, _dir) = server(150, 100);
    drop(server.acquire().await.unwrap());
    until("the idle stop", || log.stops.load(Ordering::SeqCst) == 1).await;
    assert!(!server.status().running);
    assert_eq!(
        server.status().last_stop.as_deref(),
        Some("it was idle"),
        "the status says why it stopped"
    );
    drop(server.acquire().await.unwrap());
    assert_eq!(log.launches(), 2);
}

#[tokio::test]
async fn a_server_in_use_is_not_stopped_for_being_idle() {
    let (server, log, _dir) = server(150, 60);
    let lease = server.acquire().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(server.status().running, "held by a lease");
    assert_eq!(log.stops.load(Ordering::SeqCst), 0);
    drop(lease);
    until("the idle stop after the lease", || !server.status().running).await;
}

#[tokio::test]
async fn an_idle_stop_of_zero_keeps_it_running() {
    let (server, _log, _dir) = server(150, 0);
    drop(server.acquire().await.unwrap());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(server.status().running);
}

#[tokio::test]
async fn a_server_that_exits_on_its_own_is_replaced() {
    let (server, log, _dir) = server(150, 60_000);
    drop(server.acquire().await.unwrap());
    log.crash();
    until("the watchdog noticing", || !server.status().running).await;
    assert!(
        server
            .status()
            .last_stop
            .unwrap_or_default()
            .contains("exited"),
        "the exit is recorded"
    );
    drop(server.acquire().await.unwrap());
    assert_eq!(log.launches(), 2);
}

#[tokio::test]
async fn a_failed_call_restarts_the_server_for_the_next_one() {
    let (server, log, _dir) = server(150, 60_000);
    let lease = server.acquire().await.unwrap();
    lease.failed();
    drop(lease);
    drop(server.acquire().await.unwrap());
    assert_eq!(log.launches(), 2);
    assert_eq!(
        log.stops.load(Ordering::SeqCst),
        1,
        "the bad one was stopped"
    );
}

#[tokio::test]
async fn a_start_that_fails_is_reported_and_holds_nothing() {
    let (server, log, _dir) = server(150, 60_000);
    *lock(&log.failure) = Some(SpeechError::Start(
        "whisper-server exited while loading (exit code: 1); see whisper.log".into(),
    ));
    let error = server.acquire().await.err().unwrap();
    assert!(
        error.to_string().contains("exited while loading"),
        "{error}"
    );
    assert!(!server.status().running);
    // Once the cause is gone, the next recording starts it.
    *lock(&log.failure) = None;
    drop(server.acquire().await.unwrap());
    assert!(server.status().running);
}

#[tokio::test]
async fn a_missing_model_is_explained() {
    let (_server, _log, dir) = server(1, 60_000);
    let config = SpeechConfig {
        binary: PathBuf::from("whisper-server"),
        model: dir.path().join("missing.bin"),
        gpu: true,
        threads: 0,
        language: "auto".into(),
        convert: false,
        extra_args: Vec::new(),
        idle_stop: Duration::ZERO,
        start_timeout: Duration::from_secs(1),
        log: dir.path().join("w.log"),
        watch: Duration::from_millis(20),
    };
    let launcher = Arc::new(FakeSpeechLauncher {
        log: Arc::new(Log::default()),
    });
    let error = SpeechServer::new(config, launcher)
        .acquire()
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("voice.local.model"), "{error}");
}

// ---- memory beside the language models ----

fn hardware(vram_gib: u64, ram_gib: u64) -> Hardware {
    Hardware {
        gpus: if vram_gib == 0 {
            Vec::new()
        } else {
            vec![Gpu {
                name: "test".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: vram_gib * GIB,
                vram_free_bytes: vram_gib * GIB,
                driver: None,
            }]
        },
        ram_total_bytes: ram_gib * GIB,
        ram_available_bytes: ram_gib * GIB,
        cpu_cores: 8,
        unified_memory: false,
        overridden: true,
        vram_overridden: false,
    }
}

fn settings() -> LlamaCppSettings {
    LlamaCppSettings {
        backend: "auto".into(),
        custom_path: None,
        find_existing: true,
        auto_install: false,
        update_external: "off".into(),
        check_updates: false,
        idle_unload_s: 900,
        context: 4096.into(),
        parallel: 2,
        cache_type: "f16".into(),
        slots: Some(2),
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
        max_wait_s: 2,
        extra_args: Vec::new(),
    }
}

fn model(id: &str, gib: u64) -> LocalModel {
    LocalModel {
        id: id.into(),
        path: PathBuf::from(format!("{id}.gguf")),
        parts: 1,
        size_bytes: gib * GIB,
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

#[derive(Debug)]
struct FakeLlamaLauncher;

struct FakeLlama;

impl Server for FakeLlama {
    fn base_url(&self) -> &str {
        "http://fake/v1"
    }

    fn save_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Ok(SlotTransfer::default()) })
    }

    fn restore_slot<'a>(&'a self, _slot: u32, _file: &'a str) -> SlotFuture<'a> {
        Box::pin(async { Ok(SlotTransfer::default()) })
    }

    fn erase_slot(&self, _slot: u32) -> SlotFuture<'_> {
        Box::pin(async { Ok(SlotTransfer::default()) })
    }

    fn stop(&self) -> ModelStopFuture<'_> {
        Box::pin(async {})
    }
}

impl Launcher for FakeLlamaLauncher {
    fn launch<'a>(&'a self, _spec: &'a LaunchSpec) -> ModelLaunchFuture<'a> {
        Box::pin(async {
            let server: Box<dyn Server> = Box::new(FakeLlama);
            Ok(server)
        })
    }
}

fn supervisor(vram_gib: u64, ram_gib: u64, models: Vec<LocalModel>) -> Arc<Supervisor> {
    Supervisor::start(
        models,
        settings(),
        Runtimes::default(),
        &hardware(vram_gib, ram_gib),
        Arc::new(FakeLlamaLauncher),
    )
}

#[tokio::test]
async fn the_server_holds_gpu_memory_beside_the_models_while_it_runs() {
    let supervisor = supervisor(10, 16, vec![model("m", 6)]);
    let (server, log, _dir) = server(1000, 60_000);
    server.attach(&supervisor);
    let before = supervisor.budget();
    let lease = server.acquire().await.unwrap();
    let held = footprint(1000 * MIB);
    assert_eq!(
        supervisor.side_load(),
        (held, 0),
        "on the GPU, counted there"
    );
    assert_eq!(supervisor.budget().vram_bytes, before.vram_bytes - held);
    assert_eq!(supervisor.budget().ram_bytes, before.ram_bytes);
    assert!(lock(&log.launches)[0].0, "started with the GPU");
    drop(lease);
    assert!(server.stop_if_unused("test").await);
    assert_eq!(supervisor.side_load(), (0, 0), "given back when it stops");
    assert_eq!(supervisor.budget(), before);
}

#[tokio::test]
async fn with_no_gpu_room_it_runs_on_the_cpu_and_holds_ram() {
    // A model holds most of the GPU; the server's footprint does not fit in what is left.
    let supervisor = supervisor(7, 16, vec![model("m", 6)]);
    let lease = supervisor.acquire("m", "s/root").await.unwrap();
    let (server, log, _dir) = server(3000, 60_000);
    server.attach(&supervisor);
    drop(server.acquire().await.unwrap());
    assert!(!lock(&log.launches)[0].0, "started without the GPU");
    let (vram, ram) = supervisor.side_load();
    assert_eq!((vram, ram), (0, footprint(3000 * MIB)));
    drop(lease);
}

#[tokio::test]
async fn with_no_room_at_all_it_does_not_start_and_says_so() {
    let supervisor = supervisor(0, 1, Vec::new());
    let (server, log, _dir) = server(2000, 60_000);
    server.attach(&supervisor);
    let error = server.acquire().await.err().unwrap();
    assert!(matches!(error, SpeechError::NoRoom { .. }), "{error}");
    assert_eq!(log.launches(), 0, "nothing was started");
    assert_eq!(supervisor.side_load(), (0, 0));
}

#[tokio::test]
async fn a_model_that_needs_the_room_makes_an_idle_server_leave() {
    // The 6 GiB model and the server (about 5 GiB for this file) do not fit on a 10 GiB GPU
    // together, and there is no RAM to spill into.
    let supervisor = supervisor(10, 0, vec![model("m", 6)]);
    let (server, log, _dir) = server(3800, 60_000);
    server.attach(&supervisor);
    drop(server.acquire().await.unwrap());
    assert!(supervisor.side_load().0 > 4 * GIB);
    let lease = supervisor.acquire("m", "s/root").await;
    let lease = lease.expect("the model loads once the server has left");
    assert_eq!(
        log.stops.load(Ordering::SeqCst),
        1,
        "the server stopped for it"
    );
    assert!(!server.status().running);
    assert_eq!(supervisor.side_load(), (0, 0));
    drop(lease);
}

#[tokio::test]
async fn a_server_in_use_finishes_its_recording_before_it_leaves() {
    let supervisor = supervisor(10, 0, vec![model("m", 6)]);
    let (server, log, _dir) = server(3800, 60_000);
    server.attach(&supervisor);
    let recording = server.acquire().await.unwrap();
    let model = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("m", "s/root").await.map(drop) })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(log.stops.load(Ordering::SeqCst), 0, "still transcribing");
    assert!(!model.is_finished(), "the model waits");
    drop(recording);
    model.await.unwrap().expect("loads after the recording");
    assert_eq!(log.stops.load(Ordering::SeqCst), 1);
}

/// A holder that gives nothing back.
#[derive(Debug)]
struct Stubborn(AtomicU32);

impl SideLoad for Stubborn {
    fn yield_now(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn without_a_holder_that_gives_way_a_model_does_not_fit() {
    let supervisor = supervisor(10, 0, vec![model("m", 6)]);
    let stubborn = Arc::new(Stubborn(AtomicU32::new(0)));
    let as_side: Arc<dyn SideLoad> = stubborn.clone();
    supervisor.set_side_yielder(Arc::downgrade(&as_side));
    supervisor.set_side_load(5 * GIB, 0);
    let error = match supervisor.acquire("m", "s/root").await {
        Ok(_) => panic!("loaded into memory that is held"),
        Err(error) => error,
    };
    assert!(!error.is_empty(), "the call fails with a reason");
    assert!(
        stubborn.0.load(Ordering::SeqCst) >= 1,
        "it was asked to leave"
    );
}

#[tokio::test]
async fn a_cancelled_start_gives_its_memory_back() {
    let supervisor = supervisor(10, 16, Vec::new());
    let (server, log, _dir) = server(1000, 60_000);
    server.attach(&supervisor);
    log.delay_ms.store(60_000, Ordering::SeqCst);
    let starting = {
        let server = Arc::clone(&server);
        tokio::spawn(async move { server.acquire().await.map(drop) })
    };
    until("the memory to be held", || supervisor.side_load().0 > 0).await;
    starting.abort();
    let _ = starting.await;
    assert_eq!(
        supervisor.side_load(),
        (0, 0),
        "nothing is held for no server"
    );
    assert!(!server.status().running);
    // The next recording starts normally.
    log.delay_ms.store(0, Ordering::SeqCst);
    drop(server.acquire().await.unwrap());
    assert!(server.status().running);
}
