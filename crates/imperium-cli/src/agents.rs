//! `imperium agents list`: the named agents the daemon knows from the user's `agents/` folder and
//! plugins. A project's own agent files are loaded per session, for a trusted project, so they
//! are not listed here.

use imperium_protocol::methods::{McpList, McpListParams};

use crate::{block_on, connect};

/// Longest description shown in the list.
const DESCRIPTION_CHARS: usize = 100;

pub fn list() -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let listing = client.call::<McpList>(McpListParams {}).await?;
        if listing.agents.is_empty() {
            println!(
                "No named agents. Put a Markdown file with name and description in its front \
                 matter in the agents folder of your configuration or of a plugin."
            );
            return Ok(());
        }
        let width = listing
            .agents
            .iter()
            .map(|agent| agent.name.chars().count())
            .max()
            .unwrap_or(0);
        for agent in &listing.agents {
            let description = clean(
                agent
                    .description
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or_default(),
            );
            let first = description.as_str();
            let mut shown: String = first.chars().take(DESCRIPTION_CHARS).collect();
            if first.chars().count() > DESCRIPTION_CHARS {
                shown.push_str("...");
            }
            println!(
                "{:<width$}  {:<16}  {shown}",
                clean(&agent.name),
                clean(&agent.origin)
            );
        }
        Ok(())
    })
}

/// `text` without control characters: agent files are other people's text, and nothing in them
/// may drive the terminal.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn control_characters_are_stripped_from_what_is_printed() {
        assert_eq!(clean("a\x1b[31mred\x07\r\n\t\u{9b}x"), "a[31mredx");
        assert_eq!(
            clean("plain text, with accents: \u{e9}"),
            "plain text, with accents: \u{e9}"
        );
    }
}
