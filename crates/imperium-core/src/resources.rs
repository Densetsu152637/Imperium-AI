//! The resource monitor: machine usage (CPU, memory, GPUs, model servers), the local models the
//! supervisor holds, and the hosted endpoints live sessions use.
//!
//! `system.resources` takes one snapshot. `system.resources_subscribe` adds a watcher; while at
//! least one watches, a task samples every [`INTERVAL`] and sends `system.resources_changed` to
//! the watching connections. With nobody watching, nothing is sampled.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use imperium_config::{LLAMACPP_ENDPOINT, VLLM_ENDPOINT};
use imperium_local::monitor::{Probe, Sample, SystemProbe};
use imperium_local::supervisor::LoadedStatus;
use imperium_protocol::methods::{
    ActiveEndpoint, ActiveLocalModel, CpuUsage, GpuUsage, MemoryUsage, ProcessUsage,
    RESOURCES_CHANGED, ResourceSnapshot,
};
use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use tokio::sync::broadcast;

use crate::state::DaemonState;

/// How often watchers get a snapshot.
pub const INTERVAL: Duration = Duration::from_secs(2);
/// How far back endpoint activity is counted.
const WINDOW: Duration = Duration::from_secs(15 * 60);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One model call, or one failed turn, from a session.
#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub at: Instant,
    pub session_id: String,
    /// `<endpoint>:<model>`.
    pub spec: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u64,
    /// Set for a failed turn.
    pub error: Option<String>,
}

/// Samples the machine for watchers and remembers recent model calls.
pub struct ResourceMonitor {
    probe: Arc<Mutex<Box<dyn Probe>>>,
    /// Recent calls, oldest first, at most [`WINDOW`] old.
    calls: Mutex<VecDeque<Call>>,
    watchers: AtomicUsize,
    /// Whether the sampling task is alive.
    running: AtomicBool,
    interval: Mutex<Duration>,
}

impl std::fmt::Debug for ResourceMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceMonitor")
            .field("watchers", &self.watchers.load(Ordering::SeqCst))
            .field("running", &self.running.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl Default for ResourceMonitor {
    fn default() -> Self {
        ResourceMonitor {
            probe: Arc::new(Mutex::new(Box::new(SystemProbe::new()))),
            calls: Mutex::new(VecDeque::new()),
            watchers: AtomicUsize::new(0),
            running: AtomicBool::new(false),
            interval: Mutex::new(INTERVAL),
        }
    }
}

/// A connection's subscription; dropping it unsubscribes.
#[derive(Debug)]
pub struct Watch {
    monitor: Arc<ResourceMonitor>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.monitor.watchers.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ResourceMonitor {
    /// Reads the machine with `probe` instead (for tests).
    pub fn use_probe(&self, probe: Box<dyn Probe>) {
        *lock(&self.probe) = probe;
    }

    /// Samples every `interval` while watched (for tests).
    pub fn set_interval(&self, interval: Duration) {
        *lock(&self.interval) = interval;
    }

    pub fn interval(&self) -> Duration {
        *lock(&self.interval)
    }

    pub fn watchers(&self) -> usize {
        self.watchers.load(Ordering::SeqCst)
    }

    /// Whether the sampling task is alive.
    pub fn sampling(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub(crate) fn record(&self, call: Call) {
        let mut calls = lock(&self.calls);
        calls.push_back(call);
        prune(&mut calls, Instant::now());
    }

    fn recent(&self) -> Vec<Call> {
        let mut calls = lock(&self.calls);
        prune(&mut calls, Instant::now());
        calls.iter().cloned().collect()
    }

    /// Adds a watcher, starting the sampling task when it is the first. Must run inside the
    /// Tokio runtime.
    pub fn watch(self: &Arc<Self>, state: &Arc<DaemonState>) -> Watch {
        self.watchers.fetch_add(1, Ordering::SeqCst);
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tokio::spawn(sample_while_watched(
                Arc::clone(self),
                Arc::downgrade(state),
            ));
        }
        Watch {
            monitor: Arc::clone(self),
        }
    }

    /// Samples the machine on a blocking thread, reading these processes too.
    async fn sample(&self, pids: Vec<u32>) -> Sample {
        let probe = Arc::clone(&self.probe);
        tokio::task::spawn_blocking(move || lock(&probe).sample(&pids))
            .await
            .unwrap_or_default()
    }
}

fn prune(calls: &mut VecDeque<Call>, now: Instant) {
    while calls
        .front()
        .is_some_and(|call| now.duration_since(call.at) > WINDOW)
    {
        calls.pop_front();
    }
}

/// Announces a snapshot every interval until nobody watches or the daemon is gone.
async fn sample_while_watched(monitor: Arc<ResourceMonitor>, daemon: std::sync::Weak<DaemonState>) {
    let mut ticks = tokio::time::interval(monitor.interval());
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        if monitor.watchers() == 0 {
            monitor.running.store(false, Ordering::SeqCst);
            // A watcher that arrived just now saw the task running and did not start another.
            let resumed = monitor.watchers() > 0
                && monitor
                    .running
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok();
            if !resumed {
                return;
            }
        }
        let Some(state) = daemon.upgrade() else {
            monitor.running.store(false, Ordering::SeqCst);
            return;
        };
        let snapshot = snapshot(&state).await;
        state.announce(RESOURCES_CHANGED, snapshot);
    }
}

/// What the machine and the sessions are using now.
pub async fn snapshot(state: &DaemonState) -> ResourceSnapshot {
    let loaded = state.loaded_local_models();
    let daemon_pid = std::process::id();
    let mut pids = vec![daemon_pid];
    pids.extend(loaded.iter().filter_map(|model| model.pid));
    let monitor = state.resource_monitor();
    let sample = monitor.sample(pids).await;
    let sessions: Vec<(String, Option<String>)> = state
        .sessions()
        .iter()
        .map(|session| (session.id().to_string(), session.model()))
        .collect();
    let hosts = state
        .settings()
        .endpoints
        .iter()
        .filter_map(|(id, endpoint)| Some((id.clone(), host(&endpoint.base_url)?)))
        .collect();
    assemble(&Inputs {
        sample,
        daemon_pid,
        loaded: &loaded,
        sessions: &sessions,
        calls: &monitor.recent(),
        hosts: &hosts,
        unified_memory: cfg!(all(target_os = "macos", target_arch = "aarch64")),
        now: Instant::now(),
    })
}

/// Everything a snapshot is made from.
pub(crate) struct Inputs<'a> {
    pub sample: Sample,
    pub daemon_pid: u32,
    pub loaded: &'a [LoadedStatus],
    /// Live sessions and their model specs.
    pub sessions: &'a [(String, Option<String>)],
    pub calls: &'a [Call],
    /// Endpoint id to host.
    pub hosts: &'a HashMap<String, String>,
    pub unified_memory: bool,
    pub now: Instant,
}

pub(crate) fn assemble(inputs: &Inputs<'_>) -> ResourceSnapshot {
    let sample = &inputs.sample;
    let process = |pid: u32, role: &str, model: Option<&str>| {
        let found = sample.processes.iter().find(|p| p.pid == pid)?;
        Some(ProcessUsage {
            pid,
            name: found.name.clone(),
            role: role.into(),
            model: model.map(str::to_string),
            cpu_percent: found.cpu_percent,
            ram_bytes: found.ram_bytes,
            vram_bytes: found.vram_bytes,
        })
    };
    let mut processes: Vec<ProcessUsage> = process(inputs.daemon_pid, "daemon", None)
        .into_iter()
        .collect();
    processes.extend(
        inputs
            .loaded
            .iter()
            .filter_map(|model| process(model.pid?, &model.runtime, Some(&model.model_id))),
    );

    let live: BTreeSet<&str> = inputs.sessions.iter().map(|(id, _)| id.as_str()).collect();
    let recent = || {
        inputs
            .calls
            .iter()
            .filter(|call| inputs.now.saturating_duration_since(call.at) <= WINDOW)
    };
    // Live sessions by the spec they use: their own model, or calls in the window.
    let mut users: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (id, spec) in inputs.sessions {
        if let Some(spec) = spec {
            users.entry(spec).or_default().insert(id);
        }
    }
    for call in recent() {
        let entry = users.entry(&call.spec).or_default();
        if live.contains(call.session_id.as_str()) {
            entry.insert(&call.session_id);
        }
    }

    let local_models = inputs
        .loaded
        .iter()
        .map(|model| {
            let sessions: BTreeSet<&str> = users
                .iter()
                .filter(|(spec, _)| local_model(spec) == Some(model.model_id.as_str()))
                .flat_map(|(_, sessions)| sessions.iter().copied())
                .collect();
            ActiveLocalModel {
                model: model.model_id.clone(),
                runtime: model.runtime.clone(),
                placement: placement(model, inputs.unified_memory).into(),
                gpu_layers: model.gpu_layers,
                context: model.context,
                vram_bytes: model.vram_bytes,
                ram_bytes: model.ram_bytes,
                slots: model.slots,
                slots_in_use: model.slots_in_use,
                sessions: sessions.into_iter().map(str::to_string).collect(),
                pid: model.pid,
                idle_ms: model.idle_for.map(millis),
                unload_in_ms: model.unload_in.map(millis),
                can_unload: model.slots_in_use == 0,
            }
        })
        .collect();

    let endpoints = users
        .iter()
        .filter(|(spec, _)| local_model(spec).is_none())
        .filter_map(|(spec, sessions)| {
            let (endpoint, model) = spec.split_once(':')?;
            let calls: Vec<&Call> = recent().filter(|call| call.spec == *spec).collect();
            if sessions.is_empty() && calls.is_empty() {
                return None;
            }
            let ok = || calls.iter().filter(|call| call.error.is_none());
            let failed: Vec<&&Call> = calls.iter().filter(|call| call.error.is_some()).collect();
            Some(ActiveEndpoint {
                endpoint: endpoint.into(),
                host: inputs.hosts.get(endpoint).cloned(),
                model: model.into(),
                spec: (*spec).to_string(),
                sessions: sessions.iter().map(|id| (*id).to_string()).collect(),
                requests: u32::try_from(ok().count()).unwrap_or(u32::MAX),
                prompt_tokens: ok().map(|call| call.prompt_tokens).sum(),
                completion_tokens: ok().map(|call| call.completion_tokens).sum(),
                last_latency_ms: ok().max_by_key(|call| call.at).map(|call| call.duration_ms),
                errors: u32::try_from(failed.len()).unwrap_or(u32::MAX),
                last_error: failed
                    .iter()
                    .max_by_key(|call| call.at)
                    .and_then(|call| call.error.clone()),
            })
        })
        .collect();

    ResourceSnapshot {
        taken_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| millis(since)),
        cpu: CpuUsage {
            total_percent: sample.cpu_percent,
            per_core: sample.per_core.clone(),
        },
        memory: MemoryUsage {
            ram_used_bytes: sample.ram_used_bytes,
            ram_total_bytes: sample.ram_total_bytes,
            swap_used_bytes: sample.swap_used_bytes,
            swap_total_bytes: sample.swap_total_bytes,
        },
        gpus: sample
            .gpus
            .iter()
            .map(|gpu| GpuUsage {
                index: gpu.index,
                name: gpu.name.clone(),
                vendor: gpu.vendor.clone(),
                utilization_percent: gpu.utilization_percent,
                vram_used_bytes: gpu.vram_used_bytes,
                vram_total_bytes: gpu.vram_total_bytes,
                temperature_c: gpu.temperature_c,
            })
            .collect(),
        processes,
        local_models,
        endpoints,
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The local model id a spec names (`llamacpp:<id>` or `vllm:<id>`).
fn local_model(spec: &str) -> Option<&str> {
    let (endpoint, model) = spec.split_once(':')?;
    (endpoint == LLAMACPP_ENDPOINT || endpoint == VLLM_ENDPOINT).then_some(model)
}

fn placement(model: &LoadedStatus, unified_memory: bool) -> &'static str {
    if model.all_layers_on_gpu || (unified_memory && model.gpu_layers == 0) {
        "gpu"
    } else if model.gpu_layers > 0 {
        "partial"
    } else {
        "cpu"
    }
}

/// The host (and port) of a base URL.
fn host(base_url: &str) -> Option<String> {
    let rest = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next()?;
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    (!host.is_empty()).then(|| host.to_string())
}

/// Records a session's model calls and failed turns for endpoint activity.
pub fn watch_session(
    state: &Arc<DaemonState>,
    session_id: String,
    mut events: broadcast::Receiver<AgentEvent>,
) {
    let daemon = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let Some(state) = daemon.upgrade() else {
                return;
            };
            let call = match event.event {
                SessionEvent::ModelCallCompleted {
                    model,
                    usage,
                    duration_ms,
                    ..
                } => Call {
                    at: Instant::now(),
                    session_id: session_id.clone(),
                    spec: model,
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    duration_ms,
                    error: None,
                },
                SessionEvent::TurnFailed { message } if event.agent_id == ROOT_AGENT => {
                    let Some(spec) = state.session(&session_id).and_then(|s| s.model()) else {
                        continue;
                    };
                    Call {
                        at: Instant::now(),
                        session_id: session_id.clone(),
                        spec,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        duration_ms: 0,
                        error: Some(message),
                    }
                }
                _ => continue,
            };
            state.resource_monitor().record(call);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_config::{Config, Layer, Paths};
    use imperium_local::monitor::{GpuSample, ProcessSample};
    use std::sync::atomic::AtomicU32;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn sample() -> Sample {
        Sample {
            cpu_percent: 25.0,
            per_core: vec![50.0, 0.0],
            ram_used_bytes: 8 * GIB,
            ram_total_bytes: 32 * GIB,
            swap_used_bytes: 0,
            swap_total_bytes: 4 * GIB,
            gpus: vec![GpuSample {
                index: 0,
                name: "Fake GPU".into(),
                vendor: "nvidia".into(),
                utilization_percent: Some(80.0),
                vram_used_bytes: 6 * GIB,
                vram_total_bytes: 16 * GIB,
                temperature_c: None,
            }],
            processes: vec![
                ProcessSample {
                    pid: 1,
                    name: "imperium-core".into(),
                    cpu_percent: 1.5,
                    ram_bytes: GIB / 4,
                    vram_bytes: None,
                },
                ProcessSample {
                    pid: 42,
                    name: "llama-server".into(),
                    cpu_percent: 12.0,
                    ram_bytes: GIB,
                    vram_bytes: Some(5 * GIB),
                },
            ],
        }
    }

    fn loaded(id: &str, pid: Option<u32>, in_use: u32) -> LoadedStatus {
        let idle = (in_use == 0).then(|| Duration::from_secs(60));
        LoadedStatus {
            model_id: id.into(),
            runtime: "llamacpp".into(),
            base_url: "http://127.0.0.1:1/v1".into(),
            pid,
            slots: 2,
            slots_in_use: in_use,
            context: 8192,
            gpu_layers: 20,
            all_layers_on_gpu: false,
            vram_bytes: 5 * GIB,
            ram_bytes: GIB,
            idle_for: idle,
            unload_in: idle.map(|idle| Duration::from_secs(900) - idle),
        }
    }

    fn call(session: &str, spec: &str, ago: Duration, error: Option<&str>, now: Instant) -> Call {
        Call {
            at: now - ago,
            session_id: session.into(),
            spec: spec.into(),
            prompt_tokens: 100,
            completion_tokens: 10,
            duration_ms: u64::try_from(ago.as_secs()).unwrap(),
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn a_snapshot_joins_machine_models_sessions_and_calls() {
        let now = Instant::now() + Duration::from_secs(3600);
        let sessions = vec![
            ("s1".to_string(), Some("llamacpp:qwen".to_string())),
            ("s2".to_string(), Some("openai:gpt".to_string())),
            ("s3".to_string(), None),
        ];
        let calls = vec![
            call("s2", "openai:gpt", Duration::from_secs(120), None, now),
            call("s2", "openai:gpt", Duration::from_secs(60), None, now),
            call(
                "s2",
                "openai:gpt",
                Duration::from_secs(30),
                Some("rate limited"),
                now,
            ),
            // A subagent of s3 on a catalogue model, and the local model.
            call("s3", "anthropic:claude", Duration::from_secs(10), None, now),
            call("s3", "llamacpp:qwen", Duration::from_secs(5), None, now),
            // Too old, and from a session that is gone.
            call("s2", "openai:gpt", Duration::from_secs(3000), None, now),
            call("gone", "groq:llama", Duration::from_secs(5), None, now),
        ];
        let hosts = HashMap::from([("openai".to_string(), "api.openai.com".to_string())]);
        let loaded = vec![loaded("qwen", Some(42), 1), loaded("idle", None, 0)];
        let snapshot = assemble(&Inputs {
            sample: sample(),
            daemon_pid: 1,
            loaded: &loaded,
            sessions: &sessions,
            calls: &calls,
            hosts: &hosts,
            unified_memory: false,
            now,
        });
        assert_eq!(snapshot.cpu.per_core, vec![50.0, 0.0]);
        assert_eq!(snapshot.memory.ram_total_bytes, 32 * GIB);
        assert_eq!(snapshot.gpus[0].utilization_percent, Some(80.0));
        let roles: Vec<_> = snapshot.processes.iter().map(|p| p.role.as_str()).collect();
        assert_eq!(roles, ["daemon", "llamacpp"]);
        assert_eq!(snapshot.processes[1].model.as_deref(), Some("qwen"));
        assert_eq!(snapshot.processes[1].vram_bytes, Some(5 * GIB));

        let qwen = &snapshot.local_models[0];
        assert_eq!(qwen.placement, "partial");
        assert_eq!(qwen.sessions, ["s1", "s3"]);
        assert!(!qwen.can_unload);
        assert_eq!(qwen.unload_in_ms, None);
        let idle = &snapshot.local_models[1];
        assert!(idle.sessions.is_empty() && idle.can_unload);
        assert_eq!(idle.idle_ms, Some(60_000));
        assert_eq!(idle.unload_in_ms, Some(840_000));

        let specs: Vec<_> = snapshot.endpoints.iter().map(|e| e.spec.as_str()).collect();
        assert_eq!(specs, ["anthropic:claude", "groq:llama", "openai:gpt"]);
        let openai = &snapshot.endpoints[2];
        assert_eq!(openai.host.as_deref(), Some("api.openai.com"));
        assert_eq!(openai.sessions, ["s2"]);
        assert_eq!((openai.requests, openai.prompt_tokens), (2, 200));
        assert_eq!(openai.last_latency_ms, Some(60));
        assert_eq!(openai.errors, 1);
        assert_eq!(openai.last_error.as_deref(), Some("rate limited"));
        // Calls from a session that ended still count, but name no session.
        assert!(snapshot.endpoints[1].sessions.is_empty());
        assert_eq!(snapshot.endpoints[1].requests, 1);
    }

    #[test]
    fn hosts_come_from_base_urls() {
        assert_eq!(
            host("https://api.openai.com/v1").as_deref(),
            Some("api.openai.com")
        );
        assert_eq!(
            host("http://user@10.0.0.2:8080/v1?x").as_deref(),
            Some("10.0.0.2:8080")
        );
        assert_eq!(host(""), None);
    }

    /// Counts its samples.
    struct Counting(Arc<AtomicU32>);

    impl Probe for Counting {
        fn sample(&mut self, _pids: &[u32]) -> Sample {
            self.0.fetch_add(1, Ordering::SeqCst);
            sample()
        }
    }

    #[tokio::test]
    async fn sampling_runs_only_while_someone_watches() {
        let settings = Config::from_layers([Layer::defaults()]).settings().unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = DaemonState::new(Paths::under(home.path()), settings, None);
        let samples = Arc::new(AtomicU32::new(0));
        let monitor = state.resource_monitor();
        monitor.use_probe(Box::new(Counting(Arc::clone(&samples))));
        monitor.set_interval(Duration::from_millis(20));
        let mut notices = state.announcements();

        // A one-off snapshot samples once and starts nothing.
        let snapshot = snapshot(&state).await;
        assert_eq!(snapshot.gpus.len(), 1);
        assert_eq!(samples.load(Ordering::SeqCst), 1);
        assert!(!monitor.sampling());

        let first = monitor.watch(&state);
        let second = monitor.watch(&state);
        assert_eq!(monitor.watchers(), 2);
        let notice = tokio::time::timeout(Duration::from_secs(5), notices.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(notice.method, RESOURCES_CHANGED);
        drop(first);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(monitor.sampling(), "one watcher remains");
        drop(second);
        tokio::time::timeout(Duration::from_secs(5), async {
            while monitor.sampling() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let stopped_at = samples.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            samples.load(Ordering::SeqCst),
            stopped_at,
            "no sampling unwatched"
        );

        // Watching again starts a new task.
        let again = monitor.watch(&state);
        assert!(monitor.sampling());
        drop(again);
    }
}
