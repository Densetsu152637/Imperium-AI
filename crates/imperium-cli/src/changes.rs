//! `imperium config changes` and `imperium config undo`: the changes agents made to the app
//! config, and undoing one.

use imperium_protocol::methods::{
    ConfigChange, ConfigChanges, ConfigChangesParams, ConfigUndo, ConfigUndoParams,
};

use crate::{block_on, connect};

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listed = client.call::<ConfigChanges>(ConfigChangesParams {}).await?;
        if listed.changes.is_empty() {
            println!("No agent has changed the app config.");
        }
        for change in &listed.changes {
            print!("{}", describe(change));
        }
        Ok(())
    })
}

pub fn undo(id: u64) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let undone = client.call::<ConfigUndo>(ConfigUndoParams { id }).await?;
        println!("{}", undone.message);
        Ok(())
    })
}

/// A change as lines: id, time, summary and state, then its details.
fn describe(change: &ConfigChange) -> String {
    let when = chrono::DateTime::from_timestamp_millis(
        i64::try_from(change.at_unix_ms).unwrap_or(i64::MAX),
    )
    .map(|time| {
        time.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
    .unwrap_or_default();
    let mut text = format!("#{} {when} {}", change.id, change.summary);
    if change.undone_at_unix_ms.is_some() {
        text.push_str(" (undone)");
    }
    text.push('\n');
    if let Some(session) = &change.session_id {
        text.push_str(&format!("    by session {session}\n"));
    }
    if change.kind == "model_card" {
        text.push_str(&format!(
            "    before: {}\n    after: {}\n",
            one_line(change.before.as_deref()),
            one_line(change.after.as_deref())
        ));
    }
    if let Some(path) = &change.path {
        text.push_str(&format!("    folder: {path}\n"));
    }
    if let Some(source) = &change.source {
        let mut from = source.location.clone();
        if let Some(subdir) = &source.subdir {
            from.push_str(&format!(" (folder {subdir})"));
        }
        if let Some(revision) = &source.revision {
            from.push_str(&format!(" at {revision}"));
        }
        text.push_str(&format!("    from: {from}\n"));
    }
    if change.disabled {
        text.push_str("    installed turned off (it has hooks or MCP servers)\n");
    }
    text
}

fn one_line(card: Option<&str>) -> String {
    match card {
        Some(card) => {
            let flat = card.split_whitespace().collect::<Vec<_>>().join(" ");
            truncate(&flat, 100)
        }
        None => "(none)".into(),
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(limit).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imperium_protocol::methods::InstallSource;

    #[test]
    fn changes_print_their_source_and_cards() {
        let change = ConfigChange {
            id: 3,
            at_unix_ms: 0,
            session_id: Some("s1".into()),
            kind: "install".into(),
            summary: "Installed the skill pdf from https://example.com/x.git at abc".into(),
            model: None,
            model_kind: None,
            before: None,
            after: None,
            extension_kind: Some("skill".into()),
            name: Some("pdf".into()),
            path: Some("/config/skills/pdf".into()),
            source: Some(InstallSource {
                kind: "git".into(),
                location: "https://example.com/x.git".into(),
                revision: Some("abc123".into()),
                requested: None,
                subdir: Some("skills/pdf".into()),
                installed_at_unix_ms: 0,
                session_id: Some("s1".into()),
            }),
            disabled: false,
            kept_at: None,
            undone_at_unix_ms: Some(1),
        };
        let text = describe(&change);
        assert!(text.starts_with("#3 "), "{text}");
        assert!(text.contains("(undone)"), "{text}");
        assert!(
            text.contains("from: https://example.com/x.git (folder skills/pdf) at abc123"),
            "{text}"
        );
        let card = ConfigChange {
            kind: "model_card".into(),
            before: None,
            after: Some("Good at:\ncoding".into()),
            source: None,
            path: None,
            ..change
        };
        assert!(describe(&card).contains("after: Good at: coding"));
    }
}
