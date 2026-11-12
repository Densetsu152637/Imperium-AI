//! `imperium env agent initiative|suggestions|start|dismiss|review`: what a persistent agent
//! suggests, and where its initiative stands. The initiative itself is set with
//! `imperium env agent add --initiative` or `imperium env agent set --initiative`.

use imperium_protocol::initiative::{
    Initiative, InitiativeDismiss, InitiativeGet, InitiativeRef, InitiativeReview, InitiativeStart,
    InitiativeState, InitiativeSuggestionRef, SuggestionStatus,
};

use crate::env::{resolve, resolve_agent};
use crate::{block_on, connect};

/// The ids of the environment and agent a command names.
async fn target(
    client: &mut imperium_cli::client::Client,
    env: &str,
    agent: &str,
) -> anyhow::Result<(String, String)> {
    let environment_id = resolve(client, env).await?.id;
    let agent_id = resolve_agent(client, &environment_id, agent).await?;
    Ok((environment_id, agent_id))
}

/// Shows where an agent's initiative stands.
pub fn show(env: String, agent: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let (environment_id, agent_id) = target(&mut client, &env, &agent).await?;
        let state = client
            .call::<InitiativeGet>(InitiativeRef {
                environment_id,
                agent_id,
            })
            .await?;
        print!("{}", describe(&state));
        Ok(())
    })
}

/// Lists the agent's suggestions and its latest work-left reminder.
pub fn suggestions(env: String, agent: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let (environment_id, agent_id) = target(&mut client, &env, &agent).await?;
        let state = client
            .call::<InitiativeGet>(InitiativeRef {
                environment_id,
                agent_id,
            })
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
pub fn start(env: String, agent: String, suggestion: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let (environment_id, agent_id) = target(&mut client, &env, &agent).await?;
        let result = client
            .call::<InitiativeStart>(InitiativeSuggestionRef {
                environment_id,
                agent_id,
                suggestion_id: suggestion,
            })
            .await?;
        println!("Started session {}", result.session_id);
        Ok(())
    })
}

/// Dismisses a suggestion.
pub fn dismiss(env: String, agent: String, suggestion: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let (environment_id, agent_id) = target(&mut client, &env, &agent).await?;
        client
            .call::<InitiativeDismiss>(InitiativeSuggestionRef {
                environment_id,
                agent_id,
                suggestion_id: suggestion,
            })
            .await?;
        println!("Dismissed.");
        Ok(())
    })
}

/// Asks the agent to review its environment now.
pub fn review(env: String, agent: String) -> anyhow::Result<()> {
    block_on(async {
        let (mut client, _) = connect().await?;
        let (environment_id, agent_id) = target(&mut client, &env, &agent).await?;
        client
            .call::<InitiativeReview>(InitiativeRef {
                environment_id,
                agent_id,
            })
            .await?;
        println!("Reviewing. See the result with: imperium env agent suggestions {env} {agent}");
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
            agent_id: "a".into(),
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
