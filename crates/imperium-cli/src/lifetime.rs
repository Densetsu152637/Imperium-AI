//! `imperium daemon status`: what keeps the running imperium-core up.

use imperium_protocol::methods::{
    LeaseKind, LifetimeMode, LifetimeStatus, SystemLifetime, SystemLifetimeParams, WorkKind,
};

use crate::{block_on, connect_existing};

pub fn status() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, url) = connect_existing().await?;
        let status = client
            .call::<SystemLifetime>(SystemLifetimeParams { persistent: None })
            .await?;
        let daemon = client.daemon();
        println!(
            "{} {} (protocol {}) at {url}",
            daemon.daemon.name, daemon.daemon.version, daemon.protocol
        );
        print!("{}", describe(&status));
        Ok(())
    })
}

fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

fn kind(kind: LeaseKind) -> &'static str {
    match kind {
        LeaseKind::Cli => "cli",
        LeaseKind::Desktop => "desktop",
        LeaseKind::Tool => "tool",
    }
}

fn work(kind: WorkKind) -> &'static str {
    match kind {
        WorkKind::Turn => "running turns",
        WorkKind::Schedule => "schedules",
        WorkKind::Initiative => "agents with initiative",
        WorkKind::Github => "GitHub responders",
    }
}

/// The report, one line per fact.
fn describe(status: &LifetimeStatus) -> String {
    let mut out = String::new();
    match status.mode {
        LifetimeMode::Persistent => out.push_str("lifetime: runs until it is stopped\n"),
        LifetimeMode::Shared => out.push_str(&format!(
            "lifetime: shared; exits {} after the last client and the last work are gone\n",
            seconds(status.grace_ms)
        )),
    }
    if status.exiting {
        out.push_str("it has decided to exit and takes no more clients\n");
    }
    if status.leases.is_empty() {
        out.push_str("clients holding it open: none\n");
    } else {
        out.push_str(&format!(
            "clients holding it open: {}\n",
            status.leases.len()
        ));
        for lease in &status.leases {
            let pid = lease
                .pid
                .map_or_else(|| "-".to_string(), |pid| pid.to_string());
            out.push_str(&format!(
                "  {}  {}  pid {pid}  {}  (held {})\n",
                lease.id,
                kind(lease.kind),
                lease.label,
                seconds(lease.held_ms)
            ));
        }
    }
    if status.work.is_empty() {
        out.push_str("work keeping it up: none\n");
    } else {
        out.push_str("work keeping it up:\n");
        for item in &status.work {
            out.push_str(&format!(
                "  {} {}: {}\n",
                item.count,
                work(item.kind),
                item.detail
            ));
        }
    }
    if let (Some(idle), Some(left)) = (status.idle_ms, status.exits_in_ms) {
        out.push_str(&format!(
            "idle for {}; exits in {}\n",
            seconds(idle),
            seconds(left)
        ));
    } else if let Some(idle) = status.idle_ms {
        out.push_str(&format!(
            "idle for {}; it does not exit when idle\n",
            seconds(idle)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use imperium_protocol::methods::{LeaseInfo, WorkInfo};

    use super::*;

    fn status() -> LifetimeStatus {
        LifetimeStatus {
            mode: LifetimeMode::Shared,
            grace_ms: 10_000,
            leases: vec![LeaseInfo {
                id: "lease-1".into(),
                kind: LeaseKind::Cli,
                pid: Some(4321),
                label: "imperium chat".into(),
                held_ms: 12_340,
            }],
            work: vec![WorkInfo {
                kind: WorkKind::Schedule,
                count: 2,
                detail: "nightly, triage".into(),
            }],
            idle_ms: None,
            exits_in_ms: None,
            exiting: false,
        }
    }

    #[test]
    fn the_report_lists_who_holds_the_daemon_and_what_work_does() {
        let report = describe(&status());
        assert!(report.contains("shared; exits 10.0 s after"), "{report}");
        assert!(
            report.contains("  lease-1  cli  pid 4321  imperium chat  (held 12.3 s)"),
            "{report}"
        );
        assert!(
            report.contains("  2 schedules: nightly, triage"),
            "{report}"
        );
        assert!(!report.contains("idle for"), "{report}");
    }

    #[test]
    fn an_idle_daemon_says_when_it_exits() {
        let mut idle = status();
        idle.leases.clear();
        idle.work.clear();
        idle.idle_ms = Some(3_000);
        idle.exits_in_ms = Some(7_000);
        let report = describe(&idle);
        assert!(report.contains("clients holding it open: none"), "{report}");
        assert!(report.contains("work keeping it up: none"), "{report}");
        assert!(
            report.contains("idle for 3.0 s; exits in 7.0 s"),
            "{report}"
        );

        idle.mode = LifetimeMode::Persistent;
        idle.exits_in_ms = None;
        let report = describe(&idle);
        assert!(report.contains("runs until it is stopped"), "{report}");
        assert!(report.contains("it does not exit when idle"), "{report}");
    }
}
