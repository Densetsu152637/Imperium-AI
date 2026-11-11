//! Stage 2g: calls from sessions a schedule or a project agent started rank below the user's own.
//! An interactive call is admitted before background calls queued earlier, background work never
//! swaps out a model the user used lately, and it waits while the machine is saturated.
//! llama-server is replaced by in-process fakes; agents are played by the test through the same
//! calls the agent loop makes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use imperium_api::{Load, LoadView, LocalModels, Priority};
use imperium_config::LlamaCppSettings;
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, StopFuture, Supervisor,
};

const GIB: u64 = 1024 * 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What the fake servers did, in order: `launch <model>`, `stop <model>`.
#[derive(Debug, Default)]
struct Log {
    ops: Mutex<Vec<String>>,
}

impl Log {
    fn ops(&self) -> Vec<String> {
        lock(&self.ops).clone()
    }
}

#[derive(Debug)]
struct FakeLauncher {
    log: Arc<Log>,
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

    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move {
            lock(&self.log.ops).push(format!("stop {}", self.model));
        })
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            lock(&self.log.ops).push(format!("launch {}", spec.model_id));
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(Box::new(FakeServer {
                model: spec.model_id.clone(),
                url: format!("http://fake/{}/v1", spec.model_id),
                log: Arc::clone(&self.log),
            }) as Box<dyn Server>)
        })
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
        repo: None,
        format: Format::Gguf,
    }
}

fn settings() -> LlamaCppSettings {
    LlamaCppSettings {
        backend: "auto".into(),
        custom_path: None,
        find_existing: true,
        auto_install: false,
        update_external: "off".into(),
        idle_unload_s: 900,
        context: 4096.into(),
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
    }
}

/// A 10 GiB GPU and no spare RAM: one of the 6 GiB models at a time, each with one slot.
fn start() -> (Arc<Supervisor>, Arc<Log>) {
    start_with(settings())
}

fn start_with(settings: LlamaCppSettings) -> (Arc<Supervisor>, Arc<Log>) {
    let log = Arc::new(Log::default());
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
        vec![model("worker"), model("helper")],
        settings,
        Runtimes::default(),
        &hardware,
        Arc::new(FakeLauncher {
            log: Arc::clone(&log),
        }),
    );
    (supervisor, log)
}

/// A load view the test sets.
#[derive(Debug, Default)]
struct Manual(Mutex<Load>);

impl Manual {
    fn set(&self, load: Load) {
        *lock(&self.0) = load;
    }
}

impl LoadView for Manual {
    fn level(&self) -> Load {
        *lock(&self.0)
    }
}

/// Runs a call that takes `model` for `work` and records `name` when it gets its slot.
fn spawn_call(
    supervisor: &Arc<Supervisor>,
    order: &Arc<Mutex<Vec<&'static str>>>,
    name: &'static str,
    model: &'static str,
    priority: Priority,
    work: Duration,
) -> tokio::task::JoinHandle<()> {
    let supervisor = Arc::clone(supervisor);
    let order = Arc::clone(order);
    tokio::spawn(async move {
        let lease = supervisor
            .acquire_as(model, name, priority)
            .await
            .unwrap_or_else(|error| panic!("{name} could not get {model}: {error}"));
        lock(&order).push(name);
        tokio::time::sleep(work).await;
        drop(lease);
    })
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(60)).await;
}

#[tokio::test]
async fn background_priority_admits_the_users_call_before_earlier_background_calls() {
    let (supervisor, _log) = start();
    let order = Arc::new(Mutex::new(Vec::new()));
    let work = Duration::from_millis(30);
    // The one slot is taken while three calls arrive: two background, then the user's.
    let held = supervisor.acquire("worker", "user-0").await.unwrap();
    let first = spawn_call(
        &supervisor,
        &order,
        "background-1",
        "worker",
        Priority::Background,
        work,
    );
    settle().await;
    let second = spawn_call(
        &supervisor,
        &order,
        "background-2",
        "worker",
        Priority::Background,
        work,
    );
    settle().await;
    let user = spawn_call(
        &supervisor,
        &order,
        "user-1",
        "worker",
        Priority::Interactive,
        work,
    );
    settle().await;
    assert!(lock(&order).is_empty(), "the slot is still held");
    // Background calls wait outside the queue, so the queue shows the user's call alone.
    assert_eq!(supervisor.activity().interactive_queued, 1);
    drop(held);
    for call in [user, first, second] {
        tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        *lock(&order),
        ["user-1", "background-1", "background-2"],
        "the user's call is served first, then background calls in arrival order"
    );
}

#[tokio::test]
async fn background_priority_lets_the_users_call_load_its_model_first() {
    let (supervisor, log) = start();
    supervisor.set_background_policy(Arc::new(Manual::default()), Duration::from_millis(200));
    let order = Arc::new(Mutex::new(Vec::new()));
    // Only one model fits. "worker" is loaded and in use; a background call for "helper"
    // arrives first, then the user's call for... "helper" too, on another holder.
    let held = supervisor.acquire("worker", "user-0").await.unwrap();
    let background = spawn_call(
        &supervisor,
        &order,
        "background",
        "helper",
        Priority::Background,
        Duration::from_millis(20),
    );
    settle().await;
    let user = spawn_call(
        &supervisor,
        &order,
        "user",
        "helper",
        Priority::Interactive,
        Duration::from_millis(20),
    );
    settle().await;
    drop(held);
    for call in [user, background] {
        tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(*lock(&order), ["user", "background"]);
    assert_eq!(
        log.ops(),
        ["launch worker", "stop worker", "launch helper"],
        "the model loaded once, for the user's call"
    );
}

#[tokio::test]
async fn background_priority_never_evicts_a_model_the_user_used_lately() {
    let (supervisor, log) = start();
    supervisor.set_background_policy(Arc::new(Manual::default()), Duration::from_millis(400));
    // The user's call uses "worker" and finishes; "worker" now sits idle.
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    let order = Arc::new(Mutex::new(Vec::new()));
    let background = spawn_call(
        &supervisor,
        &order,
        "background",
        "helper",
        Priority::Background,
        Duration::from_millis(10),
    );
    // Inside the window the background call waits and "worker" stays loaded.
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(lock(&order).is_empty());
    assert_eq!(log.ops(), ["launch worker"]);
    // The user comes back to "worker": the window starts over.
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(lock(&order).is_empty());
    assert_eq!(log.ops(), ["launch worker"]);
    // Once the window has passed, background work may swap it out.
    tokio::time::timeout(Duration::from_secs(5), background)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*lock(&order), ["background"]);
    assert_eq!(log.ops(), ["launch worker", "stop worker", "launch helper"]);
    // A model only background calls used is not protected: the user's call swaps it out at once.
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    assert_eq!(
        log.ops(),
        [
            "launch worker",
            "stop worker",
            "launch helper",
            "stop helper",
            "launch worker"
        ]
    );
}

#[tokio::test]
async fn background_priority_waits_while_the_machine_is_saturated() {
    let (supervisor, _log) = start();
    let view = Arc::new(Manual::default());
    supervisor.set_background_policy(Arc::clone(&view) as Arc<dyn LoadView>, Duration::ZERO);
    view.set(Load::Saturated);
    let order = Arc::new(Mutex::new(Vec::new()));
    let background = spawn_call(
        &supervisor,
        &order,
        "background",
        "worker",
        Priority::Background,
        Duration::from_millis(10),
    );
    // The user's own call is served while the background one waits.
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(lock(&order).is_empty(), "saturated: background waits");
    // A busy machine is not enough to hold a call back; only a saturated one is.
    view.set(Load::Busy);
    tokio::time::timeout(Duration::from_secs(5), background)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(*lock(&order), ["background"]);
    assert_eq!(supervisor.activity().background_in_use, 0);
}

#[tokio::test]
async fn background_priority_only_waiting_on_the_users_use_is_uncapped() {
    // Waits for any other reason still fail at `max_wait_s`.
    let capped = LlamaCppSettings {
        max_wait_s: 1,
        ..settings()
    };
    let (supervisor, _log) = start_with(capped.clone());
    supervisor.set_background_policy(Arc::new(Manual::default()), Duration::from_millis(1500));
    let in_use = supervisor
        .acquire_as("worker", "background-0", Priority::Background)
        .await
        .unwrap();
    let error = supervisor
        .acquire_as("helper", "background-1", Priority::Background)
        .await
        .unwrap_err();
    assert!(error.contains("waited 1 s"), "{error}");
    drop(in_use);

    // Waiting out the window after the user's own use is giving way, not a failure to get
    // room: it outlasts `max_wait_s` and then succeeds.
    let (supervisor, log) = start_with(capped);
    supervisor.set_background_policy(Arc::new(Manual::default()), Duration::from_millis(2500));
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    let lease = supervisor
        .acquire_as("helper", "background-1", Priority::Background)
        .await
        .expect("the wait for the user's window does not count against max_wait_s");
    drop(lease);
    assert_eq!(log.ops(), ["launch worker", "stop worker", "launch helper"]);
}

#[tokio::test]
async fn background_priority_a_wait_the_users_use_does_not_explain_is_still_capped() {
    // The swap limit is spent, so the call would wait even if the user had not used the model
    // lately: that wait is not giving way to the user, and `max_wait_s` bounds it.
    let (supervisor, _log) = start_with(LlamaCppSettings {
        max_wait_s: 1,
        max_swaps_per_min: 1,
        ..settings()
    });
    supervisor.set_background_policy(Arc::new(Manual::default()), Duration::from_secs(30));
    drop(supervisor.acquire("worker", "user-0").await.unwrap());
    drop(supervisor.acquire("helper", "user-0").await.unwrap());
    let error = supervisor
        .acquire_as("worker", "background-1", Priority::Background)
        .await
        .unwrap_err();
    assert!(error.contains("waited 1 s"), "{error}");
}
