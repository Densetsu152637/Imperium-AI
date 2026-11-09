//! Stage 2f: a model pinned to the CPU lane lives in system RAM beside the GPU models. It is
//! started with no GPU layers, counts against the RAM budget alone, is never swapped out to make
//! room on the GPU, and is refused (and reported unavailable) when the RAM is short.
//! llama-server is replaced by an in-process fake that records how it was started.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use imperium_api::{Availability, LocalModels};
use imperium_config::LlamaCppSettings;
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    CpuLane, LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, StopFuture, Supervisor,
};

const GIB: u64 = 1024 * 1024 * 1024;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What was asked of the fake servers, in order.
#[derive(Debug, Default)]
struct Log {
    /// `(model, arguments)` of each launch.
    launches: Mutex<Vec<(String, Vec<String>)>>,
    /// Models stopped, in order.
    stops: Mutex<Vec<String>>,
}

impl Log {
    fn args(&self, model: &str) -> String {
        lock(&self.launches)
            .iter()
            .rev()
            .find(|(id, _)| id == model)
            .map(|(_, args)| args.join(" "))
            .unwrap_or_else(|| panic!("{model} was not launched"))
    }

    fn launched(&self) -> Vec<String> {
        lock(&self.launches)
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn stopped(&self) -> Vec<String> {
        lock(&self.stops).clone()
    }
}

#[derive(Debug)]
struct FakeLauncher(Arc<Log>);

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
            lock(&self.log.stops).push(self.model.clone());
        })
    }
}

impl Launcher for FakeLauncher {
    fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
        Box::pin(async move {
            lock(&self.0.launches).push((spec.model_id.clone(), spec.args()));
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(Box::new(FakeServer {
                model: spec.model_id.clone(),
                url: format!("http://fake/{}/v1", spec.model_id),
                log: Arc::clone(&self.0),
            }) as Box<dyn Server>)
        })
    }
}

/// A 32-layer model with `gib` GiB of weights.
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

fn settings(cache_ram_mb: u64) -> LlamaCppSettings {
    LlamaCppSettings {
        backend: "auto".into(),
        custom_path: None,
        find_existing: true,
        auto_install: false,
        update_external: "off".into(),
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
        cache_ram_mb,
        swap_cache_mb: 0,
        min_residency_s: 30,
        max_swaps_per_min: 6,
        swap_gain: 2.0,
        max_wait_s: 600,
        extra_args: Vec::new(),
    }
}

fn hardware(vram_gib: u64, ram_gib: u64) -> Hardware {
    Hardware {
        gpus: vec![Gpu {
            name: "test".into(),
            vendor: "nvidia".into(),
            vram_total_bytes: vram_gib * GIB,
            vram_free_bytes: vram_gib * GIB,
            driver: None,
        }],
        ram_total_bytes: ram_gib * GIB,
        ram_available_bytes: ram_gib * GIB,
        cpu_cores: 16,
        unified_memory: false,
        overridden: true,
    }
}

/// Two 6 GiB GPU models, one at a time on the 10 GiB GPU, and an 18 GiB model pinned to the
/// CPU lane.
fn start(hardware: &Hardware, cache_ram_mb: u64) -> (Arc<Supervisor>, Arc<Log>) {
    let log = Arc::new(Log::default());
    let supervisor = Supervisor::start(
        vec![model("emperor", 6), model("worker", 6), model("critic", 18)],
        settings(cache_ram_mb),
        Runtimes::default(),
        hardware,
        Arc::new(FakeLauncher(Arc::clone(&log))),
    );
    supervisor.set_cpu_pinned(HashMap::from([("critic".to_string(), CpuLane::default())]));
    (supervisor, log)
}

#[tokio::test]
async fn cpu_lane_row_is_started_with_no_gpu_layers_and_one_slot() {
    let (supervisor, log) = start(&hardware(24, 64), 0);
    let lease = supervisor.acquire("critic", "reviewer").await.unwrap();
    assert_eq!(lease.base_url, "http://fake/critic/v1");
    let args = log.args("critic");
    assert!(args.contains("--n-gpu-layers 0"), "{args}");
    // `-ngl 0` alone still initialises the GPU backend; the lane uses no device at all.
    assert!(args.contains("--device none"), "{args}");
    assert!(args.contains("--parallel 1"), "{args}");
    // The per-slot context times the slots.
    assert!(args.contains("--ctx-size 4096"), "{args}");

    let loaded = supervisor.loaded();
    assert_eq!(loaded.len(), 1);
    let critic = &loaded[0];
    assert!(critic.cpu_pinned);
    assert_eq!(
        (critic.gpu_layers, critic.vram_bytes, critic.slots),
        (0, 0, 1)
    );
    assert!(critic.ram_bytes >= 18 * GIB, "{} bytes", critic.ram_bytes);
    drop(lease);

    // An ordinary model on the same machine goes to the GPU with the configured slots.
    drop(supervisor.acquire("emperor", "agent").await.unwrap());
    let args = log.args("emperor");
    assert!(args.contains("--n-gpu-layers 999"), "{args}");
    assert!(!args.contains("--device"), "{args}");
    assert!(args.contains("--parallel 2"), "{args}");
    assert!(
        !supervisor
            .loaded()
            .iter()
            .any(|m| m.model_id == "emperor" && m.cpu_pinned)
    );
}

#[tokio::test]
async fn cpu_lane_slots_follow_the_row() {
    let (supervisor, log) = start(&hardware(24, 64), 0);
    let lane = CpuLane {
        slots: 3,
        ..CpuLane::default()
    };
    supervisor.set_cpu_pinned(HashMap::from([("critic".to_string(), lane)]));
    drop(supervisor.acquire("critic", "reviewer").await.unwrap());
    let args = log.args("critic");
    assert!(args.contains("--parallel 3"), "{args}");
    assert!(args.contains("--ctx-size 12288"), "{args}");
    assert_eq!(supervisor.loaded()[0].slots, 3);
}

#[tokio::test]
async fn cpu_lane_context_follows_the_row_and_is_what_the_planner_counts() {
    let (supervisor, log) = start(&hardware(24, 64), 0);
    let critic = supervisor
        .models()
        .into_iter()
        .find(|m| m.id == "critic")
        .unwrap();
    let default_kv = supervisor.footprint(&critic).kv_bytes;
    let lane = CpuLane {
        context: Some(16384),
        ..CpuLane::default()
    };
    supervisor.set_cpu_pinned(HashMap::from([("critic".to_string(), lane)]));
    // Four times the settings' 4096 tokens, one slot.
    assert_eq!(supervisor.footprint(&critic).kv_bytes, default_kv * 4);
    drop(supervisor.acquire("critic", "reviewer").await.unwrap());
    let args = log.args("critic");
    assert!(args.contains("--ctx-size 16384"), "{args}");
    assert!(args.contains("--parallel 1"), "{args}");
}

#[tokio::test(start_paused = true)]
async fn cpu_lane_waits_longer_than_the_global_cap_for_a_slot() {
    let (supervisor, _) = start(&hardware(24, 64), 0);
    // The global cap is 600 s. A call queued behind a long CPU task outlasts it...
    let held = supervisor.acquire("critic", "first").await.unwrap();
    let waiting = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("critic", "second").await })
    };
    tokio::time::sleep(Duration::from_secs(1200)).await;
    assert!(!waiting.is_finished(), "still queued past the global cap");
    drop(held);
    assert!(waiting.await.unwrap().is_ok());

    // ...unless the row caps it lower.
    let lane = CpuLane {
        max_wait_s: Some(30),
        ..CpuLane::default()
    };
    supervisor.set_cpu_pinned(HashMap::from([("critic".to_string(), lane)]));
    let held = supervisor.acquire("critic", "first").await.unwrap();
    let error = supervisor.acquire("critic", "third").await.unwrap_err();
    assert!(error.contains("waited 30 s"), "{error}");
    assert!(error.contains("row"), "{error}");
    drop(held);
}

#[tokio::test]
async fn cpu_lane_is_excluded_from_vram_and_never_a_gpu_swap_victim() {
    let (supervisor, log) = start(&hardware(10, 64), 0);
    // The lane loads first and goes idle for longer than anything else.
    drop(supervisor.acquire("critic", "reviewer").await.unwrap());
    drop(supervisor.acquire("emperor", "agent").await.unwrap());
    let loaded = supervisor.loaded();
    let vram: u64 = loaded.iter().map(|m| m.vram_bytes).sum();
    let critic = loaded.iter().find(|m| m.model_id == "critic").unwrap();
    let emperor = loaded.iter().find(|m| m.model_id == "emperor").unwrap();
    assert_eq!(critic.vram_bytes, 0);
    // The GPU model has the whole GPU, as if the lane were not there.
    assert!(emperor.gpu_layers >= 32);
    assert!(
        emperor.vram_bytes > 6 * GIB && vram <= 10 * GIB,
        "{vram} bytes of VRAM"
    );

    // The worker needs the GPU the emperor holds: the idle emperor is swapped out, not the
    // lane, though the lane has been idle the longest.
    assert!(matches!(
        supervisor.availability("worker"),
        Availability::Swap { .. }
    ));
    drop(supervisor.acquire("worker", "agent").await.unwrap());
    let mut ids: Vec<_> = supervisor
        .loaded()
        .into_iter()
        .map(|m| m.model_id)
        .collect();
    ids.sort();
    assert_eq!(ids, ["critic", "worker"]);
    assert_eq!(log.stopped(), ["emperor"], "the lane was left alone");
    assert_eq!(log.launched(), ["critic", "emperor", "worker"]);

    // Swapping back and forth never touches it either.
    drop(supervisor.acquire("emperor", "agent").await.unwrap());
    assert_eq!(log.stopped(), ["emperor", "worker"]);
    assert!(supervisor.loaded().iter().any(|m| m.model_id == "critic"));
}

#[tokio::test]
async fn cpu_lane_is_refused_and_unavailable_when_ram_is_short() {
    // Plenty of VRAM, but the lane needs RAM: 18 GiB of weights do not fit in 16 GiB.
    let (supervisor, log) = start(&hardware(24, 16), 0);
    assert_eq!(supervisor.availability("critic"), Availability::Unavailable);
    let error = supervisor.acquire("critic", "reviewer").await.unwrap_err();
    assert!(error.contains("not enough memory for critic"), "{error}");
    assert!(log.launched().is_empty());
    assert!(supervisor.loaded().is_empty());
    // The same machine serves GPU models as usual.
    assert_eq!(supervisor.availability("emperor"), Availability::Fits);
    drop(supervisor.acquire("emperor", "agent").await.unwrap());

    // Unpinned, the same file would have been loaded (split across the GPU and RAM).
    supervisor.set_cpu_pinned(HashMap::new());
    assert_eq!(supervisor.availability("critic"), Availability::Fits);
}

#[tokio::test]
async fn cpu_lane_and_gpu_models_coexist_within_both_budgets() {
    // 22 GiB of RAM: the lane takes about 19, and the 8 GiB prompt cache asked of each server
    // shrinks to what is left (3 GiB for the lane, then none for the GPU model).
    let hardware = hardware(10, 22);
    let (supervisor, log) = start(&hardware, 8192);
    drop(supervisor.acquire("critic", "reviewer").await.unwrap());
    let lane_cache = log.args("critic");
    let cache_of = |args: &str| {
        let words: Vec<&str> = args.split(' ').collect();
        let at = words
            .iter()
            .position(|word| *word == "--cache-ram")
            .unwrap();
        words[at + 1].parse::<u64>().unwrap()
    };
    let lane_cache_mb = cache_of(&lane_cache);
    assert!(lane_cache_mb > 0 && lane_cache_mb < 8192, "{lane_cache}");

    drop(supervisor.acquire("emperor", "agent").await.unwrap());
    assert_eq!(cache_of(&log.args("emperor")), 0);
    let loaded = supervisor.loaded();
    assert_eq!(loaded.len(), 2, "both stay loaded");
    let budget = supervisor.budget();
    let (vram, ram) = loaded.iter().fold((0, 0), |(vram, ram), m| {
        (vram + m.vram_bytes, ram + m.ram_bytes)
    });
    assert!(vram <= budget.vram_bytes, "{vram} bytes of VRAM committed");
    assert!(ram <= budget.ram_bytes, "{ram} bytes of RAM committed");
    // The lane's cache is part of what it holds.
    let critic = loaded.iter().find(|m| m.model_id == "critic").unwrap();
    assert!(critic.ram_bytes >= 18 * GIB + lane_cache_mb * 1024 * 1024);
}

#[tokio::test]
async fn cpu_lane_listing_shows_a_pinned_row_as_cpu_placement() {
    let (supervisor, _) = start(&hardware(24, 64), 0);
    let critic = supervisor
        .models()
        .into_iter()
        .find(|m| m.id == "critic")
        .unwrap();
    let placement = supervisor.placement_when_idle(&critic).unwrap();
    assert_eq!((placement.gpu_layers, placement.vram_bytes), (0, 0));
    assert!(!placement.all_layers_on_gpu);
    // One slot of context, not the GPU models' two.
    assert!(
        supervisor.footprint(&critic).kv_bytes < supervisor.footprint(&model("same", 18)).kv_bytes
    );
}

#[tokio::test]
async fn cpu_lane_speed_is_measured_from_finished_calls() {
    let (supervisor, _) = start(&hardware(24, 64), 0);
    assert_eq!(supervisor.tokens_per_s("critic"), None);
    // Nothing is recorded for a call that generated nothing.
    supervisor.record_throughput("critic", 0, Duration::from_secs(5));
    supervisor.record_throughput("critic", 50, Duration::ZERO);
    assert_eq!(supervisor.tokens_per_s("critic"), None);

    supervisor.record_throughput("critic", 100, Duration::from_secs(10));
    assert!((supervisor.tokens_per_s("critic").unwrap() - 10.0).abs() < 1e-9);
    // Recent calls weigh more, and a long generation weighs more than a short one.
    supervisor.record_throughput("critic", 600, Duration::from_secs(20));
    let rate = supervisor.tokens_per_s("critic").unwrap();
    assert!(rate > 10.0 && rate < 30.0, "{rate}");
    assert_eq!(supervisor.tokens_per_s("emperor"), None);

    // The speed outlives the server.
    drop(supervisor.acquire("critic", "reviewer").await.unwrap());
    assert!(supervisor.tokens_per_s("critic").is_some());
}

#[tokio::test]
async fn a_cpu_lane_model_another_session_is_using_is_never_evicted_for_ram() {
    // 24 GiB of RAM holds the 18 GiB lane model or a 6 GiB model beside the GPU's, not both
    // the lane and a second RAM-hungry load: the newcomer needs the lane's RAM.
    let (supervisor, log) = start(&hardware(10, 24), 0);
    let turn = supervisor.attach("critic", "first/root");
    drop(supervisor.acquire("critic", "first/root").await.unwrap());
    // A newcomer from another session needs more RAM than is left beside the lane.
    supervisor.set_cpu_pinned(HashMap::from([
        ("critic".to_string(), CpuLane::default()),
        ("worker".to_string(), CpuLane::default()),
    ]));
    let newcomer = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move { supervisor.acquire("worker", "second/root").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!newcomer.is_finished(), "the newcomer waits for the RAM");
    assert!(log.stopped().is_empty(), "{:?}", log.stopped());
    assert!(supervisor.loaded().iter().any(|m| m.model_id == "critic"));
    // Released, the lane model is swapped out and the newcomer loads.
    drop(turn);
    let lease = tokio::time::timeout(Duration::from_secs(5), newcomer)
        .await
        .expect("the newcomer loads once the claim drops")
        .unwrap()
        .unwrap();
    assert_eq!(log.stopped(), ["critic"]);
    drop(lease);
}
