//! Bounded probes for the cap tests and the CI measurements (plan stage 1, goal 9). Each one
//! enforces a hard ceiling of its own, so a cap that fails to hold fails the test (exit 3)
//! instead of exhausting the machine.
//!
//! - `sleep SECS`
//! - `alloc STEP_MIB CEILING_MIB [--section] [--hold SECS] [--pause-ms MS] [--give-up-ms MS]`:
//!   allocates and touches memory in steps, printing `allocated <MiB>` after each. At the
//!   ceiling it prints `CEILING` and exits 3, or with `--hold` prints `holding <MiB>` and sleeps.
//!   A refused allocation prints `alloc-failed <MiB>` and is retried until `--give-up-ms`
//!   (exit 4). `--section` commits pagefile-backed section memory (Windows).
//! - `fork CEILING`: starts copies of itself (`sleep 30`) in a loop, printing `started <n>`;
//!   refused starts print `spawn-failed <n>` and are retried for 10 s (exit 4); at the ceiling
//!   it prints `CEILING` and exits 3.
//! - `spin THREADS SECS`: busy threads for a wall-clock time.
//! - `threads COUNT SECS`: parked threads (a busy machine for the thread-snapshot measurement);
//!   prints `ready` once they run.
//! - `breakaway` (Windows): after a line on stdin, starts `sleep 30` with
//!   `CREATE_BREAKAWAY_FROM_JOB` and prints `started <pid>` or `refused <error>`.
//! - `launch ...` (Linux): the unit launcher (`imperium_limits::launcher`).
//! - `measure [--secs N] -- PROGRAM ARGS...`: runs the program in an uncapped unit and prints
//!   `MEASURE` with its peak processes, tasks and memory.

use std::{
    ffi::OsString,
    io::Write as _,
    process::ExitCode,
    time::{Duration, Instant},
};

/// The exit code for a probe that reached its own hard ceiling: the cap under test did not hold.
const CEILING: u8 = 3;
/// The exit code for a probe whose allocations or starts stayed refused.
const GAVE_UP: u8 = 4;

const MIB: usize = 1 << 20;

fn say(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn number(value: Option<&OsString>, what: &str) -> u64 {
    value
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| panic!("limit_probe: {what} must be a number"))
}

fn option(args: &[OsString], name: &str) -> Option<u64> {
    let at = args.iter().position(|arg| arg == name)?;
    Some(number(args.get(at + 1), name))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let command = args.first().and_then(|arg| arg.to_str()).unwrap_or("");
    let rest = args.get(1..).unwrap_or(&[]);
    match command {
        "sleep" => {
            std::thread::sleep(Duration::from_secs(number(rest.first(), "SECS")));
            ExitCode::SUCCESS
        }
        "alloc" => alloc(rest),
        "fork" => fork(number(rest.first(), "CEILING")),
        "spin" => {
            spin(number(rest.first(), "THREADS"), number(rest.get(1), "SECS"));
            ExitCode::SUCCESS
        }
        "threads" => {
            threads(number(rest.first(), "COUNT"), number(rest.get(1), "SECS"));
            ExitCode::SUCCESS
        }
        "breakaway" => breakaway(),
        #[cfg(target_os = "linux")]
        "launch" => {
            let code = imperium_limits::launcher::run(rest.to_vec());
            ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
        "measure" => measure(rest),
        _ => {
            eprintln!("limit_probe: unknown command {command:?}");
            ExitCode::from(2)
        }
    }
}

fn alloc(args: &[OsString]) -> ExitCode {
    let step = usize::try_from(number(args.first(), "STEP_MIB")).unwrap_or(1) * MIB;
    let ceiling = usize::try_from(number(args.get(1), "CEILING_MIB")).unwrap_or(1) * MIB;
    let section = args.iter().any(|arg| arg == "--section");
    let hold = option(args, "--hold");
    let pause = Duration::from_millis(option(args, "--pause-ms").unwrap_or(20));
    let give_up = Duration::from_millis(option(args, "--give-up-ms").unwrap_or(2_000));
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut total = 0_usize;
    let mut refused_since: Option<Instant> = None;
    while total < ceiling {
        let step = step.min(ceiling - total);
        let done = if section {
            commit_section(step)
        } else {
            let mut chunk = Vec::new();
            chunk.try_reserve_exact(step).ok().map(|()| {
                // Nonzero bytes: every page is touched and none is a shared zero page.
                chunk.resize(step, 1);
                kept.push(chunk);
            })
        };
        if done.is_some() {
            total += step;
            refused_since = None;
            say(&format!("allocated {}", total / MIB));
        } else {
            say(&format!("alloc-failed {}", total / MIB));
            let since = *refused_since.get_or_insert_with(Instant::now);
            if since.elapsed() > give_up {
                return ExitCode::from(GAVE_UP);
            }
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        std::thread::sleep(pause);
    }
    if let Some(secs) = hold {
        say(&format!("holding {}", total / MIB));
        std::thread::sleep(Duration::from_secs(secs));
        drop(kept);
        return ExitCode::SUCCESS;
    }
    say("CEILING");
    ExitCode::from(CEILING)
}

#[cfg(windows)]
fn commit_section(bytes: usize) -> Option<()> {
    imperium_limits::windows::commit_section(bytes).ok()
}

#[cfg(not(windows))]
fn commit_section(_bytes: usize) -> Option<()> {
    eprintln!("limit_probe: --section is Windows only");
    std::process::exit(2);
}

fn fork(ceiling: u64) -> ExitCode {
    let me = std::env::current_exe().expect("limit_probe: own path");
    let mut children = Vec::new();
    let mut refused_since: Option<Instant> = None;
    loop {
        if children.len() as u64 >= ceiling {
            say("CEILING");
            return ExitCode::from(CEILING);
        }
        match std::process::Command::new(&me)
            .args(["sleep", "30"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                children.push(child);
                refused_since = None;
                say(&format!("started {}", children.len()));
            }
            Err(err) => {
                say(&format!("spawn-failed {} {err}", children.len()));
                let since = *refused_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(10) {
                    return ExitCode::from(GAVE_UP);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn spin(threads: u64, secs: u64) {
    let end = Instant::now() + Duration::from_secs(secs);
    let workers: Vec<_> = (0..threads)
        .map(|_| {
            std::thread::spawn(move || {
                let mut x = 0_u64;
                while Instant::now() < end {
                    x = std::hint::black_box(
                        x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                    );
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
}

fn threads(count: u64, secs: u64) {
    let workers: Vec<_> = (0..count)
        .map(|_| {
            std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn(move || std::thread::sleep(Duration::from_secs(secs)))
                .expect("limit_probe: thread")
        })
        .collect();
    say("ready");
    for worker in workers {
        let _ = worker.join();
    }
}

#[cfg(windows)]
fn breakaway() -> ExitCode {
    use std::os::windows::process::CommandExt as _;
    /// `CREATE_BREAKAWAY_FROM_JOB`.
    const BREAKAWAY: u32 = 0x0100_0000;
    let mut go = String::new();
    let _ = std::io::stdin().read_line(&mut go);
    let me = std::env::current_exe().expect("limit_probe: own path");
    let child = std::process::Command::new(me)
        .args(["sleep", "30"])
        .creation_flags(BREAKAWAY)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match child {
        Ok(child) => say(&format!("started {}", child.id())),
        Err(err) => say(&format!("refused {err}")),
    }
    std::thread::sleep(Duration::from_secs(30));
    ExitCode::SUCCESS
}

#[cfg(not(windows))]
fn breakaway() -> ExitCode {
    eprintln!("limit_probe: breakaway is Windows only");
    ExitCode::from(2)
}

/// Runs a program in an uncapped unit and reports its peaks (the L6 measurements). On Linux the
/// unit is a cgroup under `$IMPERIUM_TEST_CGROUP/measure`.
fn measure(args: &[OsString]) -> ExitCode {
    let secs = option(args, "--secs");
    let Some(split) = args.iter().position(|arg| arg == "--") else {
        eprintln!("limit_probe: measure needs -- PROGRAM");
        return ExitCode::from(2);
    };
    let Some(program) = args.get(split + 1) else {
        eprintln!("limit_probe: measure needs -- PROGRAM");
        return ExitCode::from(2);
    };
    let program_args = args.get(split + 2..).unwrap_or(&[]).to_vec();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("limit_probe: runtime");
    runtime.block_on(async move {
        let limits = imperium_limits::Limits::new(config()).expect("limit_probe: limits");
        let spec = imperium_limits::UnitSpec {
            class: imperium_limits::Class::Internal,
            caps: imperium_limits::Caps::default(),
            session: None,
            label: program.to_string_lossy().into_owned(),
        };
        let mut command = limits.command(spec, program).expect("limit_probe: unit");
        command.args(&program_args);
        let mut unit = command.spawn().expect("limit_probe: spawn");
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let ending = loop {
            tokio::select! {
                ending = unit.ended() => break ending,
                _ = tick.tick() => {
                    let _ = unit.usage();
                    if secs.is_some_and(|secs| started.elapsed() >= Duration::from_secs(secs)) {
                        unit.stop();
                    }
                }
            }
        };
        let usage = unit.usage();
        let name = std::path::Path::new(program)
            .file_name()
            .map_or_else(|| program.to_string_lossy(), |name| name.to_string_lossy());
        say(&format!(
            "MEASURE {name} mechanism={} peak_processes={} peak_tasks={} peak_memory_mib={} seconds={} ending={ending:?}",
            limits.mechanism().as_str(),
            usage.peak_processes,
            usage.peak_tasks.map_or_else(|| "n/a".to_owned(), |tasks| tasks.to_string()),
            usage.peak_memory_bytes >> 20,
            started.elapsed().as_secs(),
        ));
        match ending {
            imperium_limits::Ending::Exited(status) if status.success() => ExitCode::SUCCESS,
            imperium_limits::Ending::Stopped(_) if secs.is_some() => ExitCode::SUCCESS,
            _ => ExitCode::FAILURE,
        }
    })
}

#[cfg(target_os = "linux")]
fn config() -> imperium_limits::Config {
    let launcher = imperium_limits::Launcher {
        program: std::env::current_exe().expect("limit_probe: own path"),
        args: vec!["launch".into()],
    };
    let cgroup_root = std::env::var_os("IMPERIUM_TEST_CGROUP").map(|parent| {
        imperium_limits::cgroup::prepare_child(std::path::Path::new(&parent), "measure")
            .expect("limit_probe: cgroup root")
    });
    imperium_limits::Config {
        total: imperium_limits::Caps::default(),
        cgroup_root,
        launcher: Some(launcher),
    }
}

#[cfg(not(target_os = "linux"))]
fn config() -> imperium_limits::Config {
    imperium_limits::Config::default()
}
