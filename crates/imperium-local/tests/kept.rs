//! Knowledge agents' slots (docs/21-knowledge-agents.md, stage 2): a knowledge agent's holder is
//! `kept` on its model. Its slot is saved after every fork without erasing its cells, restored
//! only when another agent has taken the slot (and kept on disk after the restore), saved when its
//! model is swapped out or unloaded, and trimmed after the files of agents that cannot come back.
//! The `kept` mark pins nothing. llama-server is replaced by in-process fakes that record what
//! they were asked to do.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use imperium_api::{LocalModels, LocalRequest, Priority};
use imperium_config::LlamaCppSettings;
use imperium_local::gguf::GgufInfo;
use imperium_local::hardware::{Gpu, Hardware};
use imperium_local::residency::{Move, Reason};
use imperium_local::scan::{Format, LocalModel};
use imperium_local::supervisor::{
    LaunchFuture, LaunchSpec, Launcher, Runtimes, Server, SlotFuture, SlotTransfer, StopFuture,
    Supervisor,
};

const GIB: u64 = 1024 * 1024 * 1024;
const HOLDER: &str = "session/t1";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap()
}

/// What the fake servers did, in order: `launch <model>`, `save <model> <slot> <file>`,
/// `erase <model> <slot>`, `stop <model>`, `restore <model> <slot> <file>`.
#[derive(Debug, Default)]
struct Log {
    ops: Mutex<Vec<String>>,
    dir: OnceLock<PathBuf>,
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

    fn count(&self, verb: &str) -> usize {
        lock(&self.ops)
            .iter()
            .filter(|op| op.starts_with(verb))
            .count()
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

    /// The file of the latest save.
    fn last_save(&self) -> String {
        lock(&self.ops)
            .iter()
            .rev()
            .find(|op| op.starts_with("save"))
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

    fn exited(&self) -> Option<String> {
        None
    }

    fn save_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(async move {
            std::fs::write(self.log.dir.get().unwrap().join(file), b"kv").unwrap();
            self.log.push(format!("save {} {slot} {file}", self.model));
            Ok(SlotTransfer {
                tokens: 1000,
                bytes: 1024 * 1024,
            })
        })
    }

    fn erase_slot(&self, slot: u32) -> SlotFuture<'_> {
        Box::pin(async move {
            self.log.push(format!("erase {} {slot}", self.model));
            Ok(SlotTransfer::default())
        })
    }

    fn stop(&self) -> StopFuture<'_> {
        Box::pin(async move {
            self.log.push(format!("stop {}", self.model));
        })
    }

    fn restore_slot<'a>(&'a self, slot: u32, file: &'a str) -> SlotFuture<'a> {
        Box::pin(async move {
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
            self.log.push(format!("launch {}", spec.model_id));
            tokio::time::sleep(Duration::from_millis(10)).await;
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
        repo: None,
        format: Format::Gguf,
    }
}

fn settings(swap_cache_mb: u64) -> LlamaCppSettings {
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
        swap_cache_mb,
        min_residency_s: 0,
        max_swaps_per_min: 60,
        swap_gain: 2.0,
        max_wait_s: 600,
        extra_args: Vec::new(),
    }
}

/// A 10 GiB GPU and no spare RAM: one of the two 6 GiB models at a time.
fn start(swap_cache_mb: u64) -> (Arc<Supervisor>, Arc<Log>) {
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
        vram_overridden: false,
    };
    let supervisor = Supervisor::start(
        vec![model("emperor"), model("worker")],
        settings(swap_cache_mb),
        Runtimes::default(),
        &hardware,
        launcher,
    );
    (supervisor, log)
}

/// One model call: takes a slot and releases it; returns the slot it got.
async fn call(supervisor: &Supervisor, model: &str, holder: &str) -> Option<u32> {
    let lease = supervisor
        .acquire(model, holder)
        .await
        .unwrap_or_else(|error| panic!("{holder} could not get {model}: {error}"));
    lease.slot
}

/// Two other agents hold both slots of `model` at once, so whatever prompt either slot held is
/// overwritten.
async fn interfere(supervisor: &Supervisor, model: &str) {
    let small = LocalRequest {
        priority: Priority::Interactive,
        prompt_tokens: Some(16),
        uncapped: false,
    };
    let first = supervisor
        .acquire_with(model, "session/x", small)
        .await
        .unwrap();
    let second = supervisor
        .acquire_with(model, "session/y", small)
        .await
        .unwrap();
    drop((first, second));
}

#[tokio::test]
async fn a_second_question_with_nothing_in_between_is_a_hot_hit() {
    let (supervisor, log) = start(1024);
    let _kept = supervisor.keep("emperor", HOLDER);
    let first = call(&supervisor, "emperor", HOLDER).await;
    supervisor.save_kept("emperor", HOLDER).await;
    assert_eq!(log.steps(), ["launch emperor", "save emperor 0"]);
    assert_eq!(log.files().len(), 1);

    let second = call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(second, first, "the agent gets its own slot back");
    assert_eq!(log.count("restore"), 0, "its cells are still there");
    assert_eq!(log.count("erase"), 0, "saving left them alone");
    assert_eq!(log.files().len(), 1, "the file stays");
}

#[tokio::test]
async fn an_overwritten_slot_is_restored_and_its_file_stays_for_the_next_time() {
    let (supervisor, log) = start(1024);
    let _kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    supervisor.save_kept("emperor", HOLDER).await;
    let file = log.last_save();

    // A third agent uses the model between two questions.
    interfere(&supervisor, "emperor").await;
    call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(log.count("restore"), 1);
    assert_eq!(
        log.files(),
        std::slice::from_ref(&file),
        "the file survives its restore"
    );

    // After more interference it is restored again.
    interfere(&supervisor, "emperor").await;
    call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(log.count("restore"), 2);
    assert_eq!(log.files(), [file]);
}

#[tokio::test]
async fn a_fork_cancelled_mid_answer_still_has_its_slot_saved() {
    let (supervisor, log) = start(1024);
    let _kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    supervisor.save_kept("emperor", HOLDER).await;

    // The next fork is cancelled while it holds its slot; the slot is saved all the same.
    let lease = supervisor.acquire("emperor", HOLDER).await.unwrap();
    drop(lease);
    supervisor.save_kept("emperor", HOLDER).await;
    assert_eq!(log.count("save"), 2);

    interfere(&supervisor, "emperor").await;
    call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(
        log.count("restore"),
        1,
        "the cancelled fork's slot was saved"
    );
}

#[tokio::test]
async fn retiring_or_resuming_removes_the_saved_file() {
    let (supervisor, log) = start(1024);
    let kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    supervisor.save_kept("emperor", HOLDER).await;
    assert_eq!(log.files().len(), 1);

    supervisor.drop_saved(HOLDER);
    drop(kept);
    assert!(log.files().is_empty());
    interfere(&supervisor, "emperor").await;
    call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(log.count("restore"), 0, "there is nothing to restore");
}

#[tokio::test]
async fn kept_slots_are_saved_on_a_swap_and_on_an_idle_unload() {
    // Swapped out for another model's load: the kept holder's slot is saved first.
    let (supervisor, log) = start(1024);
    let _kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    call(&supervisor, "worker", "session/t2").await;
    assert_eq!(
        log.steps(),
        [
            "launch emperor",
            "save emperor 0",
            "stop emperor",
            "launch worker"
        ],
        "the model is not pinned by a kept agent, and its slot is saved"
    );

    // Unloaded when idle: saved too. An agent that is not kept is not.
    let (supervisor, log) = start(1024);
    let kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    call(&supervisor, "emperor", "session/other").await;
    supervisor.unload_idle_now(Duration::ZERO).await;
    assert_eq!(log.count("save"), 1, "only the kept holder's slot is saved");
    assert_eq!(log.count("stop"), 1);
    drop(kept);

    let (supervisor, log) = start(1024);
    call(&supervisor, "emperor", HOLDER).await;
    supervisor.unload_idle_now(Duration::ZERO).await;
    assert_eq!(log.count("save"), 0, "a finished agent's slot is not saved");
    assert_eq!(log.count("stop"), 1);
}

#[tokio::test]
async fn the_kept_mark_is_neither_working_nor_waiting() {
    let (supervisor, log) = start(1024);
    let _kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    let loaded = supervisor.loaded();
    assert!(
        !loaded[0].claimed,
        "a kept agent is not mid-turn on the model"
    );
    assert!(
        loaded[0].idle_for.is_some(),
        "and it does not hold off the idle unload"
    );
    // Another agent using the model does not make the kept agent give its slot away.
    call(&supervisor, "emperor", "session/other").await;
    assert_eq!(log.count("erase"), 0);
    // A working agent, by contrast, claims the model.
    let _turn = supervisor.attach("emperor", "session/other");
    assert!(supervisor.loaded()[0].claimed);
}

#[tokio::test]
async fn trimming_drops_finished_agents_files_before_kept_ones() {
    // Room for one 1 MiB file.
    let (supervisor, log) = start(1);
    // A finished agent's slot was saved while it waited.
    let waiting = supervisor.waiting("emperor", "session/finished");
    call(&supervisor, "emperor", "session/finished").await;
    supervisor.suspend("emperor", "session/finished").await;
    drop(waiting);
    assert_eq!(log.files().len(), 1);

    // The kept agent's save pushes the folder over its capacity: the finished agent's file goes.
    let _kept = supervisor.keep("emperor", HOLDER);
    call(&supervisor, "emperor", HOLDER).await;
    supervisor.save_kept("emperor", HOLDER).await;
    assert_eq!(log.files(), [log.last_save()]);
    assert_eq!(log.count("save"), 2);
}

#[tokio::test]
async fn the_saved_slot_cache_and_kv_size_are_reported() {
    let (supervisor, _log) = start(1024);
    assert_eq!(supervisor.swap_cache_bytes(), Some(1024 * 1024 * 1024));
    // 32 layers x 8 KV heads x 128 per head x 2 (K and V) x 2 bytes (f16).
    assert_eq!(supervisor.kv_bytes_per_token("emperor"), Some(131_072));
    assert_eq!(supervisor.kv_bytes_per_token("missing"), None);
}

#[tokio::test]
async fn a_fork_fits_only_when_a_slot_is_free_for_it() {
    let (supervisor, _log) = start(1024);
    assert!(
        !supervisor.fork_fits("emperor", "emperor"),
        "not loaded yet"
    );
    call(&supervisor, "emperor", "session/asker").await;
    // The asker holds no slot while it waits: even on its own model one free permit is enough.
    assert!(supervisor.fork_fits("emperor", "emperor"));
    let small = LocalRequest {
        priority: Priority::Interactive,
        prompt_tokens: Some(16),
        uncapped: false,
    };
    let busy = supervisor
        .acquire_with("emperor", "session/x", small)
        .await
        .unwrap();
    assert!(
        supervisor.fork_fits("emperor", "emperor"),
        "one of two is free"
    );
    let busier = supervisor
        .acquire_with("emperor", "session/y", small)
        .await
        .unwrap();
    assert!(!supervisor.fork_fits("emperor", "emperor"), "none is free");
    drop((busy, busier));
}

/// Every move the supervisor reports.
fn record_moves(supervisor: &Supervisor) -> Arc<Mutex<Vec<Move>>> {
    let moves = Arc::new(Mutex::new(Vec::new()));
    let mut receiver = supervisor.moves();
    let sink = Arc::clone(&moves);
    tokio::spawn(async move {
        while let Ok(moved) = receiver.recv().await {
            lock(&sink).push(moved);
        }
    });
    moves
}

#[tokio::test]
async fn a_fork_saves_its_slot_under_its_last_lease_before_a_queued_call_takes_it() {
    let (supervisor, log) = start(1024);
    let moves = record_moves(&supervisor);
    let small = LocalRequest {
        priority: Priority::Interactive,
        prompt_tokens: Some(16),
        uncapped: false,
    };
    let _kept = supervisor.keep("emperor", HOLDER);
    // The fork holds one slot, another agent the other, and a third call is queued for a slot.
    let fork = supervisor
        .acquire_with("emperor", HOLDER, small)
        .await
        .unwrap();
    let slot = fork.slot.unwrap();
    let other = supervisor
        .acquire_with("emperor", "session/x", small)
        .await
        .unwrap();
    let queued = {
        let supervisor = Arc::clone(&supervisor);
        tokio::spawn(async move {
            supervisor
                .acquire_with("emperor", "session/y", small)
                .await
                .unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Saved while the lease is still held: the queued call cannot have the slot yet.
    supervisor
        .save_leased("emperor", HOLDER, slot)
        .await
        .unwrap();
    assert_eq!(log.count("save"), 1);
    drop(fork);
    // It takes the slot the moment it is released, and writes over the prefix.
    let taker = queued.await.unwrap();
    assert_eq!(taker.slot, Some(slot));
    drop((taker, other));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let saved = lock(&moves)
        .iter()
        .filter(|moved| moved.action.name() == "save_slot" && moved.ok)
        .map(|moved| moved.reason)
        .collect::<Vec<_>>();
    assert_eq!(saved, [Reason::Kept]);

    // The next question finds its slot overwritten and restores the saved prefix.
    call(&supervisor, "emperor", HOLDER).await;
    assert_eq!(log.count("restore"), 1);
}

#[tokio::test]
async fn a_save_that_cannot_happen_is_a_failed_move_not_a_silent_skip() {
    let (supervisor, log) = start(1024);
    let moves = record_moves(&supervisor);
    // The model is not loaded.
    assert!(!supervisor.save_kept_slot("emperor", HOLDER).await);
    // It is, but the holder's prompt is not in a free slot.
    call(&supervisor, "emperor", "session/other").await;
    assert!(!supervisor.save_kept_slot("emperor", HOLDER).await);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let failed: Vec<(Reason, bool)> = lock(&moves)
        .iter()
        .filter(|moved| moved.action.name() == "save_slot")
        .map(|moved| (moved.reason, moved.ok))
        .collect();
    assert_eq!(failed, [(Reason::Kept, false), (Reason::Kept, false)]);
    assert_eq!(log.count("save"), 0);
}
