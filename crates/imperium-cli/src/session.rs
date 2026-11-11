//! `imperium session list|archive|unarchive`: the daemon's sessions, and archiving them.

use imperium_protocol::methods::{
    ExportFormat, SessionControl, SessionControlParams, SessionExport, SessionExportParams,
    SessionList, SessionListParams, SessionSummary,
};

use crate::{block_on, connect};

/// Lists running sessions, or with `archived` only the archived ones.
pub fn list(archived: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client.call::<SessionList>(SessionListParams {}).await?;
        let shown: Vec<&SessionSummary> = listing
            .sessions
            .iter()
            .filter(|session| session.archived == archived)
            .collect();
        if shown.is_empty() {
            println!(
                "{}",
                if archived {
                    "No archived sessions."
                } else {
                    "No sessions. Start one with: imperium chat"
                }
            );
        }
        for session in shown {
            println!("{}", describe(session));
        }
        Ok(())
    })
}

fn describe(session: &SessionSummary) -> String {
    let title = if session.title.is_empty() {
        "(no messages yet)"
    } else {
        session.title.as_str()
    };
    let status = serde_json::to_value(session.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default();
    format!(
        "{}  {:<15} {}  {}  {}",
        session.session_id,
        if session.archived {
            "archived"
        } else {
            &status
        },
        session.model.as_deref().unwrap_or("no model"),
        session.project_dir,
        title,
    )
}

/// Archives a session (`archive`) or resumes an archived one (`unarchive`).
pub fn set_archived(session_id: String, archived: bool) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        client
            .call::<SessionControl>(SessionControlParams {
                session_id: session_id.clone(),
                action: if archived { "archive" } else { "unarchive" }.into(),
                resume_policy: None,
                repo: None,
                pull_request: None,
                model: None,
                effort: None,
                pinned: None,
                title: None,
            })
            .await?;
        println!(
            "{} session {session_id}",
            if archived { "archived" } else { "unarchived" }
        );
        Ok(())
    })
}

/// Writes a session's transcript to `output`, or standard output.
pub fn export(
    session_id: String,
    format: &str,
    no_subagents: bool,
    output: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    let format = match format {
        "md" | "markdown" => ExportFormat::Markdown,
        "json" => ExportFormat::Json,
        other => anyhow::bail!("unknown format {other:?}: use md or json"),
    };
    block_on(async {
        let (mut client, _) = connect().await?;
        let exported = client
            .call::<SessionExport>(SessionExportParams {
                session_id,
                format,
                include_subagents: no_subagents.then_some(false),
            })
            .await?;
        if let Some(warning) = &exported.warning {
            eprintln!("warning: {warning}");
        }
        match output {
            Some(path) => {
                std::fs::write(&path, exported.content)?;
                eprintln!("wrote {}", path.display());
            }
            None => print!("{}", exported.content),
        }
        Ok(())
    })
}
