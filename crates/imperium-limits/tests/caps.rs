//! The caps hold on this OS (plan stage 1; goals 2 to 4 and 9). Every probe has a hard ceiling
//! of its own (four times the cap, at most 1 GiB), and exits 3 at it: a cap that fails to hold
//! fails the test rather than exhausting the runner. The probes run one at a time.
//!
//! On Linux the cgroup tests need a delegated subtree, named by `IMPERIUM_TEST_CGROUP` (CI's
//! `limits` job creates one with `sudo`). Without it they are skipped, unless
//! `IMPERIUM_REQUIRE_CGROUP=1`. Lines starting `MEASURE` are the stage 1 measurements.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use imperium_limits::{Caps, Class, Config, Ending, Limit, Limits, Unit, UnitSpec};
use tokio::io::{AsyncBufReadExt as _, BufReader};

const PROBE: &str = env!("CARGO_BIN_EXE_limit_probe");
const MIB: u64 = 1 << 20;

/// One probe at a time: they are sized against the runner, not against each other.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn spec(caps: Caps) -> UnitSpec {
    UnitSpec {
        class: Class::Command,
        caps,
        session: None,
        label: "probe".to_owned(),
    }
}

#[cfg(target_os = "linux")]
fn launcher() -> imperium_limits::Launcher {
    imperium_limits::Launcher {
        program: PROBE.into(),
        args: vec!["launch".into()],
    }
}

/// The limits of this host's main mechanism: a job, a cgroup (if the subtree is there) or the
/// poll. `None` when a Linux test has no subtree and may skip.
fn limits(test: &str) -> Option<Limits> {
    #[cfg(target_os = "linux")]
    {
        let Some(parent) = std::env::var_os("IMPERIUM_TEST_CGROUP") else {
            assert!(
                std::env::var("IMPERIUM_REQUIRE_CGROUP").as_deref() != Ok("1"),
                "IMPERIUM_REQUIRE_CGROUP=1 but IMPERIUM_TEST_CGROUP is not set"
            );
            eprintln!("skipped: {test} needs IMPERIUM_TEST_CGROUP");
            return None;
        };
        let root = imperium_limits::cgroup::prepare_child(std::path::Path::new(&parent), test)
            .expect("the test's cgroup root");
        Some(
            Limits::new(Config {
                cgroup_root: Some(root),
                launcher: Some(launcher()),
                ..Config::default()
            })
            .expect("limits on the delegated subtree"),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = test;
        Some(Limits::new(Config::default()).expect("limits"))
    }
}

/// A started probe and the lines it printed so far.
struct Probe {
    unit: Unit,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Probe {
    fn start(limits: &Limits, spec: UnitSpec, args: &[&str]) -> Self {
        let mut command = limits.command(spec, PROBE).expect("the unit");
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        let mut unit = command.spawn().expect("the probe starts in its unit");
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stdout = unit.stdout.take().expect("piped stdout");
        let sink = Arc::clone(&lines);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                sink.lock().unwrap().push(line);
            }
        });
        Self { unit, lines }
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// The last `allocated <MiB>` the probe printed.
    fn allocated(&self) -> u64 {
        self.lines()
            .iter()
            .rev()
            .find_map(|line| line.strip_prefix("allocated ")?.parse().ok())
            .unwrap_or(0)
    }

    async fn ended(&mut self, within: Duration) -> Ending {
        tokio::time::timeout(within, self.unit.ended())
            .await
            .unwrap_or_else(|_| {
                self.unit.stop();
                panic!(
                    "the probe did not end within {within:?}; it printed {:?}",
                    self.lines()
                )
            })
    }

    /// Waits until the probe prints a line starting with `prefix`.
    #[cfg_attr(not(windows), allow(dead_code))]
    async fn wait_for(&self, prefix: &str, within: Duration) -> String {
        let deadline = Instant::now() + within;
        loop {
            if let Some(line) = self
                .lines()
                .into_iter()
                .find(|line| line.starts_with(prefix))
            {
                return line;
            }
            assert!(
                Instant::now() < deadline,
                "no {prefix:?} line within {within:?}: {:?}",
                self.lines()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the unit holds no process.
    async fn wait_empty(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.empty() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "processes outlived the unit: {:?}",
                self.unit.usage()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn empty(&self) -> bool {
        self.unit.usage().processes == 0
    }

    /// macOS: nothing is left in the probe's process group.
    #[cfg(target_os = "macos")]
    fn empty(&self) -> bool {
        let group = self.unit.id().to_string();
        let status = std::process::Command::new("pgrep")
            .args(["-g", &group])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("pgrep");
        status.code() == Some(1)
    }
}

/// Goal 2: an allocation bomb in 64 MiB steps under a 256 MiB unit is stopped, and its peak is at
/// most the cap (on macOS, at most the cap plus the poll's overshoot).
#[tokio::test]
async fn an_allocation_bomb_is_stopped_at_its_cap() {
    let _serial = SERIAL.lock().await;
    let Some(limits) = limits("alloc") else {
        return;
    };
    let cap = 256 * MIB;
    let caps = Caps {
        memory_bytes: Some(cap),
        ..Caps::default()
    };
    let mut probe = Probe::start(
        &limits,
        spec(caps),
        &["alloc", "64", "1024", "--pause-ms", "50"],
    );
    let ending = probe.ended(Duration::from_secs(60)).await;
    let Ending::Limited { limit, value, peak } = ending else {
        panic!(
            "not stopped by its cap: {ending:?}; printed {:?}",
            probe.lines()
        );
    };
    assert_eq!((limit, value), (Limit::Memory, cap));
    let allocated = probe.allocated();
    println!(
        "MEASURE alloc mechanism={} cap_mib=256 peak_mib={} last_allocated_mib={allocated} overshoot_mib={}",
        limits.mechanism().as_str(),
        peak / MIB,
        (peak / MIB).saturating_sub(256),
    );
    if cfg!(target_os = "macos") {
        // 64 MiB every 50 ms against a 100 ms poll: at most three steps past the cap.
        assert!(peak <= cap + 3 * 64 * MIB, "peak {peak} too far over {cap}");
    } else if cfg!(windows) {
        // The job checks its limit against each commit request, but its peak
        // (`PeakJobMemoryUsed`) also counts what the kernel charges without one (page tables,
        // stack pages): it read 1.25 MiB over the limit on the runner. 2 MiB of slack is far
        // under the probe's 64 MiB step, so a limit that let one more step through still fails.
        assert!(peak <= cap + 2 * MIB, "peak {peak} over the cap {cap}");
    } else {
        assert!(peak <= cap, "peak {peak} over the cap {cap}");
    }
    assert!(!probe.lines().iter().any(|line| line == "CEILING"));
    probe.wait_empty().await;
}

/// Goal 3: a fork bomb is stopped at its cap, and nothing of it is left.
#[tokio::test]
async fn a_fork_bomb_is_stopped_at_its_cap() {
    let _serial = SERIAL.lock().await;
    let Some(limits) = limits("fork") else { return };
    let (caps, expected, ceiling) = if cfg!(target_os = "linux") {
        (
            Caps {
                tasks: Some(256),
                ..Caps::default()
            },
            (Limit::Tasks, 256),
            "1024",
        )
    } else {
        (
            Caps {
                processes: Some(32),
                ..Caps::default()
            },
            (Limit::Processes, 32),
            "128",
        )
    };
    let mut probe = Probe::start(&limits, spec(caps), &["fork", ceiling]);
    let ending = probe.ended(Duration::from_secs(60)).await;
    let Ending::Limited { limit, value, peak } = ending else {
        panic!(
            "not stopped by its cap: {ending:?}; printed {:?}",
            probe.lines()
        );
    };
    assert_eq!((limit, value), expected);
    println!(
        "MEASURE fork mechanism={} cap={} peak={peak}",
        limits.mechanism().as_str(),
        expected.1
    );
    probe.wait_empty().await;
}

/// Goal 4: a four-thread spin under a 25% cap uses at most 35% of the machine, measured as the
/// unit's own CPU time. Where the mechanism cannot cap CPU, the cap is reported as not enforced.
#[tokio::test]
async fn a_cpu_spin_is_held_to_its_cap() {
    let _serial = SERIAL.lock().await;
    let Some(limits) = limits("cpu") else { return };
    let caps = Caps {
        cpu_percent: Some(25),
        ..Caps::default()
    };
    let started = Instant::now();
    let mut probe = Probe::start(&limits, spec(caps), &["spin", "4", "3"]);
    if probe.unit.not_enforced().contains(&Limit::Cpu) {
        assert_ne!(
            limits.mechanism(),
            imperium_limits::Mechanism::Job,
            "jobs enforce CPU caps"
        );
        println!(
            "MEASURE cpu mechanism={} not_enforced",
            limits.mechanism().as_str()
        );
        probe.unit.stop();
        return;
    }
    let ending = probe.ended(Duration::from_secs(30)).await;
    assert!(
        matches!(ending, Ending::Exited(status) if status.success()),
        "{ending:?}"
    );
    let wall = started.elapsed().as_secs_f64();
    let cpus = std::thread::available_parallelism().unwrap().get() as f64;
    let cpu = probe
        .unit
        .usage()
        .cpu_time
        .expect("CPU accounting")
        .as_secs_f64();
    let share = cpu / (cpus * wall);
    println!(
        "MEASURE cpu mechanism={} cpus={cpus} wall_s={wall:.2} cpu_s={cpu:.2} share={share:.3}",
        limits.mechanism().as_str()
    );
    assert!(share <= 0.35, "the spin used {share:.3} of the machine");
}

/// Linux: `memory.oom.group` stops the whole tree, not only the allocator.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_oom_group_kills_the_whole_tree() {
    let _serial = SERIAL.lock().await;
    let Some(limits) = limits("oom-group") else {
        return;
    };
    let caps = Caps {
        memory_bytes: Some(256 * MIB),
        ..Caps::default()
    };
    let script = format!("{PROBE} sleep 30 & exec {PROBE} alloc 64 1024");
    let mut command = limits.command(spec(caps), "sh").unwrap();
    command.args(["-c", &script]);
    let mut unit = command.spawn().unwrap();
    let ending = tokio::time::timeout(Duration::from_secs(60), unit.ended())
        .await
        .expect("ended");
    assert!(
        matches!(
            ending,
            Ending::Limited {
                limit: Limit::Memory,
                ..
            }
        ),
        "{ending:?}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while unit.usage().processes > 0 {
        assert!(Instant::now() < deadline, "the sleeping sibling survived");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Linux: `cgroup.kill` stops a tree on request.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_stopped_unit_takes_its_tree_with_it() {
    let _serial = SERIAL.lock().await;
    let Some(limits) = limits("kill") else { return };
    let script = format!("{PROBE} sleep 30 & {PROBE} sleep 30 & wait");
    let mut command = limits.command(spec(Caps::default()), "sh").unwrap();
    command.args(["-c", &script]);
    let mut unit = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while unit.usage().processes < 3 {
        assert!(Instant::now() < deadline, "the tree did not start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    unit.stop();
    let ending = tokio::time::timeout(Duration::from_secs(10), unit.ended())
        .await
        .expect("ended");
    assert!(matches!(ending, Ending::Stopped(_)), "{ending:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while unit.usage().processes > 0 {
        assert!(Instant::now() < deadline, "a sleeper survived the stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Linux without a subtree: `RLIMIT_DATA` refuses a single allocator past its cap.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_fallback_refuses_allocations_past_rlimit_data() {
    let _serial = SERIAL.lock().await;
    let limits = Limits::new(Config {
        launcher: Some(launcher()),
        ..Config::default()
    })
    .unwrap();
    assert_eq!(limits.mechanism(), imperium_limits::Mechanism::RlimitPoll);
    let caps = Caps {
        memory_bytes: Some(256 * MIB),
        cpu_percent: Some(25),
        ..Caps::default()
    };
    let mut probe = Probe::start(
        &limits,
        spec(caps),
        &["alloc", "64", "1024", "--give-up-ms", "500"],
    );
    assert!(probe.unit.not_enforced().contains(&Limit::Cpu));
    let ending = probe.ended(Duration::from_secs(60)).await;
    // The allocator is refused (exit 4) or stopped by the poll; either way it stays under the cap.
    match ending {
        Ending::Exited(status) => assert_eq!(status.code(), Some(4), "{:?}", probe.lines()),
        Ending::Limited { limit, .. } => assert_eq!(limit, Limit::Memory),
        Ending::Stopped(_) => panic!("not stopped by us"),
    }
    assert!(probe.allocated() <= 256, "{:?}", probe.lines());
    assert!(
        probe
            .lines()
            .iter()
            .any(|line| line.starts_with("alloc-failed"))
            || matches!(ending, Ending::Limited { .. })
    );
}

/// Linux without a subtree: the 250 ms tree poll stops a tree whose processes are each under
/// `RLIMIT_DATA` but whose sum is past the cap.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_fallback_poll_stops_a_tree_past_its_cap() {
    let _serial = SERIAL.lock().await;
    let limits = Limits::new(Config {
        launcher: Some(launcher()),
        ..Config::default()
    })
    .unwrap();
    let caps = Caps {
        memory_bytes: Some(256 * MIB),
        ..Caps::default()
    };
    let script = format!("{PROBE} alloc 32 192 --hold 30 & {PROBE} alloc 32 192 --hold 30 & wait");
    let mut command = limits.command(spec(caps), "sh").unwrap();
    command.args(["-c", &script]);
    let mut unit = command.spawn().unwrap();
    let ending = tokio::time::timeout(Duration::from_secs(30), unit.ended())
        .await
        .expect("the poll stopped the tree");
    let Ending::Limited { limit, value, peak } = ending else {
        panic!("{ending:?}");
    };
    assert_eq!((limit, value), (Limit::Memory, 256 * MIB));
    println!(
        "MEASURE fallback-poll cap_mib=256 sampled_peak_mib={}",
        peak / MIB
    );
    let group = unit.id().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = std::process::Command::new("pgrep")
            .args(["-g", &group])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        if left.code() == Some(1) {
            break;
        }
        assert!(Instant::now() < deadline, "the tree outlived the stop");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    fn kill(pid: u32) {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    async fn go(probe: &mut Probe) -> String {
        let mut stdin = probe.unit.stdin.take().expect("piped stdin");
        stdin.write_all(b"go\n").await.unwrap();
        stdin.flush().await.unwrap();
        probe.wait_for("", Duration::from_secs(10)).await
    }

    /// F2: a capped unit's `CREATE_BREAKAWAY_FROM_JOB` child fails to start or stays in the unit.
    /// The control: the same probe in a plain job with `BREAKAWAY_OK` does break away, which
    /// shows the probe really tries.
    #[tokio::test]
    async fn breakaway_cannot_leave_a_unit() {
        let _serial = SERIAL.lock().await;
        let limits = limits("breakaway").unwrap();
        let mut probe = Probe::start(&limits, spec(Caps::default()), &["breakaway"]);
        let line = go(&mut probe).await;
        if let Some(pid) = line.strip_prefix("started ") {
            let pid: u32 = pid.parse().unwrap();
            let inside = probe.unit.job().contains(pid);
            kill(pid);
            assert!(inside.unwrap(), "the breakaway child left the unit");
            println!("MEASURE breakaway unit=stayed_in_unit");
        } else {
            assert!(line.starts_with("refused "), "{line}");
            println!("MEASURE breakaway unit=refused ({line})");
        }
        probe.unit.stop();

        // The control: a plain job that allows breakaway.
        let job = imperium_limits::windows::Job::new().unwrap();
        job.limit(&Caps::default(), true).unwrap();
        let mut command = tokio::process::Command::new(PROBE);
        command
            .arg("breakaway")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        job.assign_child(&child).unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"go\n").await.unwrap();
        stdin.flush().await.unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        job.terminate();
        let pid: u32 = line
            .strip_prefix("started ")
            .unwrap_or_else(|| panic!("the control could not break away: {line}"))
            .parse()
            .unwrap();
        let inside = job.contains(pid);
        kill(pid);
        assert!(!inside.unwrap(), "the control's child did not break away");
        println!("MEASURE breakaway control=broke_away");
    }

    /// L2: the nesting order holds (a unit's process is in its unit, session and total jobs),
    /// and a session limit stops the session's largest unit, not the allocator that hit it.
    #[tokio::test]
    async fn a_session_limit_stops_the_largest_unit() {
        let _serial = SERIAL.lock().await;
        let limits = limits("session").unwrap();
        limits
            .set_session(
                "s",
                Caps {
                    memory_bytes: Some(384 * MIB),
                    ..Caps::default()
                },
            )
            .unwrap();
        let in_session = || UnitSpec {
            session: Some("s".to_owned()),
            ..spec(Caps::default())
        };
        let mut large = Probe::start(
            &limits,
            in_session(),
            &["alloc", "32", "256", "--hold", "60"],
        );
        large.wait_for("holding", Duration::from_secs(30)).await;
        let mut small = Probe::start(
            &limits,
            in_session(),
            &[
                "alloc",
                "16",
                "192",
                "--hold",
                "60",
                "--give-up-ms",
                "20000",
            ],
        );
        let pid = small.unit.id();
        assert!(limits.total_contains(pid).unwrap());
        assert!(limits.session_contains("s", pid).unwrap());
        assert!(small.unit.job().contains(pid).unwrap());

        let ending = large.ended(Duration::from_secs(30)).await;
        assert!(
            matches!(ending, Ending::Limited { limit: Limit::SessionMemory, value, .. } if value == 384 * MIB),
            "{ending:?}"
        );
        small.wait_for("holding", Duration::from_secs(30)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), small.unit.ended())
                .await
                .is_err(),
            "the allocator was stopped instead of the largest unit"
        );
        small.unit.stop();
        let _ = small.ended(Duration::from_secs(10)).await;
    }

    /// A3 and the cost of the Toolhelp thread snapshot on a machine with thousands of threads.
    #[tokio::test]
    async fn the_thread_snapshot_is_cheap_on_a_busy_machine() {
        let _serial = SERIAL.lock().await;
        let mut busy = tokio::process::Command::new(PROBE)
            .args(["threads", "3000", "60"])
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut ready = BufReader::new(busy.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(30), ready.next_line())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(line.as_deref(), Some("ready"));
        let limits = limits("snapshot").unwrap();
        let mut times = Vec::new();
        for _ in 0..20 {
            let mut command = limits.command(spec(Caps::default()), PROBE).unwrap();
            command.args(["sleep", "0"]);
            let mut unit = command.spawn().expect("A3: one suspended thread, resumed");
            times.push(unit.snapshot_time());
            let ending = tokio::time::timeout(Duration::from_secs(10), unit.ended())
                .await
                .unwrap();
            assert!(
                matches!(ending, Ending::Exited(status) if status.success()),
                "{ending:?}"
            );
        }
        times.sort();
        println!(
            "MEASURE snapshot busy_threads=3000 median_ms={:.2} max_ms={:.2}",
            times[times.len() / 2].as_secs_f64() * 1000.0,
            times[times.len() - 1].as_secs_f64() * 1000.0,
        );
        let _ = busy.kill().await;
    }

    /// L15: whether pagefile-backed section memory counts against a job's memory limit. Either
    /// answer is recorded; the probe's ceiling bounds the other.
    #[tokio::test]
    async fn section_memory_against_the_job_limit() {
        let _serial = SERIAL.lock().await;
        let limits = limits("section").unwrap();
        let caps = Caps {
            memory_bytes: Some(256 * MIB),
            ..Caps::default()
        };
        let mut probe = Probe::start(&limits, spec(caps), &["alloc", "64", "1024", "--section"]);
        let ending = probe.ended(Duration::from_secs(60)).await;
        match ending {
            Ending::Limited {
                limit: Limit::Memory,
                peak,
                ..
            } => {
                println!(
                    "MEASURE section counted peak_mib={} allocated_mib={}",
                    peak / MIB,
                    probe.allocated()
                );
            }
            Ending::Exited(status) if status.code() == Some(3) => {
                println!(
                    "MEASURE section escaped allocated_mib={}",
                    probe.allocated()
                );
            }
            other => panic!("unexpected: {other:?}; printed {:?}", probe.lines()),
        }
    }
}
