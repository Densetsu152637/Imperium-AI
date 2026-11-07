//! Schedules: when a task should run (cron, interval, or one-shot triggers), what to do about
//! runs missed while the computer was off, and the record of past runs that survives restarts.

pub mod cron;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

pub use cron::Cron;

/// When a schedule fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    Cron(Cron),
    /// Every interval after the previous run (or after the daemon starts).
    Every(Duration),
    /// Once, at a local time.
    At(DateTime<Local>),
}

impl Trigger {
    /// Parses exactly one of `cron`, `every` (`90s`, `30m`, `2h`, `1d`), or `at`
    /// (`2026-11-08 15:00` or `2026-11-08T15:00`, local time).
    pub fn parse(
        cron: Option<&str>,
        every: Option<&str>,
        at: Option<&str>,
    ) -> Result<Self, String> {
        match (cron, every, at) {
            (Some(expression), None, None) => Cron::parse(expression).map(Trigger::Cron),
            (None, Some(interval), None) => parse_interval(interval).map(Trigger::Every),
            (None, None, Some(time)) => parse_time(time).map(Trigger::At),
            _ => Err("set exactly one of trigger.cron, trigger.every, or trigger.at".into()),
        }
    }

    /// The first time strictly after `after` that the schedule fires, if any.
    pub fn next_after(&self, after: DateTime<Local>) -> Option<DateTime<Local>> {
        match self {
            Trigger::Cron(cron) => cron.next_after(after),
            Trigger::Every(interval) => Some(after + *interval),
            Trigger::At(time) => (*time > after).then_some(*time),
        }
    }
}

fn parse_interval(text: &str) -> Result<Duration, String> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| format!("{text:?} needs a unit: s, m, h, or d"))?;
    let (number, unit) = text.split_at(split);
    let number: i64 = number
        .parse()
        .map_err(|_| format!("{text:?} does not start with a number"))?;
    let interval = match unit.trim() {
        "s" => Duration::seconds(number),
        "m" | "min" => Duration::minutes(number),
        "h" => Duration::hours(number),
        "d" => Duration::days(number),
        other => return Err(format!("unknown unit {other:?}; use s, m, h, or d")),
    };
    if interval <= Duration::zero() {
        return Err("the interval must be positive".into());
    }
    Ok(interval)
}

fn parse_time(text: &str) -> Result<DateTime<Local>, String> {
    let text = text.trim().replace('T', " ");
    let naive = NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(&text, "%Y-%m-%d %H:%M:%S"))
        .map_err(|_| format!("{text:?} is not a time like 2026-11-08 15:00"))?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .ok_or_else(|| format!("{text} does not exist in the local time zone"))
}

/// What to do about runs that fell while the daemon was not running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Missed {
    Skip,
    #[default]
    RunOnceOnWake,
}

/// When the schedule should next run, given when it last ran (or when tracking began) and
/// the current time. A run due in the past is returned as `now` under `run_once_on_wake`
/// (once, however many were missed) and skipped under `skip`.
pub fn next_run(
    trigger: &Trigger,
    missed: Missed,
    last: Option<DateTime<Local>>,
    now: DateTime<Local>,
) -> Option<DateTime<Local>> {
    let Some(last) = last else {
        return trigger.next_after(now);
    };
    match trigger.next_after(last) {
        Some(due) if due <= now => match missed {
            Missed::RunOnceOnWake => Some(now),
            Missed::Skip => trigger.next_after(now),
        },
        other => other,
    }
}

/// The outcome of a run, as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub at: DateTime<Local>,
    pub ok: bool,
    pub summary: String,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Past runs per schedule id, kept in a JSON file so missed runs can be found after a restart.
#[derive(Debug)]
pub struct History {
    path: PathBuf,
    records: BTreeMap<String, RunRecord>,
}

impl History {
    /// Loads `path`; a missing or unreadable file starts an empty history.
    pub fn load(path: &Path) -> Self {
        let records = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        History {
            path: path.to_path_buf(),
            records,
        }
    }

    pub fn last(&self, id: &str) -> Option<&RunRecord> {
        self.records.get(id)
    }

    /// Records a run and writes the file.
    pub fn record(&mut self, id: &str, record: RunRecord) -> std::io::Result<()> {
        self.records.insert(id.to_string(), record);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&self.records).map_err(std::io::Error::other)?;
        std::fs::write(&self.path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(text: &str) -> DateTime<Local> {
        parse_time(text).unwrap()
    }

    #[test]
    fn triggers_parse_and_step() {
        let every = Trigger::parse(None, Some("30m"), None).unwrap();
        assert_eq!(
            every.next_after(time("2026-11-07 10:00")),
            Some(time("2026-11-07 10:30"))
        );
        let once = Trigger::parse(None, None, Some("2026-11-08T15:00")).unwrap();
        assert_eq!(
            once.next_after(time("2026-11-08 14:00")),
            Some(time("2026-11-08 15:00"))
        );
        assert_eq!(once.next_after(time("2026-11-08 16:00")), None);
        assert!(Trigger::parse(Some("* * * * *"), Some("5m"), None).is_err());
        assert!(Trigger::parse(None, Some("5 parsecs"), None).is_err());
        assert!(Trigger::parse(None, Some("0s"), None).is_err());
    }

    #[test]
    fn missed_runs_run_once_on_wake_or_are_skipped() {
        let daily = Trigger::parse(Some("0 8 * * *"), None, None).unwrap();
        let last = Some(time("2026-11-05 08:00"));
        let now = time("2026-11-07 12:00");
        // Two runs were missed; one runs now.
        assert_eq!(
            next_run(&daily, Missed::RunOnceOnWake, last, now),
            Some(now)
        );
        assert_eq!(
            next_run(&daily, Missed::Skip, last, now),
            Some(time("2026-11-08 08:00"))
        );
        // Never ran: wait for the next occurrence.
        assert_eq!(
            next_run(&daily, Missed::RunOnceOnWake, None, now),
            Some(time("2026-11-08 08:00"))
        );
        // Nothing missed.
        let recent = Some(time("2026-11-07 08:00"));
        assert_eq!(
            next_run(&daily, Missed::RunOnceOnWake, recent, now),
            Some(time("2026-11-08 08:00"))
        );
    }

    #[test]
    fn history_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules/history.json");
        let mut history = History::load(&path);
        assert!(history.last("triage").is_none());
        let record = RunRecord {
            at: time("2026-11-07 08:00"),
            ok: true,
            summary: "done".into(),
            session_id: Some("s1".into()),
        };
        history.record("triage", record.clone()).unwrap();
        assert_eq!(History::load(&path).last("triage"), Some(&record));
    }
}
