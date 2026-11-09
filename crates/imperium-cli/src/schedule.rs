//! `imperium schedule list` and `imperium schedule run`.

use imperium_protocol::methods::{
    ScheduleList, ScheduleListParams, ScheduleRun, ScheduleRunParams,
};

use crate::{block_on, connect};

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client.call::<ScheduleList>(ScheduleListParams {}).await?;
        if listing.schedules.is_empty() {
            println!("No schedules. Add [[schedules]] to your config and restart the daemon.");
        }
        for row in listing.schedules {
            let state = match (&row.problem, row.running, row.enabled) {
                (Some(problem), ..) => format!("error: {problem}"),
                (None, true, _) => match &row.deferred {
                    Some(reason) => format!(
                        "deferred: {reason} (imperium schedule run {} runs it now)",
                        row.id
                    ),
                    None => "running".into(),
                },
                (None, false, false) => "disabled".into(),
                (None, false, true) => row
                    .next_run
                    .map_or_else(|| "no further runs".into(), |next| format!("next {next}")),
            };
            let last = match (&row.last_run, row.last_ok) {
                (Some(at), Some(true)) => format!("last ok {at}"),
                (Some(at), _) => format!("last failed {at}"),
                (None, _) => "never run".into(),
            };
            println!(
                "{:<24} {:<26} {:<8} {}  ({last})",
                row.id, row.trigger, row.action, state
            );
        }
        Ok(())
    })
}

pub fn run(id: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let result = client
            .call::<ScheduleRun>(ScheduleRunParams { id: id.clone() })
            .await?;
        if result.started {
            println!("started {id}; its session and result appear in the desktop app");
        } else {
            println!("{id} is already running");
        }
        Ok(())
    })
}
