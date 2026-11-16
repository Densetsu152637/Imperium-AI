//! Branching a conversation: where a session's root conversation stood after each turn, and
//! how much of a journal and conversation a branch from an earlier message keeps.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use imperium_protocol::session::{AgentEvent, ROOT_AGENT, SessionEvent};
use serde::{Deserialize, Serialize};

use crate::journal::{messages_path, read_events};

/// Where a session records how long its conversation was after each root turn, beside its
/// journal, so a branch can take the conversation as it was at an earlier message.
pub fn turns_path(journal: &Path) -> PathBuf {
    journal.with_extension("turns.jsonl")
}

/// One line of the turn index: after `events` events had been journalled, the root
/// conversation held `messages` messages. `reset` marks a compaction, which rewrites the
/// conversation, so earlier lines no longer describe it.
#[derive(Debug, Serialize, Deserialize)]
struct TurnMark {
    events: usize,
    messages: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reset: bool,
}

/// Appends a line to the turn index. A failure is ignored: branching then reports the point as
/// unavailable.
pub fn mark_turn(journal: &Path, events: usize, messages: usize, reset: bool) {
    let mark = TurnMark {
        events,
        messages,
        reset,
    };
    let Ok(mut line) = serde_json::to_vec(&mark) else {
        return;
    };
    line.push(b'\n');
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(turns_path(journal))
    {
        let _ = file.write_all(&line);
    }
}

/// Where a branch from an event of a session's journal begins.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchPlan {
    /// How many of the journal's first events the branch keeps.
    pub events: usize,
    /// When the event is a user message: that message, which the branch leaves out so it can
    /// be edited and sent again.
    pub prompt: Option<String>,
    /// The root conversation at that point, as the JSON array a session saves.
    pub messages: Vec<serde_json::Value>,
}

/// Works out the branch from event `at` of the session whose journal is at `journal`. The turn
/// containing `at` is kept whole, unless `at` is the event that started it (a user message): that
/// turn is then left out and its text returned. Errors say why a point cannot be branched from:
/// its turn is still running, the conversation was compacted since, or the session predates the
/// turn index.
pub fn plan_branch(journal: &Path, at: usize) -> Result<BranchPlan, String> {
    let events = read_events(journal);
    if at >= events.len() {
        return Err(format!("the session has no event {at}"));
    }
    let is_root = |event: &AgentEvent| event.agent_id == ROOT_AGENT;
    let starts: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            is_root(event) && matches!(event.event, SessionEvent::TurnStarted { .. })
        })
        .map(|(index, _)| index)
        .collect();
    let turn = starts
        .iter()
        .rposition(|start| *start <= at)
        .ok_or("there is no message at or before that point")?;
    let (keep, prompt) = if starts[turn] == at {
        let prompt = match &events[at].event {
            SessionEvent::TurnStarted { text, .. } => text.clone(),
            _ => String::new(),
        };
        (at, Some(prompt))
    } else {
        let end = starts.get(turn + 1).copied().unwrap_or(events.len());
        let finished = events[starts[turn]..end].iter().any(|event| {
            is_root(event)
                && matches!(
                    event.event,
                    SessionEvent::TurnCompleted { .. } | SessionEvent::TurnFailed { .. }
                )
        });
        if !finished {
            return Err("that message's turn is still running; branch once it has ended".into());
        }
        (end, None)
    };
    let mut messages: Vec<serde_json::Value> = std::fs::read_to_string(messages_path(journal))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    let wanted = match std::fs::read_to_string(turns_path(journal)) {
        Ok(text) => {
            let marks: Vec<TurnMark> = text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            // Marks before the last compaction describe a conversation that no longer exists.
            let current = marks.iter().rposition(|mark| mark.reset);
            let usable = &marks[current.unwrap_or(0)..];
            if let Some(first) = usable.first()
                && first.reset
                && first.events > keep
            {
                return Err(
                    "the conversation was compacted after that message, so it cannot \
                            be branched from there any more"
                        .into(),
                );
            }
            usable
                .iter()
                .take_while(|mark| mark.events <= keep)
                .last()
                .map_or(0, |mark| mark.messages)
        }
        // Sessions from before the turn index: only their latest point is known.
        Err(_) if keep == events.len() => messages.len(),
        Err(_) => return Err("this session predates branching from earlier messages".into()),
    };
    messages.truncate(wanted);
    Ok(BranchPlan {
        events: keep,
        prompt,
        messages,
    })
}

/// The branch from a session's current point: everything so far. Fails while a root turn is
/// running; a session with no turns yet branches into an empty conversation.
pub fn plan_branch_at_end(journal: &Path) -> Result<BranchPlan, String> {
    let events = read_events(journal);
    let has_turn = events.iter().any(|event| {
        event.agent_id == ROOT_AGENT && matches!(event.event, SessionEvent::TurnStarted { .. })
    });
    if !has_turn {
        return Ok(BranchPlan {
            events: events.len(),
            prompt: None,
            messages: Vec::new(),
        });
    }
    let plan = plan_branch(journal, events.len() - 1)?;
    if plan.prompt.is_some() {
        return Err("a turn is running; wait for it to finish or stop it first".into());
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use imperium_protocol::session::TokenUsage;

    fn root(event: SessionEvent) -> AgentEvent {
        AgentEvent {
            agent_id: ROOT_AGENT.into(),
            event,
        }
    }

    fn started(text: &str) -> AgentEvent {
        root(SessionEvent::TurnStarted {
            text: text.into(),
            attachments: Vec::new(),
        })
    }

    fn completed() -> AgentEvent {
        root(SessionEvent::TurnCompleted {
            usage: TokenUsage::default(),
            cost_usd: None,
            steps: 1,
        })
    }

    #[test]
    fn a_branch_keeps_whole_turns_and_the_conversation_at_that_point() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("s.jsonl");
        let mut file = Journal::open(&journal).unwrap();
        // Two turns of two conversation messages each, after one system message.
        let log = [
            started("one"),
            completed(),
            started("two"),
            completed(),
            started("three"),
        ];
        let messages: Vec<serde_json::Value> =
            (0..6).map(|n| serde_json::json!({"n": n})).collect();
        for (index, event) in log.iter().enumerate() {
            file.record(event).unwrap();
            if index == 1 {
                mark_turn(&journal, 2, 3, false);
            }
            if index == 3 {
                mark_turn(&journal, 4, 5, false);
            }
        }
        std::fs::write(
            messages_path(&journal),
            serde_json::to_vec(&messages).unwrap(),
        )
        .unwrap();

        // On a reply: the turn is kept whole.
        let plan = plan_branch(&journal, 1).unwrap();
        assert_eq!(
            (plan.events, plan.prompt, plan.messages.len()),
            (2, None, 3)
        );
        // On a user message: the turn is left out, its text returned.
        let plan = plan_branch(&journal, 2).unwrap();
        assert_eq!(plan.events, 2);
        assert_eq!(plan.prompt.as_deref(), Some("two"));
        assert_eq!(plan.messages.len(), 3);
        // The last turn started but has no end yet: its message can be branched at, its
        // (unfinished) reply cannot.
        assert_eq!(
            plan_branch(&journal, 4).unwrap().prompt.as_deref(),
            Some("three")
        );
        assert!(plan_branch(&journal, 9).is_err());
        // A compaction after the point hides it.
        mark_turn(&journal, 4, 2, true);
        assert!(plan_branch(&journal, 1).is_err());
        assert_eq!(plan_branch(&journal, 3).unwrap().messages.len(), 2);
    }
}
