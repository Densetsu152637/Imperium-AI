//! `imperium env initiative`, `suggestions`, `start`, `dismiss` and `review`: what an
//! environment's persistent agent suggests, and how much initiative it takes.

use anyhow::bail;
use imperium_cli::client::Client;
use imperium_protocol::initiative::{
    Initiative, InitiativeDismiss, InitiativeGet, InitiativeRef, InitiativeReview, InitiativeStart,
    InitiativeState, InitiativeSuggestionRef, SuggestionStatus,
};
use imperium_protocol::methods::{
    EnvironmentList, EnvironmentListParams, EnvironmentUpdate, EnvironmentUpdateParams,
};

use crate::{block_on, connect};

async fn resolve(client: &mut Client, id_or_name: &str) -> anyhow::Result<String> {
    let listing = client
        .call::<EnvironmentList>(EnvironmentListParams {})
        .await?;
    if let Some(env) = listing.environments.iter().find(|env| env.id == id_or_name) {
        return Ok(env.id.clone());
    }
    let mut named: Vec<_> = listing
        .environments
        .iter()
        .filter(|env| env.name.eq_ignore_ascii_case(id_or_name.trim()))
        .collect();
    match named.len() {
        1 => Ok(named.remove(0).id.clone()),
        0 => bail!("no environment {id_or_name:?}; see imperium env list"),
        count => bail!("{count} environments are named {id_or_name:?}; use its id"),
    }
}

/// Sets how much initiative an environment's persistent agent takes, or shows it.
pub fn initiative(env: String, mode: Option<String>) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?;
        if let Some(mode) = mode {
            let mode = Initiative::parse(&mode)
                .ok_or_else(|| anyhow::anyhow!("initiative is off, suggest or act"))?;
            client
                .call::<EnvironmentUpdate>(EnvironmentUpdateParams {
                    id: id.clone(),
                    initiative: Some(mode),
                    ..EnvironmentUpdateParams::default()
                })
                .await?;
        }
        let state = client
            .call::<InitiativeGet>(InitiativeRef { environment_id: id })
            .await?;
        print!("{}", describe(&state));
        Ok(())
    })
}

/// Lists the agent's suggestions and its latest work-left reminder.
pub fn suggestions(env: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let id = resolve(&mut client, &env).await?;
        let state = client
            .call::<InitiativeGet>(InitiativeRef { environment_id: id })
            .await?;
        print!("{}", describe(&state));
        for suggestion in &state.suggestions {
            let status = match suggestion.status {
                SuggestionStatus::Open => "open",
                SuggestionStatus::Started => "started",
                SuggestionStatus::Dismissed => "dismissed",
            };
            println!(
                "  {}  [{status}] {}\n      prompt: {}\n      why: {} (from {})",
                suggestion.id,
                suggestion.title,
                suggestion.prompt,
                suggestion.reason,
                suggestion.source
            );
        }
        if let Some(digest) = &state.digest {
            println!("  Work left: {}", digest.summary);
            for item in &digest.items {
                println!("    - {} (from {})", item.text, item.source);
            }
        }
        Ok(())
    })
}

/// Starts a suggestion as a session.
pub fn start(env: String, suggestion: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?;
        let result = client
            .call::<InitiativeStart>(InitiativeSuggestionRef {
                environment_id,
                suggestion_id: suggestion,
            })
            .await?;
        println!("Started session {}", result.session_id);
        Ok(())
    })
}

/// Dismisses a suggestion.
pub fn dismiss(env: String, suggestion: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?;
        client
            .call::<InitiativeDismiss>(InitiativeSuggestionRef {
                environment_id,
                suggestion_id: suggestion,
            })
            .await?;
        println!("Dismissed.");
        Ok(())
    })
}

/// Asks the agent to review the environment now.
pub fn review(env: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let environment_id = resolve(&mut client, &env).await?;
        client
            .call::<InitiativeReview>(InitiativeRef { environment_id })
            .await?;
        println!("Reviewing. See the result with: imperium env suggestions {env}");
        Ok(())
    })
}

fn describe(state: &InitiativeState) -> String {
    let mut text = format!("initiative: {}\n", state.mode.as_str());
    if state.reviewing {
        text.push_str("  reviewing now\n");
    } else if let Some(waiting) = &state.waiting {
        text.push_str(&format!("  waiting: {waiting}\n"));
    }
    if let Some(error) = &state.last_error {
        text.push_str(&format!("  last review failed: {error}\n"));
    }
    if state.mode == Initiative::Act {
        text.push_str(&format!(
            "  started today: {} of {}; running: {} of {}\n",
            state.started_today, state.max_per_day, state.running, state.max_concurrent
        ));
    }
    let open = state
        .suggestions
        .iter()
        .filter(|suggestion| suggestion.status == SuggestionStatus::Open)
        .count();
    text.push_str(&format!("  open suggestions: {open}\n"));
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_is_described_with_its_limits_in_act_mode() {
        let state = InitiativeState {
            environment_id: "e".into(),
            mode: Initiative::Act,
            reviewing: false,
            waiting: Some("the machine is busy".into()),
            last_review_at: None,
            next_review_at: None,
            last_error: None,
            suggestions: vec![],
            digest: None,
            started_today: 1,
            max_per_day: 2,
            running: 0,
            max_concurrent: 1,
        };
        let text = describe(&state);
        assert!(text.contains("initiative: act"), "{text}");
        assert!(text.contains("waiting: the machine is busy"), "{text}");
        assert!(text.contains("started today: 1 of 2"), "{text}");
    }
}
