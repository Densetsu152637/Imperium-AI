//! Measurements for assumption A4 (cold start, binary size) and the type-checker follow-up of
//! F12 (resident memory). Logged with a `MEASURE` prefix; CI runs this in release mode with
//! `--nocapture` and the numbers are recorded in the plan. The assertions are loose floors that
//! catch a broken worker, not the plan's targets.
#![cfg(unix)]

mod common;

use std::{process::Command, time::Duration};

use common::{launcher, quick_limits, worker_path};
use imperium_script::ScriptEvent;

fn resident_kb(pid: u32) -> Option<u64> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[tokio::test]
async fn cold_start_size_and_resident_memory() {
    let size = std::fs::metadata(worker_path()).unwrap().len();
    eprintln!(
        "MEASURE binary_bytes={size} binary_mib={:.1} profile={}",
        size as f64 / 1_048_576.0,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let launcher = launcher();
    let mut starts: Vec<Duration> = Vec::new();
    let mut resident = Vec::new();
    for _ in 0..15 {
        let mut run = launcher.start("lookup()", &quick_limits()).await.unwrap();
        starts.push(run.startup());
        assert!(matches!(run.next().await, ScriptEvent::Call(_)));
        if let Some(kb) = run.pid().and_then(resident_kb) {
            resident.push(kb);
        }
        run.finish().await;
    }
    starts.sort();
    let median = starts[starts.len() / 2];
    eprintln!(
        "MEASURE cold_start_ms min={:.1} median={:.1} max={:.1}",
        starts[0].as_secs_f64() * 1000.0,
        median.as_secs_f64() * 1000.0,
        starts[starts.len() - 1].as_secs_f64() * 1000.0
    );
    resident.sort_unstable();
    if let Some(median) = resident.get(resident.len() / 2) {
        eprintln!(
            "MEASURE resident_mib median={:.1} max={:.1}",
            *median as f64 / 1024.0,
            resident[resident.len() - 1] as f64 / 1024.0
        );
    }
    assert!(
        median < Duration::from_secs(2),
        "cold start took {median:?}"
    );
}
