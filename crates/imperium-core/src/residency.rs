//! The residency controller: the daemon's one owner of what local models and KV caches stay in
//! memory. The local runtime (`imperium_local::supervisor`) carries its decisions out and starts
//! nothing on its own.
//!
//! - **Plan, then act.** On every change the runtime signals, and at least every
//!   [`TICK`], the controller reads the loaded models and plans ([`plan`]): models whose
//!   settings changed restart once nobody uses them ([`Reason::Config`]), and models nobody has
//!   used for `idle_unload_s` unload ([`Reason::Idle`]). The runtime executes each action and
//!   refuses anything that would move a model or slot in use.
//! - **Calls ask it.** A call whose model is not loaded asks the controller's policy
//!   ([`CorePolicy`]) what to swap out, under the arbiter's rules: never a model in use, the GPU
//!   rule (nothing runs on the CPU while nothing generates; the critic is the exception), the
//!   swap limits and `swap_gain`. A waiting agent yields its KV cells to another agent on the
//!   same model ([`Reason::Yield`]); an agent that goes on gets its context back first
//!   ([`Reason::Resume`]).
//! - **The budget follows the machine.** Every [`BUDGET_CHECK`] it reads the free VRAM (and,
//!   while nothing is loaded, the available RAM) and moves the budget when it changed by more
//!   than [`BUDGET_STEP`] ([`refreshed_budget`]), so memory another program took or gave back
//!   is planned around.
//! - **Every move says why.** The runtime reports each move with its reason; the daemon sends
//!   them to clients as `model.residency`.

use std::sync::{Arc, Weak};
use std::time::Duration;

use imperium_config::HardwareSettings;
use imperium_local::arbiter::Decision;
use imperium_local::hardware::{Hardware, probe};
use imperium_local::planner::Budget;
use imperium_local::residency::{Action, LoadAsk, Reason, ResidencyPolicy, Standard};
use imperium_local::supervisor::{LoadedStatus, Supervisor};

/// How often the controller looks again when nothing signals a change.
pub const TICK: Duration = Duration::from_secs(15);
/// How often the free memory is measured again.
pub const BUDGET_CHECK: Duration = Duration::from_secs(30);
/// The least change in free memory that moves the budget, so measurement noise does not.
pub const BUDGET_STEP: u64 = 512 * 1024 * 1024;

/// Reads how much memory is free now.
pub trait MemoryProbe: Send + Sync + std::fmt::Debug {
    /// The machine as it is now; `None` when it cannot be read.
    fn read(&self) -> Option<Hardware>;
}

/// Reads the machine as the daemon's start did (`nvidia-smi` and `sysinfo`), with the
/// `[hardware]` overrides applied.
#[derive(Debug, Clone)]
pub struct MachineProbe {
    pub overrides: HardwareSettings,
}

impl MemoryProbe for MachineProbe {
    fn read(&self) -> Option<Hardware> {
        Some(probe(&self.overrides))
    }
}

/// One thing the controller wants done, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub action: Action,
    pub reason: Reason,
}

/// What to do now about the loaded models: restart those whose settings changed once nobody
/// uses them, and unload those unused for `idle_unload`. A model with a slot in use, a call
/// queued, or an agent of any session mid-turn on it is left alone.
pub fn plan(loaded: &[LoadedStatus], idle_unload: Duration) -> Vec<Planned> {
    loaded
        .iter()
        .filter(|model| model.slots_in_use == 0 && model.queued == 0 && !model.claimed)
        .filter_map(|model| {
            let unload = Action::Unload {
                model: model.model_id.clone(),
            };
            if model.stale {
                Some(Planned {
                    action: unload,
                    reason: Reason::Config,
                })
            } else if model.idle_for.is_some_and(|idle| idle >= idle_unload) {
                Some(Planned {
                    action: unload,
                    reason: Reason::Idle,
                })
            } else {
                None
            }
        })
        .collect()
}

/// The budget to plan against after reading `fresh`, or `None` when it stays as it is.
///
/// VRAM is what is free now plus what the loaded models hold (they are Imperium's own), so it
/// follows what other programs take or give back. RAM is read only while nothing is loaded:
/// weights are memory-mapped and prompt caches fill lazily, so the free RAM beside a loaded
/// model says little about what it will use. A reading with no GPU where the start found one
/// (a failed `nvidia-smi`) leaves the VRAM as it is. Changes under [`BUDGET_STEP`] are noise.
pub fn refreshed_budget(
    current: Budget,
    start: Budget,
    fresh: &Hardware,
    ram_headroom_bytes: u64,
    loaded: &[LoadedStatus],
) -> Option<Budget> {
    let measured = Budget::of(fresh, ram_headroom_bytes);
    let held_vram: u64 = loaded.iter().map(|model| model.vram_bytes).sum();
    let vram = if fresh.gpus.is_empty() && start.vram_bytes > 0 {
        current.vram_bytes
    } else {
        measured.vram_bytes.saturating_add(held_vram)
    };
    let ram = if loaded.is_empty() {
        measured.ram_bytes
    } else {
        current.ram_bytes
    };
    let moved = |now: u64, was: u64| now.abs_diff(was) >= BUDGET_STEP;
    (moved(vram, current.vram_bytes) || moved(ram, current.ram_bytes)).then_some(Budget {
        vram_bytes: vram,
        ram_bytes: ram,
        unified: current.unified,
    })
}

/// The controller's answer to a call that needs room: the arbiter's rules, with each eviction
/// logged with its reason.
#[derive(Debug, Default)]
pub struct CorePolicy;

impl ResidencyPolicy for CorePolicy {
    fn decide(&self, ask: &LoadAsk<'_>) -> Decision {
        let decision = Standard.decide(ask);
        if let Decision::Load { evict, why, .. } = &decision
            && !evict.is_empty()
        {
            tracing::info!(model = ask.model, holder = ask.holder, evict = ?evict, reason = why.name(), "making room for a local model");
        }
        decision
    }
}

/// Owns residency for one local runtime: installs [`CorePolicy`], and runs the plan loop.
#[derive(Debug)]
pub struct ResidencyController {
    supervisor: Weak<Supervisor>,
    /// Reads free memory; `None` keeps the budget measured at start (tests).
    probe: Option<Arc<dyn MemoryProbe>>,
    ram_headroom_bytes: u64,
}

impl ResidencyController {
    /// A controller for `supervisor`, which from now on asks it how calls make room.
    pub fn new(supervisor: &Arc<Supervisor>, probe: Option<Arc<dyn MemoryProbe>>) -> Arc<Self> {
        supervisor.set_policy(Arc::new(CorePolicy));
        Arc::new(ResidencyController {
            supervisor: Arc::downgrade(supervisor),
            probe,
            ram_headroom_bytes: supervisor.settings().ram_headroom_mb * 1024 * 1024,
        })
    }

    /// Starts the plan loop on the current Tokio runtime, if there is one. It ends when the
    /// supervisor or the controller is dropped.
    pub fn start(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let Some(supervisor) = self.supervisor.upgrade() else {
            return;
        };
        let changes = supervisor.changes();
        let me = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut measured = tokio::time::Instant::now();
            loop {
                // Listen before looking, so a change between the two is not missed.
                let changed = changes.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                let Some(controller) = me.upgrade() else {
                    return;
                };
                controller.tick().await;
                if measured.elapsed() >= BUDGET_CHECK {
                    measured = tokio::time::Instant::now();
                    controller.refresh_budget().await;
                }
                drop(controller);
                let _ = tokio::time::timeout(TICK, changed).await;
            }
        });
    }

    /// Plans from the loaded models and carries the plan out; returns what was done.
    pub async fn tick(&self) -> Vec<Planned> {
        let Some(supervisor) = self.supervisor.upgrade() else {
            return Vec::new();
        };
        let idle_unload = Duration::from_secs(supervisor.settings().idle_unload_s);
        let mut done = Vec::new();
        for planned in plan(&supervisor.loaded(), idle_unload) {
            match supervisor
                .execute(planned.action.clone(), planned.reason)
                .await
            {
                Ok(()) => done.push(planned),
                // Taken up again meanwhile: the next look decides afresh.
                Err(error) => {
                    tracing::debug!(action = ?planned.action, reason = planned.reason.name(), %error, "a planned move did not happen");
                }
            }
        }
        done
    }

    /// Measures the free memory and moves the budget if it changed; returns whether it did.
    pub async fn refresh_budget(&self) -> bool {
        let (Some(probe), Some(supervisor)) = (self.probe.clone(), self.supervisor.upgrade())
        else {
            return false;
        };
        // `nvidia-smi` and `sysinfo` block.
        let Ok(Some(fresh)) = tokio::task::spawn_blocking(move || probe.read()).await else {
            return false;
        };
        let loaded = supervisor.loaded();
        let Some(budget) = refreshed_budget(
            supervisor.budget(),
            supervisor.start_budget(),
            &fresh,
            self.ram_headroom_bytes,
            &loaded,
        ) else {
            return false;
        };
        supervisor
            .execute(
                Action::SetBudget {
                    vram_bytes: budget.vram_bytes,
                    ram_bytes: budget.ram_bytes,
                },
                Reason::Budget,
            )
            .await
            .is_ok()
    }

    /// Unloads `model` now for `models.unload`, saving its slots, unless something uses it.
    pub async fn unload(&self, model: &str) -> Result<(), String> {
        let supervisor = self
            .supervisor
            .upgrade()
            .ok_or_else(|| format!("{model} is not loaded: the local runtime has stopped"))?;
        supervisor
            .execute(
                Action::Unload {
                    model: model.to_string(),
                },
                Reason::Request,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_local::hardware::Gpu;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn status(id: &str) -> LoadedStatus {
        LoadedStatus {
            model_id: id.into(),
            runtime: "llamacpp".into(),
            base_url: String::new(),
            pid: None,
            slots: 4,
            slots_in_use: 0,
            queued: 0,
            gpu_layers: 32,
            all_layers_on_gpu: true,
            vram_bytes: 4 * GIB,
            ram_bytes: 0,
            idle_for: Some(Duration::from_secs(10)),
            unload_in: None,
            claimed: false,
            cpu_pinned: false,
            context: 8192,
            pool_tokens: 8192,
            pool_reserved_tokens: 0,
            stale: false,
        }
    }

    #[test]
    fn the_plan_unloads_idle_models_and_restarts_stale_ones_only_when_nobody_uses_them() {
        let idle = LoadedStatus {
            idle_for: Some(Duration::from_secs(900)),
            ..status("idle")
        };
        let recent = status("recent");
        let stale = LoadedStatus {
            stale: true,
            ..status("stale")
        };
        let busy_stale = LoadedStatus {
            stale: true,
            slots_in_use: 1,
            idle_for: None,
            ..status("busy")
        };
        let claimed_idle = LoadedStatus {
            claimed: true,
            idle_for: None,
            ..status("claimed")
        };
        let queued = LoadedStatus {
            queued: 1,
            ..idle.clone()
        };
        let planned = plan(
            &[idle, recent, stale, busy_stale, claimed_idle, queued],
            Duration::from_secs(900),
        );
        assert_eq!(
            planned,
            [
                Planned {
                    action: Action::Unload {
                        model: "idle".into()
                    },
                    reason: Reason::Idle,
                },
                Planned {
                    action: Action::Unload {
                        model: "stale".into()
                    },
                    reason: Reason::Config,
                },
            ]
        );
    }

    fn machine(vram_free_gib: u64, ram_gib: u64) -> Hardware {
        Hardware {
            gpus: vec![Gpu {
                name: "gpu".into(),
                vendor: "nvidia".into(),
                vram_total_bytes: 16 * GIB,
                vram_free_bytes: vram_free_gib * GIB,
                driver: None,
            }],
            ram_total_bytes: 64 * GIB,
            ram_available_bytes: ram_gib * GIB,
            cpu_cores: 8,
            unified_memory: false,
            overridden: false,
        }
    }

    fn budget(vram_gib: u64, ram_gib: u64) -> Budget {
        Budget {
            vram_bytes: vram_gib * GIB,
            ram_bytes: ram_gib * GIB,
            unified: false,
        }
    }

    #[test]
    fn the_budget_follows_what_other_programs_hold_and_counts_imperiums_own_models() {
        let start = budget(14, 40);
        // Another program took 6 GiB of the GPU; one of our models holds 4 GiB of it.
        let fresh = machine(4, 30);
        let loaded = [status("ours")];
        assert_eq!(
            refreshed_budget(start, start, &fresh, 0, &loaded),
            // VRAM: 4 free + 4 ours. RAM is not read while a model is loaded.
            Some(budget(8, 40))
        );
        // It gave the memory back and our model unloaded: both come back.
        assert_eq!(
            refreshed_budget(budget(8, 40), start, &machine(14, 40), 0, &[]),
            Some(start)
        );
        // Noise under the step changes nothing.
        let noisy = Hardware {
            gpus: vec![Gpu {
                vram_free_bytes: 14 * GIB - 100 * 1024 * 1024,
                ..machine(0, 0).gpus[0].clone()
            }],
            ..machine(0, 40)
        };
        assert_eq!(refreshed_budget(start, start, &noisy, 0, &[]), None);
    }

    #[test]
    fn a_failed_gpu_reading_keeps_the_vram_budget() {
        let start = budget(14, 40);
        let no_gpu = Hardware {
            gpus: Vec::new(),
            ..machine(0, 20)
        };
        // RAM moved (nothing loaded), VRAM stays.
        assert_eq!(
            refreshed_budget(start, start, &no_gpu, 0, &[]),
            Some(budget(14, 20))
        );
    }

    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use imperium_api::LocalModels;
    use imperium_config::{LlamaCppSettings, LocalModelSettings};
    use imperium_local::gguf::GgufInfo;
    use imperium_local::residency::Move;
    use imperium_local::scan::{Format, LocalModel};
    use imperium_local::supervisor::{LaunchFuture, LaunchSpec, Launcher, Runtimes, Server};

    #[derive(Debug)]
    struct FakeLauncher;

    struct FakeServer(String);

    impl Server for FakeServer {
        fn base_url(&self) -> &str {
            &self.0
        }
    }

    impl Launcher for FakeLauncher {
        fn launch<'a>(&'a self, spec: &'a LaunchSpec) -> LaunchFuture<'a> {
            Box::pin(async move {
                Ok(
                    Box::new(FakeServer(format!("http://fake/{}/v1", spec.model_id)))
                        as Box<dyn Server>,
                )
            })
        }
    }

    fn model(id: &str) -> LocalModel {
        LocalModel {
            id: id.into(),
            path: PathBuf::from(format!("{id}.gguf")),
            parts: 1,
            size_bytes: 4 * GIB,
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

    fn settings(idle_unload_s: u64) -> LlamaCppSettings {
        LlamaCppSettings {
            backend: "auto".into(),
            custom_path: None,
            find_existing: true,
            auto_install: false,
            update_external: "off".into(),
            idle_unload_s,
            context: 4096.into(),
            parallel: 2,
            cache_type: "f16".into(),
            slots: Some(2),
            pool_tokens: None,
            flash_attn: "auto".into(),
            max_output_tokens: 8192,
            models: BTreeMap::new(),
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

    fn start(idle_unload_s: u64) -> Arc<Supervisor> {
        Supervisor::start(
            vec![model("a"), model("b")],
            settings(idle_unload_s),
            Runtimes::default(),
            &machine(10, 0),
            Arc::new(FakeLauncher),
        )
    }

    fn drain(moves: &mut tokio::sync::broadcast::Receiver<Move>) -> Vec<(String, Reason)> {
        let mut seen = Vec::new();
        while let Ok(moved) = moves.try_recv() {
            seen.push((
                format!(
                    "{} {}",
                    moved.action.name(),
                    moved.action.model().unwrap_or("-")
                ),
                moved.reason,
            ));
        }
        seen
    }

    #[tokio::test]
    async fn idle_models_unload_only_when_the_controller_says_so_and_say_why() {
        let supervisor = start(0);
        let controller = ResidencyController::new(&supervisor, None);
        let mut moves = supervisor.moves();
        drop(supervisor.acquire("a", "s/root").await.unwrap());
        // Mid-turn on it: the controller leaves it.
        let turn = supervisor.attach("a", "s/root");
        assert!(controller.tick().await.is_empty());
        assert_eq!(supervisor.loaded().len(), 1);
        drop(turn);
        assert_eq!(
            controller.tick().await,
            [Planned {
                action: Action::Unload { model: "a".into() },
                reason: Reason::Idle,
            }]
        );
        assert!(supervisor.loaded().is_empty());
        assert_eq!(
            drain(&mut moves),
            [
                ("load a".to_string(), Reason::Call),
                ("unload a".to_string(), Reason::Idle),
            ]
        );
    }

    #[tokio::test]
    async fn the_running_controller_unloads_idle_models_on_its_own_loop() {
        let supervisor = start(0);
        let controller = ResidencyController::new(&supervisor, None);
        controller.start();
        drop(supervisor.acquire("a", "s/root").await.unwrap());
        tokio::time::timeout(Duration::from_secs(5), async {
            while !supervisor.loaded().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the controller unloads the idle model");
    }

    #[tokio::test]
    async fn settings_changes_restart_a_model_once_nobody_uses_it() {
        let supervisor = start(900);
        let controller = ResidencyController::new(&supervisor, None);
        let mut moves = supervisor.moves();
        let lease = supervisor.acquire("a", "s/root").await.unwrap();
        supervisor.set_model_settings(BTreeMap::from([(
            "a".to_string(),
            LocalModelSettings {
                slots: Some(4),
                ..LocalModelSettings::default()
            },
        )]));
        assert!(supervisor.loaded()[0].stale);
        // In use: it stays.
        assert!(controller.tick().await.is_empty());
        drop(lease);
        assert_eq!(
            controller.tick().await,
            [Planned {
                action: Action::Unload { model: "a".into() },
                reason: Reason::Config,
            }]
        );
        assert!(supervisor.loaded().is_empty());
        assert!(drain(&mut moves).contains(&("unload a".to_string(), Reason::Config)));
    }

    #[tokio::test]
    async fn models_unload_goes_through_the_controller_and_refuses_a_model_in_use() {
        let supervisor = start(900);
        let controller = ResidencyController::new(&supervisor, None);
        let mut moves = supervisor.moves();
        let lease = supervisor.acquire("a", "s/root").await.unwrap();
        let error = controller.unload("a").await.unwrap_err();
        assert!(error.contains("in use"), "{error}");
        drop(lease);
        controller.unload("a").await.unwrap();
        assert!(drain(&mut moves).contains(&("unload a".to_string(), Reason::Request)));
    }

    /// Reads the machine as the test sets it.
    #[derive(Debug)]
    struct Probe(std::sync::Mutex<Hardware>);

    impl MemoryProbe for Probe {
        fn read(&self) -> Option<Hardware> {
            Some(self.0.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn the_budget_is_refreshed_from_live_readings() {
        let supervisor = start(900);
        let probe = Arc::new(Probe(std::sync::Mutex::new(machine(10, 0))));
        let controller = ResidencyController::new(
            &supervisor,
            Some(Arc::clone(&probe) as Arc<dyn MemoryProbe>),
        );
        let mut moves = supervisor.moves();
        // Nothing changed.
        assert!(!controller.refresh_budget().await);
        // Another program took 7 GiB of the GPU: a 4 GiB model no longer fits.
        *probe.0.lock().unwrap() = machine(3, 0);
        assert!(controller.refresh_budget().await);
        assert_eq!(supervisor.budget().vram_bytes, 3 * GIB);
        let error = supervisor.acquire("a", "s/root").await.unwrap_err();
        assert!(error.contains("not enough memory"), "{error}");
        // It gave the memory back.
        *probe.0.lock().unwrap() = machine(10, 0);
        assert!(controller.refresh_budget().await);
        drop(supervisor.acquire("a", "s/root").await.unwrap());
        assert_eq!(
            drain(&mut moves),
            [
                ("set_budget -".to_string(), Reason::Budget),
                ("set_budget -".to_string(), Reason::Budget),
                ("load a".to_string(), Reason::Call),
            ]
        );
    }
}
